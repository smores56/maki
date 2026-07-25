//! Provider manifests loaded by the Lua host.
//!
//! A manifest wires a slug (e.g. `"deepseek"`) to an OpenAI-compatible engine,
//! an auth function-table, and a static model list. The `providers` builtin
//! plugin requires one module per provider (e.g. `plugins/providers/deepseek.lua`),
//! and each calls `maki.provider.register` as a side-effect of plugin load; `register`
//! pulls the `resolve`/`rotate`/`refresh` `mlua::Function`s out of `opts.auth`
//! (so they stay first-class and don't go through serde), nulls the field,
//! deserializes the rest of `opts` into a [`ManifestDescriptor`], and hands the
//! engine spec, the functions (wrapped in a [`LuaAuthSource`]), the model list,
//! and a leaked `&'static ProviderManifest` to [`register_manifest_provider`].
//!
//! `provider_for_slug` then short-circuits through [`has_manifest_provider`]
//! before reaching the `ProviderKind` fallback, so a Lua-registered manifest
//! wins over the static built-in enum. Capability lookups
//! (`ManifestRegistry::get`/`for_slug`) resolve the same owned manifest, so no
//! provider's display name, family, or fallback windows live in two places.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use flume::Sender;
use maki_storage::id::SessionRef;
use mlua::{Function, Value as LuaValue};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::manifest::ManifestRegistry;
use crate::manifest::ProviderManifest;
use crate::model::{Model, ModelEntry, ModelFamily, ModelInfo, ModelPricing, ModelTier};
use crate::model_registry::model_registry;
use crate::provider::{BoxFuture, Provider};
use crate::providers::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use crate::providers::{ResolvedAuth, Timeouts, with_prefix};
use crate::types::{ProviderUsage, ThinkingConfig, dialect};
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse};

const V4_MARKER: &str = "deepseek-v4";

/// Authoritative description of a manifest provider's engine. Built by the
/// Lua loader from a `maki.provider.openai_compat{...}` descriptor.
#[derive(Debug, Clone)]
pub enum EngineSpec {
    OpenaiCompat {
        slug: String,
        base_url: String,
        api_key_env: String,
        max_tokens_field: String,
        include_stream_usage: bool,
        provider_name: String,
        thinking: Option<ThinkingMode>,
        /// Balance/quota endpoint fetched via `OpenAiCompatProvider::get_text`;
        /// paired with a manifest-level `usage` parse callback. `None` disables
        /// usage reporting for this engine.
        usage_url: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    /// DeepSeek's `{"type":"enabled"|"disabled"}` toggle, `dialect::DEEPSEEK`
    /// reasoning effort, and V4 `reasoning_content` padding.
    DeepSeek,
}

/// Serde-friendly shape of the table a Lua manifest returns (minus its `auth`
/// field, which is a function-table the loader extracts as `mlua::Function`s
/// before this deserialization). `ModelInfo` itself is not `Deserialize` (it
/// carries `provider_info`), so models round-trip through `ModelInfoDescriptor`.
#[derive(Deserialize)]
pub struct ManifestDescriptor {
    pub slug: String,
    pub display_name: String,
    pub engine: EngineDescriptor,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub supports_thinking: Option<bool>,
    #[serde(default)]
    pub accepts_arbitrary_models: Option<bool>,
    #[serde(default)]
    pub fallback_max_output: Option<u32>,
    #[serde(default)]
    pub fallback_context_window: Option<u32>,
    #[serde(default)]
    pub models: Vec<ModelInfoDescriptor>,
    #[serde(default)]
    pub qualities: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineDescriptor {
    OpenaiCompat {
        base_url: String,
        api_key_env: String,
        max_tokens_field: String,
        #[serde(default)]
        include_stream_usage: bool,
        provider_name: String,
        #[serde(default)]
        thinking: Option<String>,
        #[serde(default)]
        usage_url: Option<String>,
    },
}

#[derive(Deserialize)]
pub struct ModelInfoDescriptor {
    pub id: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub default: Option<bool>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
    #[serde(default)]
    pub supports_thinking: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
}

impl ModelInfoDescriptor {
    fn into_model_info(self) -> ModelInfo {
        ModelInfo {
            id: self.id,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            pricing: self.pricing,
            supports_thinking: self.supports_thinking,
            supports_vision: self.supports_vision,
            provider_info: None,
        }
    }
}

/// Parse a provider's raw usage/balance response body into a [`ProviderUsage`].
/// Implemented by the `maki-lua` loader for a manifest's `usage` callback: Rust
/// fetches the `EngineSpec::usage_url` and hands the response body string here,
/// so the hook performs pure parsing (no I/O) and may call back into Lua via the
/// captured `Lua` handle (the host is idle except for this call).
pub trait UsageParseHook: Send + Sync {
    fn parse_usage(&self, body: String) -> BoxFuture<'static, Result<ProviderUsage, AgentError>>;
}

pub struct ManifestCompat {
    compat: OpenAiCompatProvider,
    auth_handle: Arc<Mutex<ResolvedAuth>>,
    thinking: Option<ThinkingMode>,
    usage_url: Option<String>,
    parse_usage: Option<Arc<dyn UsageParseHook>>,
    system_prefix: Option<String>,
}

impl ManifestCompat {
    pub fn new(
        engine_spec: &EngineSpec,
        auth_handle: Arc<Mutex<ResolvedAuth>>,
        parse_usage: Option<Arc<dyn UsageParseHook>>,
        timeouts: Timeouts,
        system_prefix: Option<String>,
    ) -> Self {
        let (thinking, config, usage_url) = build_openai_compat_config(engine_spec);
        Self {
            compat: OpenAiCompatProvider::new(config, timeouts),
            auth_handle,
            thinking,
            usage_url,
            parse_usage,
            system_prefix,
        }
    }

