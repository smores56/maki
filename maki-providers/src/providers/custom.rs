use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use flume::Sender;
use serde_json::Value;

use crate::auth::{AuthSpec, BuildOptions};
use crate::builtin::{
    resolve_api_key_env, resolve_base_url, resolve_default_model, resolve_protocol,
};
use maki_config::providers::{Protocol, ProviderDef, ProvidersConfig};
use maki_storage::id::SessionRef;

use super::ResolvedAuth;
use super::openai::responses;
use super::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use crate::model::{FastPricing, Model, ModelEntry, ModelPricing, ModelTier};
use crate::provider::{BoxFuture, Provider};
use crate::providers::Timeouts;
use crate::registry::{self, ProviderSpec, TOML_OWNER};
use crate::types::ThinkingConfig;
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse};

static CUSTOM_OPENAI_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    // Custom providers resolve their own base URL (including any override) from
    // config, so the compat-layer fallback slug is unused here.
    slug: "",
    api_key_env: "",
    base_url: "",
    max_tokens_field: "max_tokens",
    include_stream_usage: true,
    provider_name: "custom",
};

/// Maps a custom provider's declared protocol to the builtin spec it inherits
/// its model catalog and fallbacks from. Returns `None` for a protocol with no
/// builtin base.
fn protocol_spec(protocol: Protocol) -> Option<Arc<ProviderSpec>> {
    let slug = match protocol {
        Protocol::Openai | Protocol::OpenaiResponses => "openai",
        Protocol::Anthropic => "anthropic",
        Protocol::Google => "google",
    };
    registry::get_no_extra(slug)
}

/// Builtins win their slug in `from_spec`/`create`, so every custom path skips
/// them. Keys off the registry (all 13 builtins), not `builtin_provider`, which
/// omits `openrouter`/`opencode` and would let them shadow the builtin.
fn is_builtin_slug(slug: &str) -> bool {
    registry::get_no_extra(slug).is_some()
}

fn resolve_custom_auth(slug: &str, def: &ProviderDef) -> Result<ResolvedAuth, AgentError> {
    let resolved_env = resolve_api_key_env(slug, Some(def));
    let env_var = def.api_key_env.as_deref().unwrap_or(&resolved_env);
    let pool = super::KeyPool::resolve(slug, env_var)?;

    let base_url = resolve_base_url(slug, Some(def));
    let mut auth = ResolvedAuth::bearer(pool.current());
    auth.base_url = base_url;
    Ok(auth)
}

pub fn create(slug: &str, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    let config = ProvidersConfig::load();
    let def = config.get(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown custom provider '{slug}'"),
    })?;
    let protocol = def.protocol.ok_or_else(|| AgentError::Config {
        message: format!("custom provider '{slug}' declares no protocol"),
    })?;
    let base = protocol_spec(protocol).ok_or_else(|| AgentError::Config {
        message: format!("custom provider '{slug}' base protocol has no builtin"),
    })?;
    let spec = Arc::new(spec_from_def(slug, def, &base));
    build_custom(&spec, BuildOptions::from_timeouts(timeouts))
}

/// Registry build fn for providers.toml-produced specs. Resolves custom auth
/// (running `KeyPool::resolve` when `opts.auth` is unset), then builds the
/// inner provider via the base builtin protocol.
fn build_custom(
    spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let config = ProvidersConfig::load();
    let slug = spec.slug.as_ref();
    let def = config.get(slug).ok_or_else(|| AgentError::Config {
        message: format!("unknown custom provider '{slug}'"),
    })?;
    let resolved = match opts.auth {
        Some(auth) => auth.lock().unwrap().clone(),
        None => resolve_custom_auth(slug, def)?,
    };
    let auth = Arc::new(Mutex::new(resolved));

    let protocol = def.protocol.unwrap_or(Protocol::Openai);
    let base_slug = match protocol {
        Protocol::Openai | Protocol::OpenaiResponses => "openai",
        Protocol::Anthropic => "anthropic",
        Protocol::Google => "google",
    };
    match base_slug {
        "anthropic" => Ok(Box::new(super::anthropic::Anthropic::with_auth(
            auth,
            opts.timeouts,
        ))),
        "openai" => Ok(Box::new(CustomOpenAiProvider {
            compat: OpenAiCompatProvider::new(&CUSTOM_OPENAI_CONFIG, opts.timeouts),
            auth,
            protocol,
        })),
        "google" => Ok(Box::new(super::google::Google::with_auth(
            auth,
            opts.timeouts,
        ))),
        other => Err(AgentError::Config {
            message: format!(
                "unsupported base '{other}' for custom provider '{slug}', only openai/anthropic/google are supported"
            ),
        }),
    }
}

