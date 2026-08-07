use std::sync::{Arc, Mutex};

use flume::Sender;
use maki_storage::id::SessionRef;
use serde_json::Value;

use super::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use super::{KeyPool, ResolvedAuth, with_prefix};
use crate::auth::{AuthSpec, BuildOptions};
use crate::hooks::RequestCtx;
use crate::model::{Model, ModelInfo};
use crate::provider::{BoxFuture, Provider};
use crate::registry::ProviderSpec;
use crate::types::{ProviderUsage, RequestOptions, StreamResponse, ThinkingConfig};
use crate::{AgentError, Message, ProviderEvent};

static LUA_OPENAI_CONFIG: OpenAiCompatConfig = OpenAiCompatConfig {
    slug: "",
    api_key_env: "",
    base_url: "",
    max_tokens_field: "max_tokens",
    include_stream_usage: true,
    provider_name: "lua",
};

const EXTERNAL_AUTH_UNRESOLVABLE: &str = "external auth cannot be resolved at build time";

/// Registry build fn for Lua-registered openai-codec providers. Resolves
/// api-key auth via [`KeyPool::resolve`] when `opts.auth` is unset; external
/// auth must arrive pre-resolved on `opts.auth` from the script adapter, since
/// `build` is sync.
pub fn build_lua_openai(
    spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let resolved = match opts.auth {
        Some(auth) => auth.lock().unwrap().clone(),
        None => resolve_lua_auth(spec)?,
    };
    let auth = Arc::new(Mutex::new(resolved));
    Ok(Box::new(LuaOpenAiProvider {
        compat: OpenAiCompatProvider::new(&LUA_OPENAI_CONFIG, opts.timeouts),
        auth,
        spec: Arc::clone(spec),
        system_prefix: opts.system_prefix,
    }))
}

pub fn resolve_lua_auth(spec: &ProviderSpec) -> Result<ResolvedAuth, AgentError> {
    let env = match &spec.auth {
        AuthSpec::ApiKey { env, .. } => env,
        AuthSpec::External(_) => {
            return Err(AgentError::Config {
                message: EXTERNAL_AUTH_UNRESOLVABLE.into(),
            });
        }
    };
    let pool = KeyPool::resolve(&spec.slug, env)?;
    let mut auth = ResolvedAuth::bearer(pool.current());
    auth.base_url = spec.base_url.clone();
    Ok(auth)
}

struct LuaOpenAiProvider {
    compat: OpenAiCompatProvider,
    auth: Arc<Mutex<ResolvedAuth>>,
    spec: Arc<ProviderSpec>,
    system_prefix: Option<String>,
}

fn build_ctx(
    model: &Model,
    thinking: ThinkingConfig,
    session_id: Option<String>,
    auth_headers: Vec<(String, String)>,
    base_url: Option<String>,
) -> RequestCtx {
    RequestCtx {
        model_id: model.id.clone(),
        model_provider: Arc::clone(&model.provider),
        model_tier: model.tier,
        model_family: model.family,
        model_max_output_tokens: model.max_output_tokens,
        model_context_window: model.context_window,
        model_supports_thinking: model.supports_thinking(),
        model_supports_vision: model.supports_vision(),
        thinking,
        session_id,
        capabilities: serde_json::Map::new(),
        auth_headers,
        base_url,
    }
}

/// Run the Full request-prep pipeline the Lua provider uses: build the
/// openai-compat body, hand it (and the extra-headers vec) to the spec's
/// `on_request` hook behind shared handles, then apply the declared effort
/// dialect. Returns the serialised body plus any headers the hook emitted.
/// Shared with [`build_lua_body`] so golden tests assert exactly what the
/// codec sends, hook-for-hook.
#[allow(clippy::too_many_arguments)]
async fn prep_request(
    compat: &OpenAiCompatProvider,
    spec: &ProviderSpec,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    thinking: ThinkingConfig,
    session_id: Option<String>,
    auth_headers: Vec<(String, String)>,
) -> Result<(Value, Vec<(String, String)>), AgentError> {
    let body = compat.build_body(model, messages, system, tools);
    let body_arc = Arc::new(Mutex::new(body));
    let headers_arc: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let ctx = build_ctx(
        model,
        thinking,
        session_id,
        auth_headers,
        spec.base_url.clone(),
    );
    if let Some(h) = &spec.hooks
        && let Some(fut) = h.on_request(Arc::clone(&body_arc), Arc::clone(&headers_arc), ctx)
    {
        fut.await?;
    }
    let mut body = std::mem::replace(&mut *body_arc.lock().unwrap(), Value::Null);
    // The codec owns `reasoning_effort`: a declared effort dialect applies it
    // from the requested `ThinkingConfig` (§5.2), so the Lua `on_request`
    // never writes the field and the wire body stays order-stable.
    if thinking.is_enabled()
        && let Some(effort) = &spec.effort
    {
        thinking.apply_reasoning_effort(&mut body, &effort.as_dialect(), model);
    }
    let headers = headers_arc.lock().unwrap().clone();
    Ok((body, headers))
}

/// Build the request body a Lua-registered openai-codec provider would send,
/// without resolving auth or opening a socket. Golden tests assert the parsed
/// `serde_json::Value` of this against the Rust implementation's wire shape.
pub async fn build_lua_body(
    spec: &Arc<ProviderSpec>,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    thinking: ThinkingConfig,
    session_id: Option<String>,
) -> Result<Value, AgentError> {
    let compat = OpenAiCompatProvider::new(&LUA_OPENAI_CONFIG, super::Timeouts::default());
    Ok(prep_request(
        &compat,
        spec,
        model,
        messages,
        system,
        tools,
        thinking,
        session_id,
        Vec::new(),
    )
    .await?
    .0)
}

impl Provider for LuaOpenAiProvider {
    #[allow(clippy::too_many_arguments)]
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        let spec = Arc::clone(&self.spec);
        Box::pin(async move {
            let auth = self.auth.lock().unwrap().clone();
            let mut buf = String::new();
            let system = with_prefix(&self.system_prefix, system, &mut buf);
            let session = session_id.map(SessionRef::as_str).map(str::to_string);
            let (body, headers) = prep_request(
                &self.compat,
                &spec,
                model,
                messages,
                system,
                tools,
                opts.thinking,
                session,
                auth.headers.clone(),
            )
            .await?;
            let extra: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let hooks = spec.hooks.as_deref();
            self.compat
                .do_stream(model, &extra, &body, event_tx, &auth, hooks)
                .await
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        let auth = self.auth.lock().unwrap().clone();
        Box::pin(async move { self.compat.do_list_models(&auth).await })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        if let Some(h) = &self.spec.hooks
            && let Some(fut) = h.usage()
        {
            return fut;
        }
        Box::pin(async { Ok(None) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
}