    pub(crate) fn auth_handle(&self) -> &Arc<Mutex<ResolvedAuth>> {
        &self.auth_handle
    }
}

impl Provider for ManifestCompat {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let auth = self.auth_handle.lock().unwrap().clone();
            let mut buf = String::new();
            let system = with_prefix(&self.system_prefix, system, &mut buf);
            let mut body = self.compat.build_body(model, messages, system, tools);
            apply_thinking(&mut body, opts.thinking, model, self.thinking);
            self.compat
                .do_stream(model, &[], &body, event_tx, &auth)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async move {
            let auth = self.auth_handle.lock().unwrap().clone();
            self.compat.do_list_models(&auth).await
        })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async move {
            let Some(url) = self.usage_url.as_deref() else {
                return Ok(None);
            };
            let Some(parse) = self.parse_usage.as_ref() else {
                return Ok(None);
            };
            let auth = self.auth_handle.lock().unwrap().clone();
            let body = self.compat.get_text(&auth, url).await?;
            Ok(Some(parse.parse_usage(body).await?))
        })
    }
}

pub struct ManifestProvider {
    engine: ManifestCompat,
    auth: Arc<LuaAuthSource>,
}

impl ManifestProvider {
    /// Eager auth resolution: a missing key fails here. Used by
    /// `provider_for_slug` for env-key providers loaded from a manifest.
    pub fn new(
        engine_spec: EngineSpec,
        auth: Arc<LuaAuthSource>,
        parse_usage: Option<Arc<dyn UsageParseHook>>,
        timeouts: Timeouts,
    ) -> Result<Self, AgentError> {
        let auth_handle = Arc::new(Mutex::new(ResolvedAuth::bearer("")));
        auth.resolve(&auth_handle)?;
        let engine = ManifestCompat::new(&engine_spec, auth_handle, parse_usage, timeouts, None);
        Ok(Self { engine, auth })
    }
}

