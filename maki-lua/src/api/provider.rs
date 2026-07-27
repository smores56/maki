use std::sync::Arc;

use maki_lua_macro::{lua_fn, lua_table};
use maki_providers::manifest_provider::{
    LoginMetadata, ManifestDescriptor, ThinkingDecision, ThinkingHook, UsageParseHook,
    register_manifest_provider,
};
use maki_providers::provider::BoxFuture;
use maki_providers::{AgentError, ProviderUsage};
use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue};

/// Tag {opts} as an OpenAI-compatible engine descriptor and return it.
/// Use the result as a manifest's `engine` field.
///
/// Supported keys: `base_url`, `api_key_env`, `max_tokens_field`,
/// `include_stream_usage`, `provider_name`, `thinking` (e.g. "deepseek"),
/// and `usage_url` (optional balance/quota endpoint fetched by Rust before
/// the manifest's `usage` parse callback runs).
///
/// @param opts table Engine options.
/// @return (table) Tagged engine descriptor.
#[lua_fn]
fn openai_compat(_lua: &Lua, opts: Table) -> LuaResult<Table> {
    opts.set("kind", "openai_compat")?;
    Ok(opts)
}

/// Register a provider manifest from {opts}. A manifest wires a slug to an
/// engine descriptor and a static model list.
///
/// `opts.engine` comes from `maki.provider.openai_compat{...}`. `opts.models`
/// is a list of model entries.
///
/// `opts.login_url` (optional) points the login flow at the page where a user
/// acquires an API key; `opts.needs_url` (optional, default false) prompts for a
/// custom base URL during login. Auth for env-key scope is resolved eagerly in
/// Rust from `opts.engine.api_key_env` (no `resolve`/`rotate`/`refresh` in the
/// manifest).
///
/// `opts.usage` (optional) is a `function(body) -> { plan = string?, limits =
/// table }` parse callback. Rust fetches `opts.engine.usage_url` with the
/// provider's auth and hands the response body string to this callback; it
/// returns a `{ plan, limits }` table mirroring the Rust `ProviderUsage`
/// shape (`limits` is a list of `{ label, percentage?, reset_at?, detail? }`).
///
/// `opts.thinking` (optional) is a `function(enabled, model_id) ->
/// { toggle = string, pad = bool }` callback. Rust resolves `enabled` (from
/// the resolved thinking config) and the model id, then this callback picks
/// the request's `thinking.type` value and whether to back-fill empty
/// `reasoning_content` on assistant turns. Rust applies the body mutation and
/// the shared reasoning-effort primitive (the dialect comes from the engine
/// descriptor's `thinking = "<dialect>"` tag).
///
/// @param opts table Manifest: `{ slug, engine, models, login_url?, needs_url?, usage?, thinking? }`.
/// @return
#[lua_fn]
fn register(lua: &Lua, opts: Table) -> LuaResult<()> {
    let login_url: Option<String> = opts.get("login_url").ok();
    let needs_url: bool = opts.get("needs_url").unwrap_or(false);
    let usage: Option<mlua::Function> = opts.get("usage").ok();
    let thinking: Option<mlua::Function> = opts.get("thinking").ok();

    opts.set("usage", LuaValue::Nil)?;
    opts.set("thinking", LuaValue::Nil)?;

    let desc: ManifestDescriptor = lua
        .from_value(LuaValue::Table(opts.clone()))
        .map_err(|e| mlua::Error::runtime(format!("provider manifest: {e}")))?;
    let login_metadata = LoginMetadata {
        login_url,
        needs_url,
    };
    let (slug_arc, models, mut manifest) = desc.into_manifest_parts();
    let parse_usage = usage.map(|func| -> Arc<dyn UsageParseHook> {
        Arc::new(LuaUsageParser {
            lua: lua.clone(),
            func,
        })
    });
    let thinking_hook = thinking.map(|func| -> Arc<dyn ThinkingHook> {
        Arc::new(LuaThinkingHook {
            lua: lua.clone(),
            func,
        })
    });
    manifest.login = Some(login_metadata);
    manifest.usage_parse = parse_usage;
    manifest.thinking_hook = thinking_hook;
    register_manifest_provider(&slug_arc, models, manifest);
    Ok(())
}

/// Bridge from a manifest `usage` Lua callback to `UsageParseHook`. Rust
/// fetches the `usage_url`, hands the body string to the callback, and this
/// impl deserializes the returned `{ plan, limits }` table into a
/// `ProviderUsage` via the captured `Lua` (Arc-backed, cheaply cloned, idle
/// except for this call).
struct LuaUsageParser {
    lua: Lua,
    func: mlua::Function,
}

impl UsageParseHook for LuaUsageParser {
    fn parse_usage(&self, body: String) -> BoxFuture<'static, Result<ProviderUsage, AgentError>> {
        let lua = self.lua.clone();
        let func = self.func.clone();
        Box::pin(async move {
            let value =
                func.call_async::<LuaValue>(body)
                    .await
                    .map_err(|e| AgentError::Config {
                        message: format!("manifest provider usage parse: {e}"),
                    })?;
            lua.from_value::<ProviderUsage>(value)
                .map_err(|e| AgentError::Config {
                    message: format!("manifest provider usage parse: invalid shape: {e}"),
                })
        })
    }
}

/// Bridge from a manifest `thinking` Lua callback to [`ThinkingHook`]. Rust
/// resolves `enabled` (from `ThinkingConfig::is_enabled`) and passes it plus
/// the model id; the callback returns the ``toggle`/`pad` decision, deserialized
/// here via the captured `Lua` (Arc-backed, cheaply cloned, idle except for
/// this call).
struct LuaThinkingHook {
    lua: Lua,
    func: mlua::Function,
}

impl ThinkingHook for LuaThinkingHook {
    fn decide(
        &self,
        enabled: bool,
        model_id: String,
    ) -> BoxFuture<'static, Result<ThinkingDecision, AgentError>> {
        let lua = self.lua.clone();
        let func = self.func.clone();
        Box::pin(async move {
            let value = func
                .call_async::<LuaValue>((enabled, model_id))
                .await
                .map_err(|e| AgentError::Config {
                    message: format!("manifest provider thinking decision: {e}"),
                })?;
            lua.from_value::<ThinkingDecision>(value)
                .map_err(|e| AgentError::Config {
                    message: format!("manifest provider thinking decision: invalid shape: {e}"),
                })
        })
    }
}

lua_table! {
    /// Engine constructors for provider manifests, plus `register` which loads
    /// a manifest into the provider registry. See the "Authoring a provider"
    /// guide for a full walkthrough.
    ///
    /// ```lua
    /// maki.provider.register {
    ///   slug = "deepseek",
    ///   engine = maki.provider.openai_compat { base_url = "...", api_key_env = "..." },
    ///   login_url = "https://platform.deepseek.com/api_keys",
    ///   models = { { id = "..." } },
    /// }
    /// ```
    "maki.provider" => pub(crate) fn create_provider_table(), DOCS [
        openai_compat, register,
    ]
}