/// Build a model from an already-loaded provider definition. Preserved for
/// future lookups that resolve a single model id without going through the
/// registry (e.g. ad-hoc `from_spec` overrides); the registry path now uses
/// `Model::from_base` against the registered spec.
#[allow(dead_code)]
fn model_from_def(def: &ProviderDef, base: &ProviderSpec, slug: &str, model_id: &str) -> Model {
    let declared = def.models.iter().find(|m| m.id == model_id);
    let tier = declared
        .map(|m| ModelTier::from(m.tier))
        .unwrap_or(ModelTier::Medium);
    let max_output_tokens = declared
        .and_then(|m| m.max_output_tokens)
        .or(base.fallback_max_output);
    let context_window = declared
        .and_then(|m| m.context_window)
        .unwrap_or(base.fallback_context_window);
    let supports_tool_examples_override = declared.and_then(|m| m.supports_tool_examples);
    let supports_thinking_override = declared
        .and_then(|m| m.supports_thinking)
        .or(Some(base.supports_thinking));
    let supports_vision_override = declared.and_then(|m| m.supports_vision);
    let pricing = declared
        .filter(|m| m.has_pricing())
        .map(|m| ModelPricing {
            input: m.pricing_input.unwrap_or(0.0),
            output: m.pricing_output.unwrap_or(0.0),
            cache_write: m.pricing_cache_write.unwrap_or(0.0),
            cache_read: m.pricing_cache_read.unwrap_or(0.0),
            fast: declared
                .filter(|d| d.has_fast_pricing())
                .map(|d| FastPricing {
                    input: d.pricing_fast_input.unwrap_or(0.0),
                    output: d.pricing_fast_output.unwrap_or(0.0),
                }),
        })
        .unwrap_or_default();
    Model {
        id: model_id.to_string(),
        provider: Arc::from(slug),
        tier,
        family: base.family,
        supports_tool_examples_override,
        supports_thinking_override,
        supports_vision_override,
        pricing,
        max_output_tokens,
        context_window,
    }
}