impl Provider for ManifestProvider {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        _session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        self.engine
            .stream_message(model, messages, system, tools, event_tx, opts, _session_id)
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        self.engine.list_models()
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        self.engine.fetch_usage()
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        let auth = Arc::clone(&self.auth);
        let handle = Arc::clone(self.engine.auth_handle());
        Box::pin(async move { auth.refresh(&handle).await })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        let auth = Arc::clone(&self.auth);
        let handle = Arc::clone(self.engine.auth_handle());
        Box::pin(async move { auth.reload(&handle) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        let auth = Arc::clone(&self.auth);
        let handle = Arc::clone(self.engine.auth_handle());
        Box::pin(async move { auth.rotate_key(&handle) })
    }
}

fn apply_thinking(
    body: &mut Value,
    thinking: ThinkingConfig,
    model: &Model,
    mode: Option<ThinkingMode>,
) {
    match mode {
        None => {
            if matches!(thinking, ThinkingConfig::Off) {
                body["thinking"] = json!({"type": "disabled"});
            }
        }
        Some(ThinkingMode::DeepSeek) => {
            if thinking.is_enabled() {
                body["thinking"] = json!({"type": "enabled"});
                thinking.apply_reasoning_effort(body, &dialect::DEEPSEEK, model);
                if matches!(thinking, ThinkingConfig::Budget(_)) {
                    warn!("DeepSeek reasoning does not support token budgets");
                }
                pad_reasoning_content(&model.id, body);
            } else {
                body["thinking"] = json!({"type": "disabled"});
            }
        }
    }
}

/// DeepSeek's two reasoning models disagree about `reasoning_content`: V4 in
/// thinking mode wants it on every assistant turn (missing = 400), R1 refuses
/// it as input. So we gate on the V4 substring, same trick Vercel's AI SDK
/// uses, and back-fill the turns that have none (plain replies, tool-only
/// turns). The API only checks the field exists, so `""` is enough.
///
/// Ref: <https://api-docs.deepseek.com/guides/thinking_mode>
fn pad_reasoning_content(model_id: &str, body: &mut Value) {
    if !model_id.contains(V4_MARKER) {
        return;
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) != Some("assistant")
            || msg
                .get("reasoning_content")
                .and_then(Value::as_str)
                .is_some()
        {
            continue;
        }
        msg["reasoning_content"] = Value::String(String::new());
    }
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn build_openai_compat_config(
    spec: &EngineSpec,
) -> (
    Option<ThinkingMode>,
    Arc<OpenAiCompatConfig>,
    Option<String>,
) {
    match spec {
        EngineSpec::OpenaiCompat {
            slug,
            base_url,
            api_key_env,
            max_tokens_field,
            include_stream_usage,
            provider_name,
            thinking,
            usage_url,
        } => {
            let config = OpenAiCompatConfig {
                slug: Arc::from(slug.as_str()),
                api_key_env: Arc::from(api_key_env.as_str()),
                base_url: Arc::from(base_url.as_str()),
                max_tokens_field: Arc::from(max_tokens_field.as_str()),
                include_stream_usage: *include_stream_usage,
                provider_name: Arc::from(provider_name.as_str()),
            };
            (*thinking, Arc::new(config), usage_url.clone())
        }
    }
}

/// Auth source backed by Lua function-tables pulled from a manifest's `auth`
/// field (`resolve`, optional `rotate`/`refresh`). Built by the `maki-lua`
/// loader; `maki.auth.env_key` (and future `maki.auth.*`) produce the tables.
///
/// `resolve`/`rotate`/`reload` call into Lua synchronously from whatever
/// thread constructs the `ManifestProvider`. Under mlua's `send` feature a
/// synchronous `Function::call` from a non-owner thread serializes on the
/// global reentrant Lua mutex — the runtime thread is idle post-boot, so the
/// only hazard is a sync call overlapping an in-flight async Lua call. Today
/// no async refresh ships (env_key has none), so the overlap is empty; the
/// first real async refresh lands in the oauth follow-up PR.
pub struct LuaAuthSource {
    slug: String,
    resolve: Function,
    rotate: Option<Function>,
    refresh: Option<Function>,
    login_url: Option<String>,
    needs_url: bool,
}

impl LuaAuthSource {
    pub fn new(
        slug: String,
        resolve: Function,
        rotate: Option<Function>,
        refresh: Option<Function>,
        login_url: Option<String>,
        needs_url: bool,
    ) -> Self {
        Self {
            slug,
            resolve,
            rotate,
            refresh,
            login_url,
            needs_url,
        }
    }

    fn config_err(&self, op: &str, e: mlua::Error) -> AgentError {
        AgentError::Config {
            message: format!("{} auth {op}: {e}", self.slug),
        }
    }

    fn resolve(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        let key = self
            .resolve
            .call::<String>(LuaValue::Nil)
            .map_err(|e| self.config_err("resolve", e))?;
        *auth.lock().unwrap() = ResolvedAuth::bearer(&key);
        Ok(())
    }

    fn reload(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        self.resolve(auth)
    }

