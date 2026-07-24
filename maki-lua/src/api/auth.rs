use std::sync::{Arc, Mutex};

use maki_lua_macro::{lua_fn, lua_table};
use maki_providers::KeyPool;
use mlua::{Lua, Result as LuaResult, Table, Value as LuaValue};

/// Build an env-var API-key auth function-table from {opts}. Return it as a
/// manifest's `auth` field.
///
/// `resolve(_ctx)` reads the key from the environment (with rotation and saved
/// credential support via `KeyPool`) and returns it as a string. `rotate(_ctx)`
/// cycles the pool and returns the next key or nil. The closures are
/// Rust-backed, so the auth strategy stays in Lua: no Rust strategy string is
/// added per auth kind.
///
/// @param opts table Auth options: `slug` (provider slug) and `env_var`
/// (the environment variable holding the API key, comma-separated for rotation).
/// @return (table) `{ resolve = fn, rotate = fn? }`.
#[lua_fn]
fn env_key(lua: &Lua, opts: Table) -> LuaResult<Table> {
    let slug: String = opts.get("slug")?;
    let env_var: String = opts.get("env_var")?;
    let pool: Arc<Mutex<Option<KeyPool>>> = Arc::new(Mutex::new(None));

    let resolve = {
        let slug = slug.clone();
        let env_var = env_var.clone();
        let pool = Arc::clone(&pool);
        lua.create_function(move |_, _: LuaValue| {
            let resolved = KeyPool::resolve(&slug, &env_var).map_err(mlua::Error::runtime)?;
            let key = resolved.current().to_string();
            *pool.lock().unwrap() = Some(resolved);
            Ok(key)
        })?
    };

    let rotate = {
        let pool = Arc::clone(&pool);
        lua.create_function(move |_, _: LuaValue| {
            let guard = pool.lock().unwrap();
            let Some(p) = guard.as_ref() else {
                return Ok(None);
            };
            if p.rotate() {
                Ok(Some(p.current().to_string()))
            } else {
                Ok(None)
            }
        })?
    };

    let auth = lua.create_table()?;
    auth.set("resolve", resolve)?;
    auth.set("rotate", rotate)?;
    Ok(auth)
}

lua_table! {
    /// Auth source constructors for provider manifests. Each function returns a
    /// function-table of `{ resolve = fn, rotate = fn?, refresh = fn? }` whose
    /// members are Rust-backed closures; `maki.provider.register` pulls them out
    /// as functions and hands them to a `LuaAuthSource`.
    ///
    /// ```lua
    /// maki.auth.env_key { slug = "deepseek", env_var = "DEEPSEEK_API_KEY" }
    /// ```
    "maki.auth" => pub(crate) fn create_auth_table(), DOCS [
        env_key,
    ]
}