/// HTTP `/models` discovery for custom providers that set
/// `discover_models = true`. Not wired into `fetch_all_models` (which fans out
/// only over builtins per design §4.3); retained for an explicit `--models`
/// flow that may still call it directly.
#[allow(dead_code)]
pub fn discover_models(timeouts: Timeouts) -> Vec<String> {
    let config = ProvidersConfig::load();
    let mut all_specs = Vec::new();
    for slug in config.providers.keys() {
        if is_builtin_slug(slug) {
            continue;
        }
        let def = config.get(slug).unwrap();
        if !def.discover_models {
            continue;
        }
        if resolve_protocol(slug, Some(def)).is_none() {
            continue;
        }
        match create(slug, timeouts) {
            Ok(provider) => {
                let slug_c = slug.clone();
                let result = smol::block_on(provider.list_models());
                match result {
                    Ok(models) => {
                        for m in models {
                            all_specs.push(format!("{slug_c}/{}", m.id));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(slug, error = %e, "failed to list models for custom provider");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(slug, error = %e, "failed to create custom provider");
            }
        }
    }
    all_specs
}

/// Builds the model catalog for a custom spec: one [`ModelEntry`] per declared
/// model, the first at each tier marked `default` so tier-based selection
/// (`Model::from_tier`) resolves without the legacy `resolve_tier` path.
fn model_entries(def: &ProviderDef, base: &ProviderSpec) -> Vec<ModelEntry> {
    let mut seen_tiers: HashSet<ModelTier> = HashSet::new();
    def.models
        .iter()
        .map(|m| {
            let tier = ModelTier::from(m.tier);
            let default = seen_tiers.insert(tier);
            let declared = Some(m);
            let max_output_tokens = declared
                .and_then(|m| m.max_output_tokens)
                .or(base.fallback_max_output);
            let context_window = declared
                .and_then(|m| m.context_window)
                .unwrap_or(base.fallback_context_window);
            let pricing = declared
                .filter(|m| m.has_pricing())
                .map(|m| ModelPricing {
                    input: m.pricing_input.unwrap_or(0.0),
                    output: m.pricing_output.unwrap_or(0.0),
                    cache_write: m.pricing_cache_write.unwrap_or(0.0),
                    cache_read: m.pricing_cache_read.unwrap_or(0.0),
                    fast: declared
                        .filter(|d| d.has_fast_pricing())
                        .map(|d| FastPricing {
                            input: d.pricing_fast_input.unwrap_or(0.0),
                            output: d.pricing_fast_output.unwrap_or(0.0),
                        }),
                })
                .unwrap_or_default();
            ModelEntry {
                prefixes: vec![m.id.clone()],
                tier,
                family: base.family,
                vision: m.supports_vision.unwrap_or(false),
                default,
                pricing,
                max_output_tokens,
                context_window,
            }
        })
        .collect()
}

/// Constructs the [`ProviderSpec`] a providers.toml entry would register,
/// without writing the registry. Pure so `create` can reuse the construction
/// without re-reading config for the `build` fn lookup.
fn spec_from_def(slug: &str, def: &ProviderDef, base: &ProviderSpec) -> ProviderSpec {
    let env = resolve_api_key_env(slug, Some(def));
    ProviderSpec {
        slug: Arc::from(slug),
        owner: Some(Arc::from(TOML_OWNER)),
        display_name: crate::builtin::resolve_display_name(slug, Some(def)),
        family: base.family,
        features: base.features.clone(),
        protocol: base.protocol,
        base_url: resolve_base_url(slug, Some(def)).or_else(|| base.base_url.clone()),
        default_model: resolve_default_model(slug, Some(def))
            .or_else(|| base.default_model.clone()),
        auth: AuthSpec::ApiKey {
            env,
            login_url: base_auth_login_url(base),
            needs_url: base_auth_needs_url(base),
            plans: None,
        },
        supports_thinking: base.supports_thinking,
        accepts_arbitrary_models: base.accepts_arbitrary_models,
        fallback_max_output: base.fallback_max_output,
        fallback_context_window: base.fallback_context_window,
        capability_keys: base.capability_keys.clone(),
        models: model_entries(def, base),
        build: build_custom,
    }
}

fn base_auth_login_url(base: &ProviderSpec) -> Option<String> {
    match &base.auth {
        AuthSpec::ApiKey { login_url, .. } => login_url.clone(),
        AuthSpec::External(_) => None,
    }
}

fn base_auth_needs_url(base: &ProviderSpec) -> bool {
    match &base.auth {
        AuthSpec::ApiKey { needs_url, .. } => *needs_url,
        AuthSpec::External(_) => false,
    }
}

/// Registers every non-builtin providers.toml entry as a spec inheriting from
/// its protocol's builtin base. Idempotent via [`ensure_registered`]; idempotent
/// in `register` itself because a `Source::Toml` spec never replaces an existing
/// `Toml` spec (ties are dropped with a warning).
pub fn register_specs() {
    let config = ProvidersConfig::load();
    for (slug, def) in &config.providers {
        if is_builtin_slug(slug) {
            continue;
        }
        let Some(protocol) = resolve_protocol(slug, Some(def)) else {
            continue;
        };
        let Some(base) = protocol_spec(protocol) else {
            continue;
        };
        registry::register(spec_from_def(slug, def, &base));
    }
}

static REGISTERED: OnceLock<()> = OnceLock::new();

pub fn ensure_registered() {
    REGISTERED.get_or_init(register_specs);
}

struct CustomOpenAiProvider {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    protocol: Protocol,
}

impl Provider for CustomOpenAiProvider {
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
            let auth = self.auth.lock().unwrap().clone();

            if self.protocol == Protocol::OpenaiResponses {
                let body = responses::build_body(model, messages, system, tools);
                // TODO: wire thinking budget into responses API when llama.cpp supports it
                return responses::do_stream(
                    self.compat.client(),
                    model,
                    &body,
                    event_tx,
                    &auth,
                    self.compat.stream_timeout(),
                )
                .await;
            }

            let mut body = self.compat.build_body(model, messages, system, tools);
            if matches!(opts.thinking, ThinkingConfig::Off) {
                body["thinking"] = serde_json::json!({"type": "disabled"});
            }
            self.compat
                .do_stream(model, &[], &body, event_tx, &auth)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<crate::model::ModelInfo>, AgentError>> {
        let auth = self.auth.lock().unwrap().clone();
        Box::pin(async move { self.compat.do_list_models(&auth).await })
    }
}