    fn rotate_key(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<bool, AgentError> {
        let Some(rotate) = &self.rotate else {
            return Ok(false);
        };
        let next = rotate
            .call::<Option<String>>(LuaValue::Nil)
            .map_err(|e| self.config_err("rotate", e))?;
        match next {
            Some(key) => {
                *auth.lock().unwrap() = ResolvedAuth::bearer(&key);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn refresh(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> BoxFuture<'_, Result<(), AgentError>> {
        let auth = Arc::clone(auth);
        Box::pin(async move {
            let Some(refresh) = &self.refresh else {
                return Ok(());
            };
            // `resolve` may later return a table for multi-header flows (the
            // copilot follow-up PR); for env_key a bearer string suffices.
            let next = refresh
                .call_async::<Option<String>>(LuaValue::Nil)
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("{} auth refresh: {e}", self.slug),
                })?;
            if let Some(key) = next {
                *auth.lock().unwrap() = ResolvedAuth::bearer(&key);
            }
            Ok(())
        })
    }
}

type Registry = HashMap<
    Arc<str>,
    (
        EngineSpec,
        Arc<LuaAuthSource>,
        Option<Arc<dyn UsageParseHook>>,
    ),
>;

static MANIFEST_PROVIDERS: OnceLock<Mutex<Registry>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    MANIFEST_PROVIDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a Lua-loaded provider: store its engine spec, already-built auth
/// source, and static models for `provider_for_slug` routing and model picking,
/// AND own its capability manifest so `ManifestRegistry::get` resolves the
/// slug at runtime. All `'static` data is leaked once at boot.
pub fn register_manifest_provider(
    slug: Arc<str>,
    engine_spec: EngineSpec,
    auth: Arc<LuaAuthSource>,
    parse_usage: Option<Arc<dyn UsageParseHook>>,
    models: Vec<ModelInfo>,
    manifest: ProviderManifest,
) {
    registry()
        .lock()
        .unwrap()
        .insert(Arc::clone(&slug), (engine_spec, auth, parse_usage));
    model_registry()
        .write()
        .unwrap()
        .set_known_models(&slug, models);
    ManifestRegistry::register_owned_manifest(manifest);
}

fn family_from_str(s: Option<&str>) -> ModelFamily {
    match s {
        Some("claude") => ModelFamily::Claude,
        Some("gpt") => ModelFamily::Gpt,
        Some("gemini") => ModelFamily::Gemini,
        Some("glm") => ModelFamily::Glm,
        Some("synthetic") => ModelFamily::Synthetic,
        _ => ModelFamily::Generic,
    }
}

fn thinking_mode_from_str(s: &str) -> Option<ThinkingMode> {
    match s {
        "deepseek" => Some(ThinkingMode::DeepSeek),
        _ => None,
    }
}

impl ManifestDescriptor {
    /// Split a decoded manifest into everything the registry needs: the
    /// `(slug, engine, models)` parts plus the capability `ProviderManifest`
    /// (single-sourced here, leaked to `'static` so `ManifestRegistry` can own
    /// it without a lifetime parameter). `auth` is not part of this: the loader
    /// already pulled it out as `mlua::Function`s and built a `LuaAuthSource`.
    pub fn into_manifest_parts(self) -> (Arc<str>, EngineSpec, Vec<ModelInfo>, ProviderManifest) {
        let slug_arc: Arc<str> = Arc::from(self.slug.as_str());
        let display_name = Arc::from(self.display_name.as_str());
        let family = family_from_str(self.family.as_deref());
        let supports_thinking = self.supports_thinking.unwrap_or(false);
        let accepts_arbitrary_models = self.accepts_arbitrary_models.unwrap_or(false);
        let fallback_max_output = self.fallback_max_output;
        let fallback_context_window = self.fallback_context_window.unwrap_or(0);

        let engine_spec = match self.engine {
            EngineDescriptor::OpenaiCompat {
                base_url,
                api_key_env,
                max_tokens_field,
                include_stream_usage,
                provider_name,
                thinking,
                usage_url,
            } => EngineSpec::OpenaiCompat {
                slug: slug_arc.to_string(),
                base_url,
                api_key_env,
                max_tokens_field,
                include_stream_usage,
                provider_name,
                thinking: thinking.as_deref().and_then(thinking_mode_from_str),
                usage_url,
            },
        };

        let (entries, model_infos): (Vec<ModelEntry>, Vec<ModelInfo>) = self
            .models
            .into_iter()
            .map(|d| {
                let id = leak_str(&d.id);
                let entry = ModelEntry {
                    prefixes: Box::leak(Box::new([id])),
                    tier: d
                        .tier
                        .as_deref()
                        .and_then(|t| ModelTier::from_str(t).ok())
                        .unwrap_or(ModelTier::Medium),
                    family,
                    vision: d.supports_vision.unwrap_or(false),
                    default: d.default.unwrap_or(false),
                    pricing: d.pricing.clone().unwrap_or_default(),
                    max_output_tokens: d
                        .max_output_tokens
                        .or(fallback_max_output)
                        .unwrap_or_else(|| {
                            warn!(model = %d.id, "manifest model declares no max_output_tokens; defaulting to 0");
                            0
                        }),
                    context_window: d.context_window.unwrap_or(fallback_context_window),
                };
                (entry, d.into_model_info())
            })
            .unzip();

        let manifest = ProviderManifest {
            slug: Arc::clone(&slug_arc),
            display_name,
            family,
            supports_thinking,
            accepts_arbitrary_models,
            fallback_max_output,
            fallback_context_window,
            models: Box::leak(entries.into_boxed_slice()),
            qualities: self.qualities.map(|vec| {
                vec.into_iter()
                    .map(|s| Arc::from(s.as_str()))
                    .collect::<Box<[Arc<str>]>>()
            }),
        };
        (slug_arc, engine_spec, model_infos, manifest)
    }
}

pub fn has_manifest_provider(slug: &str) -> bool {
    registry().lock().unwrap().contains_key(slug)
}

/// Login/onboarding metadata for a Lua-registered manifest provider, projecting
/// the engine spec (api_key_env/base_url), capability manifest (display_name,
/// default model via the default-true medium-tier entry), and auth source
/// (login_url/needs_url) into the same `LoginInfo` shape builtins use. The auth
/// surface (`maki auth login`/`status`/picker) reads this; callers must have
/// booted the Lua host so the registry is populated.
pub fn login_info(slug: &str) -> Option<maki_config::providers::LoginInfo> {
    let guard = registry().lock().unwrap();
    let (spec, auth, _) = guard.get(slug)?;
    let EngineSpec::OpenaiCompat {
        api_key_env,
        base_url,
        ..
    } = spec;
    let manifest = ManifestRegistry::get(slug)?;
    let default_model =
        ManifestRegistry::find_default_for_tier(slug, crate::model::ModelTier::Medium)
            .and_then(|entry| entry.prefixes.first())
            .map(|prefix| format!("{slug}/{prefix}"));
    Some(maki_config::providers::LoginInfo {
        slug: manifest.slug.to_string(),
        display_name: manifest.display_name.to_string(),
        api_key_env: api_key_env.clone(),
        base_url: Some(base_url.clone()),
        default_model,
        login_url: auth.login_url.clone(),
        needs_url: auth.needs_url,
        plans: None,
    })
}

/// `login_info` for every registered manifest provider, for the auth picker /
/// `maki auth status` listing. Order follows registry insertion (stable per
/// boot); `BuiltInProvider` entries are excluded here — the caller merges them.
pub fn login_infos() -> Vec<maki_config::providers::LoginInfo> {
    let slugs: Vec<Arc<str>> = registry().lock().unwrap().keys().cloned().collect();
    slugs.iter().filter_map(|slug| login_info(slug)).collect()
}

/// Engine spec for a Lua-registered manifest provider, looked up by slug.
/// `EngineSpec` is the authoritative source of `api_key_env` / `base_url`
/// (the `ProviderManifest` capability struct carries neither), so docgen and
/// status surfaces query it here for env/url rendering.
pub fn engine_spec(slug: &str) -> Option<EngineSpec> {
    registry()
        .lock()
        .unwrap()
        .get(slug)
        .map(|(spec, _, _)| spec.clone())
}

/// Usage-parse callback for a Lua-registered manifest provider, looked up by
/// slug. `None` when the manifest declared no `usage` function; the `EngineSpec`
/// carries the matching `usage_url` for the fetch half. `provider_from_base`
/// (dynamic `Base::Manifest`) pulls both so a custom provider whose engine is
/// a manifest provider inherits its usage endpoint.
pub fn parse_usage(slug: &str) -> Option<Arc<dyn UsageParseHook>> {
    registry()
        .lock()
        .unwrap()
        .get(slug)
        .and_then(|(_, _, parse)| parse.clone())
}

pub(crate) fn manifest_provider(
    slug: &str,
    timeouts: Timeouts,
) -> Result<Box<dyn Provider>, AgentError> {
    let (engine_spec, auth, parse_usage) = registry()
        .lock()
        .unwrap()
        .get(slug)
        .cloned()
        .ok_or_else(|| AgentError::Config {
            message: format!("no manifest provider registered for '{slug}'"),
        })?;
    Ok(Box::new(ManifestProvider::new(
        engine_spec,
        auth,
        parse_usage,
        timeouts,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_case::test_case;

    const V4: &str = "deepseek-v4-pro";
    const R1: &str = "deepseek-reasoner";

    #[test_case(true  ; "thinking_enabled_pads")]
    #[test_case(false ; "thinking_disabled_no_pad")]
    fn deepseek_thinking_body_shape(enabled: bool) {
        let model = Model {
            id: V4.to_string(),
            provider: Arc::from("deepseek"),
            ..test_model()
        };
        let mut body = json!({"messages": [
            {"role": "assistant", "content": "", "tool_calls": [{"id": "c1"}]},
        ]});
        let thinking = if enabled {
            ThinkingConfig::Adaptive
        } else {
            ThinkingConfig::Off
        };
        apply_thinking(&mut body, thinking, &model, Some(ThinkingMode::DeepSeek));
        assert_eq!(
            body["thinking"]["type"],
            if enabled { "enabled" } else { "disabled" }
        );
        if enabled {
            assert_eq!(body["messages"][0]["reasoning_content"], "");
        } else {
            assert!(body["messages"][0].get("reasoning_content").is_none());
        }
    }

    #[test]
    fn none_mode_only_disables_when_off() {
        let model = Model {
            id: V4.to_string(),
            ..test_model()
        };
        let mut body = json!({"messages": []});
        apply_thinking(&mut body, ThinkingConfig::Adaptive, &model, None);
        assert!(body.get("thinking").is_none());

        let mut body = json!({"messages": []});
        apply_thinking(&mut body, ThinkingConfig::Off, &model, None);
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    fn test_model() -> Model {
        use crate::model::{ModelFamily, ModelPricing, ModelTier};
        Model {
            id: "deepseek-v4-pro".to_string(),
            provider: Arc::from("deepseek"),
            tier: ModelTier::Strong,
            family: ModelFamily::Generic,
            supports_tool_examples_override: None,
            supports_thinking_override: None,
            supports_vision_override: None,
            pricing: ModelPricing::ZERO,
            max_output_tokens: Some(384_000),
            context_window: 1_000_000,
        }
    }

    #[test]
    fn v4_pads_only_assistant_turns_without_reasoning() {
        let mut body = json!({"messages": [
            {"role": "system",    "content": "sys"},
            {"role": "user",      "content": "hi"},
            {"role": "assistant", "content": "ok", "reasoning_content": "kept"},
            {"role": "assistant", "content": "",   "tool_calls": [{"id": "c1"}]},
            {"role": "tool",      "tool_call_id": "c1", "content": "out"},
        ]});
        pad_reasoning_content(V4, &mut body);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[2]["reasoning_content"], "kept");
        assert_eq!(msgs[3]["reasoning_content"], "");
        for i in [0, 1, 4] {
            assert!(msgs[i].get("reasoning_content").is_none());
        }
    }

    #[test]
    fn non_v4_model_is_untouched() {
        let input = json!({"messages": [
            {"role": "assistant", "content": "", "tool_calls": [{"id": "c1"}]},
            {"role": "assistant", "content": "hi"},
        ]});
        let mut body = input.clone();
        pad_reasoning_content(R1, &mut body);
        assert_eq!(body, input);
    }

    #[test_case(Some("https://x/balance") ; "usage_url_threaded")]
    #[test_case(None                       ; "usage_url_defaults_none")]
    fn into_manifest_parts_threads_usage_url(usage_url: Option<&str>) {
        let mut engine = json!({
            "kind": "openai_compat",
            "base_url": "https://x",
            "api_key_env": "X",
            "max_tokens_field": "max_tokens",
            "provider_name": "X",
        });
        if let Some(u) = usage_url {
            engine["usage_url"] = json!(u);
        }
        let desc: ManifestDescriptor = serde_json::from_value(json!({
            "slug": "test-url-p",
            "display_name": "Test",
            "engine": engine,
            "models": [],
        }))
        .unwrap();
        let (_, engine_spec, _, _) = desc.into_manifest_parts();
        let EngineSpec::OpenaiCompat {
            usage_url: spec_url,
            ..
        } = engine_spec;
        assert_eq!(spec_url.as_deref(), usage_url);
    }
}
