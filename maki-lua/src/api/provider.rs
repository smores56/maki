use std::sync::Arc;

use maki_lua_macro::{lua_fn, lua_table};
use maki_providers::manifest_provider::{
    LuaAuthSource, ManifestDescriptor, register_manifest_provider,
};
use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue};

/// Tag {opts} as an OpenAI-compatible engine descriptor and return it.
/// Use the result as a manifest's `engine` field.
///
/// The tagged table is plain data: it serializes cleanly into the Rust
/// `EngineDescriptor` enum when `maki.provider.register` runs. Supported
/// keys: `base_url`, `api_key_env`, `max_tokens_field`,
/// `include_stream_usage`, `provider_name`, `thinking` (e.g. "deepseek").
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
/// `opts.engine` comes from `maki.provider.openai_compat{...}`. `opts.auth`
/// is a function-table (e.g. from `maki.auth.env_key`): this pulls out its
/// `resolve` (required), `rotate`, and `refresh` functions before dropping the
/// table, so only plain data serializes into the manifest descriptor. The
/// functions are held by a `LuaAuthSource` and reused per agent session, so
/// `provider_for_slug("<slug>", ...)` routes through the manifest provider.
///
/// @param opts table Manifest: `{ slug, engine, auth, models }`.
/// @return
#[lua_fn]
fn register(lua: &Lua, opts: Table) -> LuaResult<()> {
    let auth_tbl: Table = opts.get("auth")?;
    let resolve: mlua::Function = auth_tbl.get("resolve").map_err(|_| {
        mlua::Error::runtime("provider manifest: auth table must define a 'resolve' function")
    })?;
    let rotate: Option<mlua::Function> = auth_tbl.get("rotate").ok();
    let refresh: Option<mlua::Function> = auth_tbl.get("refresh").ok();

    opts.set("auth", LuaValue::Nil)?;

    let desc: ManifestDescriptor = lua
        .from_value(LuaValue::Table(opts.clone()))
        .map_err(|e| mlua::Error::runtime(format!("provider manifest: {e}")))?;
    let auth = LuaAuthSource::new(desc.slug.clone(), resolve, rotate, refresh);
    let (slug_arc, engine_spec, models, manifest) = desc.into_manifest_parts();
    register_manifest_provider(slug_arc, engine_spec, Arc::new(auth), models, manifest);
    Ok(())
}

lua_table! {
    /// Engine constructors for provider manifests, plus `register` which loads
    /// a manifest into the static provider registry. Each constructor tags an
    /// options table with an engine `kind` and returns it; `register` pulls the
    /// auth functions out of `opts.auth`, deserializes the rest into a manifest
    /// descriptor, and registers the engine + auth + models.
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
