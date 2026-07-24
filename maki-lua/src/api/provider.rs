use std::sync::Arc;

use maki_lua_macro::{lua_fn, lua_table};
use maki_providers::manifest_provider::{
    LuaAuthSource, ManifestDescriptor, UsageParseHook, register_manifest_provider,
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
/// engine descriptor, an auth function-table, and a static model list.
///
/// `opts.engine` comes from `maki.provider.openai_compat{...}` and `opts.auth`
/// from `maki.auth.env_key{...}` (or any table exposing `resolve`/`rotate`/
/// `refresh`). `opts.models` is a list of model entries.
///
/// `opts.usage` (optional) is a `function(body) -> { plan = string?, limits =
/// table }` parse callback. Rust fetches `opts.engine.usage_url` with the
/// provider's auth and hands the response body string to this callback; it
/// returns a `{ plan, limits }` table mirroring the Rust `ProviderUsage`
/// shape (`limits` is a list of `{ label, percentage?, reset_at?, detail? }`).
///
/// @param opts table Manifest: `{ slug, engine, auth, models, usage? }`.
/// @return
#[lua_fn]
fn register(lua: &Lua, opts: Table) -> LuaResult<()> {
    let auth_tbl: Table = opts.get("auth")?;
    let resolve: mlua::Function = auth_tbl.get("resolve").map_err(|_| {
        mlua::Error::runtime("provider manifest: auth table must define a 'resolve' function")
    })?;
    let rotate: Option<mlua::Function> = auth_tbl.get("rotate").ok();
    let refresh: Option<mlua::Function> = auth_tbl.get("refresh").ok();
    let login_url: Option<String> = auth_tbl.get("login_url").ok();
    let needs_url: bool = auth_tbl.get("needs_url").unwrap_or(false);

    let usage: Option<mlua::Function> = opts.get("usage").ok();

    opts.set("auth", LuaValue::Nil)?;
    opts.set("usage", LuaValue::Nil)?;

    let desc: ManifestDescriptor = lua
        .from_value(LuaValue::Table(opts.clone()))
        .map_err(|e| mlua::Error::runtime(format!("provider manifest: {e}")))?;
    let auth = LuaAuthSource::new(
        desc.slug.clone(),
        resolve,
        rotate,
        refresh,
        login_url,
        needs_url,
    );
    let (slug_arc, engine_spec, models, manifest) = desc.into_manifest_parts();
    let parse_usage = usage.map(|func| -> Arc<dyn UsageParseHook> {
        Arc::new(LuaUsageParser {
            lua: lua.clone(),
            func,
        })
    });
    register_manifest_provider(
        slug_arc,
        engine_spec,
        Arc::new(auth),
        parse_usage,
        models,
        manifest,
    );
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

lua_table! {
    /// Engine constructors for provider manifests, plus `register` which loads
    /// a manifest into the provider registry. See the "Authoring a provider"
    /// guide for a full walkthrough.
    ///
    /// ```lua
    /// maki.provider.register {
    ///   slug = "deepseek",
    ///   engine = maki.provider.openai_compat { base_url = "...", api_key_env = "..." },
    ///   auth = maki.auth.env_key { slug = "deepseek", env_var = "DEEPSEEK_API_KEY" },
    ///   models = { { id = "..." } },
    /// }
    /// ```
    "maki.provider" => pub(crate) fn create_provider_table(), DOCS [
        openai_compat, register,
    ]
}
