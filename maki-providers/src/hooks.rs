use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::AgentError;
use crate::model::TokenUsage;
use crate::types::ProviderUsage;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Per-spec Lua hooks that shape requests and parse responses without moving
/// the codec into Lua. `Option` return, not a capability query: one call site,
/// and an absent hook costs a `None` check. See §4.4.
///
/// **A dead Lua host is an error, never a skipped hook.** If `on_request`
/// degraded to `None` on a disconnected channel, a provider would silently lose
/// its request shaping — and by the same seam a `LuaAuthResolver` would
/// degrade to "no auth". Fail closed.
///
/// `body` and `extra_headers` are passed as shared `Arc<Mutex<..>>` so the
/// Lua-side handle (which wraps the same arc) mutates the codec's value
/// in place — no JSON round-trip, no key-order scramble (§5.3).
pub trait ProviderHooks: Send + Sync {
    /// Mutate the request body and `extra_headers` before the codec
    /// serialises and sends. `messages` and `tools` live inside `body` and
    /// are reached through nested handles (`body:get("messages")`), so they
    /// are not separate arguments — passing them as `&mut Value` would also
    /// be an uncallable borrow under Rust's aliasing rules.
    fn on_request<'a>(
        &'a self,
        body: Arc<Mutex<Value>>,
        extra_headers: Arc<Mutex<Vec<(String, String)>>>,
        ctx: RequestCtx,
    ) -> Option<BoxFuture<'a, Result<(), AgentError>>>;

    /// Map the raw usage `Value` from the SSE stream to [`TokenUsage`]. Wired
    /// in `openai_compat::parse_sse` only: only that codec carries
    /// `prompt_cache_hit_tokens`. A spec on `codec = "anthropic"` or
    /// `"google"` that declares `on_usage` is **rejected at registration**,
    /// not silently ignored.
    fn on_usage<'a>(
        &'a self,
        raw: &'a Value,
    ) -> Option<BoxFuture<'a, Result<TokenUsage, AgentError>>>;

    /// Fetch provider-side usage quota (balance, remaining percentage). `None`
    /// means the endpoint exists but reports nothing.
    fn usage(&self) -> Option<BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>>>;
}

/// Context handed to `on_request`. Owned (not borrowed) so the returned
/// future need not borrow through the call site; mirrors §5.4's `ctx` table.
/// `capabilities` is the codec-retained subset of `/models` keyed by the
/// spec's `capability_keys` (§1.9); openrouter's `reasoning` and tensorx's
/// `supported_openai_params` both arrive here without a `models()` hook.
pub struct RequestCtx {
    pub model_id: String,
    pub model_provider: Arc<str>,
    pub model_tier: crate::model::ModelTier,
    pub model_family: crate::model::ModelFamily,
    pub model_max_output_tokens: Option<u32>,
    pub model_context_window: u32,
    pub model_supports_thinking: bool,
    pub model_supports_vision: bool,
    pub thinking: crate::types::ThinkingConfig,
    pub session_id: Option<String>,
    pub capabilities: serde_json::Map<String, Value>,
    /// Resolved auth headers for `ctx:request` (§5.5): the provider already
    /// resolved auth at build time, so a hook does not re-enter the key pool.
    pub auth_headers: Vec<(String, String)>,
    /// The spec's `base_url`; `ctx:request` resolves a relative path against
    /// it so the egress set stays statically inspectable.
    pub base_url: Option<String>,
}
