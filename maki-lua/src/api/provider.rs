use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_lite::io::AsyncReadExt;
use isahc::config::{Configurable, RedirectPolicy};
use isahc::{AsyncBody, HttpClient, Request as HttpRequest};
use std::str::FromStr;

use maki_config::providers::Protocol;
use maki_lua_macro::{lua_fn, lua_table};
use maki_providers::auth::AuthSpec;
use maki_providers::hooks::{BoxFuture, ProviderHooks, RequestCtx};
use maki_providers::model::{ModelEntry, ModelFamily, ModelPricing, ModelTier, TokenUsage};
use maki_providers::registry::{self, LUA_OWNER_PREFIX, ProviderSpec};
use maki_providers::{
    AgentError, EffortSpec, ProviderUsage, ThinkingConfig, build_lua_openai, dialect,
    resolve_lua_auth,
};
use maki_storage::sessions::Effort;
use mlua::{
    Function, Lua, RegistryKey, Result as LuaResult, Table, UserData, UserDataFields,
    UserDataMethods, Value,
};
use serde_json::Value as JsonValue;

use crate::api::net::{extract_host, is_private_ip};
use crate::api::util::convert::{json_to_lua, lua_to_json};
use crate::loader::EventHandle;

const HOST_DISCONNECTED: &str = "provider lua host disconnected";
const UNKNOWN_KEY_ERR: &str = "register_provider: unknown key ";
const MODELS_HOOK_ERR: &str = "register_provider: 'models' hook is not supported yet";
const CODEC_ERR: &str = "register_provider: only codec = \"openai\" is supported";
const AUTH_KIND_ERR: &str = "register_provider: auth.kind must be \"api_key\"";
const SLUG_ERR: &str = "register_provider: 'slug' must be a string";
const NET_ERR: &str = "register_provider: requires the net permission";

const REQUEST_TIMEOUT_SECS: u64 = 30;
const REQUEST_MAX_BYTES: usize = 5 * 1024 * 1024;
const HANDLE_EXPIRED_ERR: &str = "maki provider handle expired";

/// One entry per registered Lua provider: the three optional hook functions as
/// `RegistryKey`s plus a generation counter shared with every handle the
/// runtime builds for that provider's hooks.
pub(crate) struct ProviderFns {
    on_request: Option<RegistryKey>,
    on_usage: Option<RegistryKey>,
    usage: Option<RegistryKey>,
    gen_count: Arc<AtomicU64>,
}

#[derive(Default, Clone)]
pub(crate) struct ProviderStore(pub(crate) Arc<Mutex<HashMap<Arc<str>, ProviderFns>>>);

fn owner_for(plugin: &str) -> Arc<str> {
    Arc::from(format!("{LUA_OWNER_PREFIX}{plugin}"))
}

struct ResponseData {
    body: String,
    status: u16,
    content_type: String,
}

/// A live view into one node of the shared body/headers `Arc<Mutex<Value>>`,
/// addressed by JSON Pointer. The codec owns the arc; the handle only borrows
/// a generation count, so it errors the moment the codec reclaims the request
/// (a stashed handle from a finished hook can never reach the next request's
/// body, §5.3).
#[derive(Clone)]
pub(crate) struct JsonObject {
    arc: Arc<Mutex<JsonValue>>,
    gen_count: u64,
    gen_arc: Arc<AtomicU64>,
    path: String,
}

#[derive(Clone)]
pub(crate) struct JsonArray {
    arc: Arc<Mutex<JsonValue>>,
    gen_count: u64,
    gen_arc: Arc<AtomicU64>,
    path: String,
}

impl JsonObject {
    pub(crate) fn new(
        arc: Arc<Mutex<JsonValue>>,
        gen_count: u64,
        gen_arc: Arc<AtomicU64>,
        path: String,
    ) -> Self {
        Self {
            arc,
            gen_count,
            gen_arc,
            path,
        }
    }

    fn check(&self) -> LuaResult<()> {
        if self.gen_count != self.gen_arc.load(Ordering::Relaxed) {
            return Err(mlua::Error::runtime(HANDLE_EXPIRED_ERR));
        }
        Ok(())
    }
}

impl JsonArray {
    pub(crate) fn new(
        arc: Arc<Mutex<JsonValue>>,
        gen_count: u64,
        gen_arc: Arc<AtomicU64>,
        path: String,
    ) -> Self {
        Self {
            arc,
            gen_count,
            gen_arc,
            path,
        }
    }

    fn check(&self) -> LuaResult<()> {
        if self.gen_count != self.gen_arc.load(Ordering::Relaxed) {
            return Err(mlua::Error::runtime(HANDLE_EXPIRED_ERR));
        }
        Ok(())
    }
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn child_pointer(path: &str, key: &str) -> String {
    let esc = escape_pointer(key);
    if path.is_empty() {
        format!("/{esc}")
    } else {
        format!("{path}/{esc}")
    }
}

impl UserData for JsonObject {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |lua, this, key: String| {
            this.check()?;
            let pointer = child_pointer(&this.path, &key);
            let guard = this.arc.lock().unwrap();
            let Some(value) = guard.pointer(&pointer) else {
                return Ok(Value::Nil);
            };
            match value {
                JsonValue::Object(_) | JsonValue::Array(_) => {
                    nested_handle(lua, this, &pointer, value)
                }
                other => json_to_lua(lua, other),
            }
        });
        methods.add_method("set", |lua, this, (key, value): (String, Value)| {
            this.check()?;
            let json = lua_to_json(lua, &value)?;
            let mut guard = this.arc.lock().unwrap();
            let Some(parent) = guard.pointer_mut(&this.path) else {
                return Err(mlua::Error::runtime("maki handle: path does not exist"));
            };
            let JsonValue::Object(map) = parent else {
                return Err(mlua::Error::runtime(
                    "maki handle: set target is not an object",
                ));
            };
            map.insert(key, json);
            Ok(())
        });
        methods.add_method("has", |_, this, key: String| {
            this.check()?;
            let pointer = child_pointer(&this.path, &key);
            let guard = this.arc.lock().unwrap();
            Ok(Value::Boolean(guard.pointer(&pointer).is_some()))
        });
    }
}

/// `pointer_mut` for a nested-set lands on the existing child node and replaces
/// it in place iff the parent container already holds it. `obj:set("k", v)`
/// cannot create a missing key through `pointer_mut("/k")` on an object that
/// lacks `k`, so `set` only mutates existing leaves — matching the request-path
/// contract (the codec always emits the structural keys a hook patches).
fn nested_handle(
    lua: &Lua,
    src: &JsonObject,
    pointer: &str,
    value: &JsonValue,
) -> LuaResult<Value> {
    let arc = Arc::clone(&src.arc);
    let gen_count = src.gen_count;
    let gen_arc = Arc::clone(&src.gen_arc);
    let ud = match value {
        JsonValue::Array(_) => {
            lua.create_userdata(JsonArray::new(arc, gen_count, gen_arc, pointer.to_string()))?
        }
        _ => lua.create_userdata(JsonObject::new(
            arc,
            gen_count,
            gen_arc,
            pointer.to_string(),
        ))?,
    };
    Ok(Value::UserData(ud))
}

impl UserData for JsonArray {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("len", |_, this, ()| {
            this.check()?;
            let guard = this.arc.lock().unwrap();
            let count = guard
                .pointer(&this.path)
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            Ok(Value::Integer(count as i64))
        });
        methods.add_method("get", |lua, this, idx: i64| {
            this.check()?;
            // Lua arrays are 1-indexed; JSON Pointer is 0-indexed.
            let zero = idx - 1;
            let pointer = format!("{}/{zero}", this.path);
            let guard = this.arc.lock().unwrap();
            let Some(value) = guard.pointer(&pointer) else {
                return Ok(Value::Nil);
            };
            match value {
                JsonValue::Object(_) | JsonValue::Array(_) => {
                    nested_array_handle(lua, this, &pointer, value)
                }
                other => json_to_lua(lua, other),
            }
        });
        methods.add_meta_method(mlua::MetaMethod::Len, |_, this, ()| {
            this.check()?;
            let guard = this.arc.lock().unwrap();
            let count = guard
                .pointer(&this.path)
                .and_then(|v| v.as_array())
                .map(Vec::len)
                .unwrap_or(0);
            Ok(count as i64)
        });
    }
}

/// Builds the handle value (does not wrap in UserData — `get` wraps object/
/// array results uniformly). Kept as a helper so the object-shape decision lives
/// in one place.
fn nested_array_handle(
    lua: &Lua,
    src: &JsonArray,
    pointer: &str,
    value: &JsonValue,
) -> LuaResult<Value> {
    let arc = Arc::clone(&src.arc);
    let gen_count = src.gen_count;
    let gen_arc = Arc::clone(&src.gen_arc);
    let ud = match value {
        JsonValue::Array(_) => {
            lua.create_userdata(JsonArray::new(arc, gen_count, gen_arc, pointer.to_string()))?
        }
        _ => lua.create_userdata(JsonObject::new(
            arc,
            gen_count,
            gen_arc,
            pointer.to_string(),
        ))?,
    };
    Ok(Value::UserData(ud))
}

/// Constructed by the runtime dispatch, not by Lua: holds the codec-built
/// [`RequestCtx`] plus the shared body/headers arcs. `tools` is `nil` when
/// the body has no tools. `ctx:request` does its HTTP directly (isahc), so no
/// runtime channel is needed here.
pub(crate) struct LuaRequestCtx {
    ctx: RequestCtx,
    body_arc: Arc<Mutex<JsonValue>>,
    headers_arc: Arc<Mutex<JsonValue>>,
    gen_count: u64,
    gen_arc: Arc<AtomicU64>,
}

impl LuaRequestCtx {
    pub(crate) fn new(
        ctx: RequestCtx,
        body_arc: Arc<Mutex<JsonValue>>,
        headers_arc: Arc<Mutex<JsonValue>>,
        gen_count: u64,
        gen_arc: Arc<AtomicU64>,
    ) -> Self {
        Self {
            ctx,
            body_arc,
            headers_arc,
            gen_count,
            gen_arc,
        }
    }

    fn check(&self) -> LuaResult<()> {
        if self.gen_count != self.gen_arc.load(Ordering::Relaxed) {
            return Err(mlua::Error::runtime(HANDLE_EXPIRED_ERR));
        }
        Ok(())
    }
}

fn model_table(lua: &Lua, ctx: &RequestCtx) -> LuaResult<Table> {
    let t = lua.create_table()?;
    t.set("id", ctx.model_id.clone())?;
    t.set("provider", ctx.model_provider.as_ref().to_string())?;
    t.set("tier", ctx.model_tier.to_string())?;
    t.set("family", model_family_str(ctx.model_family))?;
    t.set("max_output_tokens", ctx.model_max_output_tokens)?;
    t.set("context_window", ctx.model_context_window)?;
    t.set("supports_thinking", ctx.model_supports_thinking)?;
    t.set("supports_vision", ctx.model_supports_vision)?;
    let caps = lua.create_table()?;
    for (k, v) in &ctx.capabilities {
        caps.set(k.as_str(), json_to_lua(lua, v)?)?;
    }
    t.set("capabilities", caps)?;
    Ok(t)
}

fn thinking_table(lua: &Lua, thinking: ThinkingConfig) -> LuaResult<Table> {
    let t = lua.create_table()?;
    let (mode, effort, budget) = match thinking {
        ThinkingConfig::Off => ("off", None, None),
        ThinkingConfig::Adaptive => ("adaptive", None, None),
        ThinkingConfig::Effort(e) => ("effort", Some(e.as_str().to_string()), None),
        ThinkingConfig::Budget(n) => ("budget", None, Some(n)),
    };
    t.set("enabled", thinking.is_enabled())?;
    t.set("mode", mode)?;
    if let Some(eff) = effort {
        t.set("effort", eff)?;
    }
    if let Some(b) = budget {
        t.set("budget", b)?;
    }
    Ok(t)
}

fn model_family_str(f: ModelFamily) -> &'static str {
    match f {
        ModelFamily::Claude => "claude",
        ModelFamily::Generic => "generic",
        ModelFamily::Gemini => "gemini",
        ModelFamily::Glm => "glm",
        ModelFamily::Gpt => "gpt",
        ModelFamily::Synthetic => "synthetic",
    }
}

impl UserData for LuaRequestCtx {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("model", |lua, this| {
            this.check()?;
            model_table(lua, &this.ctx)
        });
        fields.add_field_method_get("thinking", |lua, this| {
            this.check()?;
            thinking_table(lua, this.ctx.thinking)
        });
        fields.add_field_method_get("session_id", |lua, this| {
            this.check()?;
            match &this.ctx.session_id {
                Some(s) => Ok(Value::String(lua.create_string(s)?)),
                None => Ok(Value::Nil),
            }
        });
        fields.add_field_method_get("messages", |lua, this| {
            this.check()?;
            let arc = Arc::clone(&this.body_arc);
            if arc.lock().unwrap().pointer("/messages").is_none() {
                return Ok(Value::Nil);
            }
            let handle = JsonArray::new(
                arc,
                this.gen_count,
                Arc::clone(&this.gen_arc),
                "/messages".to_string(),
            );
            Ok(Value::UserData(lua.create_userdata(handle)?))
        });
        fields.add_field_method_get("tools", |lua, this| {
            this.check()?;
            let arc = Arc::clone(&this.body_arc);
            if arc.lock().unwrap().pointer("/tools").is_none() {
                return Ok(Value::Nil);
            }
            let handle = JsonArray::new(
                arc,
                this.gen_count,
                Arc::clone(&this.gen_arc),
                "/tools".to_string(),
            );
            Ok(Value::UserData(lua.create_userdata(handle)?))
        });
        fields.add_field_method_get("headers", |lua, this| {
            this.check()?;
            let handle = JsonObject::new(
                Arc::clone(&this.headers_arc),
                this.gen_count,
                Arc::clone(&this.gen_arc),
                String::new(),
            );
            Ok(Value::UserData(lua.create_userdata(handle)?))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method(
            "request",
            |lua, this, (path, opts): (String, Option<Table>)| async move {
                this.check()?;
                do_ctx_request(lua, &this, &path, opts).await
            },
        );
    }
}

/// `ctx:request(path, opts)`: path-relative, pinned no-redirect, auth from the
/// spec's resolved headers, SSRF waived only for a loopback/private `base_url`
/// (covers Ollama and llama-cpp, nothing else). Body capped and URL redacted
/// in errors so a 4xx body never reaches the model verbatim (§5.5).
async fn do_ctx_request(
    lua: Lua,
    this: &LuaRequestCtx,
    path: &str,
    opts: Option<Table>,
) -> LuaResult<(Value, Value)> {
    let Some(base_url) = this.ctx.base_url.clone() else {
        return err_pair(&lua, "ctx:request: provider has no base_url");
    };
    if !path.starts_with('/') {
        return err_pair(&lua, "ctx:request: path must be relative (start with '/')");
    }
    let full_url = format!("{}{}", base_url.trim_end_matches('/'), path);

    let base_private = extract_host(&base_url)
        .and_then(|h| h.parse::<IpAddr>().ok())
        .is_some_and(|ip| is_private_ip(&ip));
    if !base_private && let Err(e) = ssrf_check(&full_url) {
        return err_pair(&lua, &e);
    }

    let method = opts
        .as_ref()
        .and_then(|o| o.get::<String>("method").ok())
        .unwrap_or_else(|| "GET".to_string());
    let body = opts
        .as_ref()
        .and_then(|o| o.get::<String>("body").ok())
        .map(String::into_bytes)
        .unwrap_or_default();
    let timeout = opts
        .as_ref()
        .and_then(|o| o.get::<u64>("timeout").ok())
        .unwrap_or(REQUEST_TIMEOUT_SECS);
    let max_bytes = opts
        .as_ref()
        .and_then(|o| o.get::<usize>("max_bytes").ok())
        .unwrap_or(REQUEST_MAX_BYTES);

    let mut headers = this.ctx.auth_headers.clone();
    if let Some(tbl) = opts.as_ref().and_then(|o| o.get::<Table>("headers").ok()) {
        for pair in tbl.pairs::<String, String>() {
            let (k, v) = pair?;
            headers.push((k, v));
        }
    }

    match exec_request(&full_url, &method, &headers, body, timeout, max_bytes).await {
        Ok(resp) => {
            let tbl = lua.create_table()?;
            tbl.set("body", resp.body)?;
            tbl.set("status", resp.status)?;
            tbl.set("content_type", resp.content_type)?;
            Ok((Value::Table(tbl), Value::Nil))
        }
        Err(e) => {
            let redacted = redact_url(&full_url);
            err_pair(&lua, &format!("{redacted}: {e}"))
        }
    }
}

fn err_pair(lua: &Lua, msg: &str) -> LuaResult<(Value, Value)> {
    Ok((Value::Nil, Value::String(lua.create_string(msg)?)))
}

fn redact_url(url: &str) -> String {
    match extract_host(url) {
        Some(host) => format!("https://{host}/..."),
        None => "<redacted>".to_string(),
    }
}

fn ssrf_check(url: &str) -> Result<(), String> {
    let host = extract_host(url).ok_or("cannot extract host from URL")?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Err(format!("blocked: {ip} is a private/metadata address"));
        }
        return Ok(());
    }
    Ok(())
}

async fn exec_request(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
    timeout: u64,
    max_bytes: usize,
) -> Result<ResponseData, String> {
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(timeout))
        .redirect_policy(RedirectPolicy::None)
        .build()
        .map_err(|e| format!("client error: {e}"))?;

    let mut builder = HttpRequest::builder().method(method).uri(url);
    for (k, v) in headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    let req = builder
        .body(AsyncBody::from(body))
        .map_err(|e| format!("request build error: {e}"))?;
    let mut response = client
        .send_async(req)
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let mut bytes = Vec::new();
    response
        .body_mut()
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("read error: {e}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("response too large: {} bytes", bytes.len()));
    }
    Ok(ResponseData {
        body: String::from_utf8_lossy(&bytes).into_owned(),
        status,
        content_type,
    })
}

/// Implements `ProviderHooks` for a Lua-registered spec: each method ships the
/// work to the Lua runtime thread via a `Request` and awaits the reply. A dead
/// host is an error, never a skipped hook (§4.4). The presence booleans are
/// snapshotted at registration so the agent thread never locks the store.
pub(crate) struct LuaHooks {
    event: EventHandle,
    slug: Arc<str>,
    has_on_request: bool,
    has_on_usage: bool,
    has_usage: bool,
}

impl LuaHooks {
    fn new(
        event: EventHandle,
        slug: Arc<str>,
        has_on_request: bool,
        has_on_usage: bool,
        has_usage: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            event,
            slug,
            has_on_request,
            has_on_usage,
            has_usage,
        })
    }

    fn fail_closed<T>(&self) -> Option<BoxFuture<'static, Result<T, AgentError>>>
    where
        T: Send + 'static,
    {
        Some(Box::pin(async move {
            Err(AgentError::Config {
                message: HOST_DISCONNECTED.into(),
            })
        }))
    }
}

async fn send_recv<T, F>(event: &EventHandle, build: F) -> Result<T, AgentError>
where
    T: Send + 'static,
    F: FnOnce(flume::Sender<Result<T, AgentError>>) -> crate::runtime::Request,
{
    let (tx, rx) = flume::bounded(1);
    event
        .request_sender()
        .send(build(tx))
        .map_err(|_| AgentError::Config {
            message: HOST_DISCONNECTED.into(),
        })?;
    rx.recv_async().await.map_err(|_| AgentError::Config {
        message: HOST_DISCONNECTED.into(),
    })?
}

impl ProviderHooks for LuaHooks {
    fn on_request<'a>(
        &'a self,
        body: Arc<Mutex<JsonValue>>,
        extra_headers: Arc<Mutex<Vec<(String, String)>>>,
        ctx: RequestCtx,
    ) -> Option<BoxFuture<'a, Result<(), AgentError>>> {
        if !self.has_on_request {
            return None;
        }
        if self.event.is_disconnected() {
            return self.fail_closed::<()>();
        }
        let slug = Arc::clone(&self.slug);
        let event = self.event.clone();
        Some(Box::pin(async move {
            send_recv(&event, |reply| crate::runtime::Request::HookOnRequest {
                slug: Arc::clone(&slug),
                body: Arc::clone(&body),
                headers: extra_headers,
                ctx,
                reply,
            })
            .await
        }))
    }

    fn on_usage<'a>(
        &'a self,
        raw: &'a JsonValue,
    ) -> Option<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<TokenUsage, AgentError>> + Send + 'a>,
        >,
    > {
        if !self.has_on_usage {
            return None;
        }
        if self.event.is_disconnected() {
            return self.fail_closed::<TokenUsage>();
        }
        let slug = Arc::clone(&self.slug);
        let event = self.event.clone();
        let raw = raw.clone();
        Some(Box::pin(async move {
            send_recv(&event, |reply| crate::runtime::Request::HookOnUsage {
                slug: Arc::clone(&slug),
                raw,
                reply,
            })
            .await
        }))
    }

    fn usage(
        &self,
    ) -> Option<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Option<ProviderUsage>, AgentError>> + Send>,
        >,
    > {
        if !self.has_usage {
            return None;
        }
        if self.event.is_disconnected() {
            return self.fail_closed::<Option<ProviderUsage>>();
        }
        let slug = Arc::clone(&self.slug);
        let event = self.event.clone();
        Some(Box::pin(async move {
            let spec = registry::get(&slug).ok_or_else(|| AgentError::Config {
                message: format!("provider {slug} not registered"),
            })?;
            let auth = resolve_lua_auth(&spec)?;
            let ctx = RequestCtx {
                model_id: String::new(),
                model_provider: Arc::clone(&slug),
                model_tier: ModelTier::Medium,
                model_family: ModelFamily::Generic,
                model_max_output_tokens: None,
                model_context_window: 0,
                model_supports_thinking: false,
                model_supports_vision: false,
                thinking: ThinkingConfig::Off,
                session_id: None,
                capabilities: serde_json::Map::new(),
                auth_headers: auth.headers,
                base_url: spec.base_url.clone(),
            };
            send_recv(&event, |reply| crate::runtime::Request::HookUsage {
                slug: Arc::clone(&slug),
                ctx,
                reply,
            })
            .await
        }))
    }
}

/// Resolves one hook's Lua function under the store lock, returning it plus the
/// generation snapshot its handles must carry. The lock is held only while the
/// registry is read, never across the Lua call.
fn lookup_fn(
    lua: &Lua,
    slug: &Arc<str>,
    field: fn(&ProviderFns) -> Option<&RegistryKey>,
    name: &str,
) -> LuaResult<(Function, Arc<AtomicU64>, u64)> {
    let store = lua
        .app_data_ref::<ProviderStore>()
        .ok_or_else(|| mlua::Error::runtime(NET_ERR))?;
    let guard = store.0.lock().unwrap();
    let Some(fns) = guard.get(slug) else {
        return Err(mlua::Error::runtime(format!(
            "provider {slug} has no hooks"
        )));
    };
    let Some(key) = field(fns) else {
        return Err(mlua::Error::runtime(format!(
            "provider {slug} has no {name}"
        )));
    };
    let func = lua.registry_value::<Function>(key)?;
    Ok((
        func,
        Arc::clone(&fns.gen_count),
        fns.gen_count.load(Ordering::Relaxed),
    ))
}

/// Runtime-thread handler for `HookOnRequest`: builds the body handle and ctx,
/// calls the Lua `on_request`, then bumps the generation (stashing any handle
/// past the hook is an error on next touch) and syncs the headers object back
/// into the codec's header vec.
pub(crate) fn dispatch_on_request(
    lua: &Lua,
    slug: &Arc<str>,
    body: Arc<Mutex<JsonValue>>,
    headers: Arc<Mutex<Vec<(String, String)>>>,
    ctx: RequestCtx,
) -> Result<(), AgentError> {
    let (func, gen_arc, gen_count) = lookup_fn(lua, slug, |f| f.on_request.as_ref(), "on_request")
        .map_err(|e| AgentError::Config {
            message: e.to_string(),
        })?;
    let headers_obj = Arc::new(Mutex::new(JsonValue::Object(
        headers
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), JsonValue::String(v.clone())))
            .collect(),
    )));
    let body_handle = JsonObject::new(
        Arc::clone(&body),
        gen_count,
        Arc::clone(&gen_arc),
        String::new(),
    );
    let lctx = LuaRequestCtx::new(
        ctx,
        Arc::clone(&body),
        Arc::clone(&headers_obj),
        gen_count,
        Arc::clone(&gen_arc),
    );
    let res = func.call::<()>((body_handle, lctx));
    gen_arc.fetch_add(1, Ordering::Relaxed);
    let mut hh = headers.lock().unwrap();
    hh.clear();
    if let JsonValue::Object(map) = &*headers_obj.lock().unwrap() {
        for (k, v) in map {
            hh.push((k.clone(), v.to_string()));
        }
    }
    res.map_err(|e| AgentError::Config {
        message: format!("on_request hook failed: {e}"),
    })
}

/// Runtime-thread handler for `HookOnUsage`: calls `on_usage(raw)` and
/// deserializes the returned table into `TokenUsage` via its serde names.
pub(crate) fn dispatch_on_usage(
    lua: &Lua,
    slug: &Arc<str>,
    raw: &JsonValue,
) -> Result<TokenUsage, AgentError> {
    let (func, _, _) = lookup_fn(lua, slug, |f| f.on_usage.as_ref(), "on_usage").map_err(|e| {
        AgentError::Config {
            message: e.to_string(),
        }
    })?;
    let raw_lua = json_to_lua(lua, raw).map_err(|e| AgentError::Config {
        message: e.to_string(),
    })?;
    let value: Value = func.call(raw_lua).map_err(|e| AgentError::Config {
        message: format!("on_usage hook failed: {e}"),
    })?;
    let json = lua_to_json(lua, &value).map_err(|e| AgentError::Config {
        message: e.to_string(),
    })?;
    serde_json::from_value::<TokenUsage>(json).map_err(|e| AgentError::Config {
        message: format!("on_usage returned invalid usage: {e}"),
    })
}

/// Runtime-thread handler for `HookUsage`: runs the `usage` hook as a Lua
/// coroutine (so its `ctx:request` async calls poll), awaits it, and converts
/// the non-nil result into `ProviderUsage`. `(nil, err)` is an error.
pub(crate) async fn dispatch_usage(
    lua: &Lua,
    slug: &Arc<str>,
    ctx: RequestCtx,
) -> Result<Option<ProviderUsage>, AgentError> {
    let (func, gen_arc, gen_count) =
        lookup_fn(lua, slug, |f| f.usage.as_ref(), "usage").map_err(|e| AgentError::Config {
            message: e.to_string(),
        })?;
    let body = Arc::new(Mutex::new(JsonValue::Null));
    let headers = Arc::new(Mutex::new(JsonValue::Object(serde_json::Map::new())));
    let body_handle = JsonObject::new(
        Arc::clone(&body),
        gen_count,
        Arc::clone(&gen_arc),
        String::new(),
    );
    let lctx = LuaRequestCtx::new(ctx, body, headers, gen_count, gen_arc);
    let thread = lua.create_thread(func).map_err(|e| AgentError::Config {
        message: e.to_string(),
    })?;
    let (value, err): (Value, Value) = thread
        .into_async((body_handle, lctx))
        .map_err(|e| AgentError::Config {
            message: format!("usage hook failed: {e}"),
        })?
        .await
        .map_err(|e| AgentError::Config {
            message: format!("usage hook failed: {e}"),
        })?;
    if matches!(value, Value::Nil) {
        let msg = match &err {
            Value::String(s) => s.to_string_lossy(),
            _ => "usage hook returned nil".to_string(),
        };
        return Err(AgentError::Config { message: msg });
    }
    let json = lua_to_json(lua, &value).map_err(|e| AgentError::Config {
        message: e.to_string(),
    })?;
    let usage = serde_json::from_value::<ProviderUsage>(json).map_err(|e| AgentError::Config {
        message: format!("usage hook returned invalid usage: {e}"),
    })?;
    Ok(Some(usage))
}

const KNOWN_KEYS: &[&str] = &[
    "slug",
    "display_name",
    "family",
    "features",
    "codec",
    "base_url",
    "auth",
    "supports_thinking",
    "accepts_arbitrary_models",
    "context_window",
    "max_output_tokens",
    "effort",
    "capability_keys",
    "models",
    "on_request",
    "on_usage",
    "usage",
];

/// Register a provider spec from a Lua table. Requires the `net` permission
/// (so `plugin.toml`'s `net = false` is enforceable against a provider, and
/// today's `denied()` init.lua cannot register one). Unknown top-level keys are
/// an error — a typo in `pricing_input` would silently mis-cost every request,
/// and the reject list is how `models`/`on_error` stay honestly unsupported.
///
/// @param spec table Provider definition (see `providers` docs).
/// @return () Nothing; throws on an invalid spec.
/// @example
/// maki.api.register_provider({ slug = "deepseek", codec = "openai", ... })
#[lua_fn(guard = Net)]
fn register_provider(lua: &Lua, #[ctx] plugin: Arc<str>, spec: Table) -> LuaResult<()> {
    let mut keys: Vec<String> = Vec::new();
    let mut models_is_fn = false;
    for pair in spec.pairs::<Value, Value>() {
        let (k, v) = pair?;
        if let Value::String(s) = &k {
            let name = s.to_string_lossy().to_string();
            if name == "models" && matches!(v, Value::Function(_)) {
                models_is_fn = true;
            }
            keys.push(name);
        }
    }
    for name in &keys {
        if !KNOWN_KEYS.contains(&name.as_str()) {
            return Err(mlua::Error::runtime(format!("{UNKNOWN_KEY_ERR}'{name}'")));
        }
    }
    if models_is_fn {
        return Err(mlua::Error::runtime(MODELS_HOOK_ERR));
    }

    let slug: String = spec
        .get("slug")
        .ok()
        .and_then(|s: Value| match s {
            Value::String(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .ok_or_else(|| mlua::Error::runtime(SLUG_ERR))?;
    let slug: Arc<str> = Arc::from(slug);

    let display_name: String = spec.get("display_name").unwrap_or_default();
    let family = parse_family(&spec)?;
    let features: Option<String> = spec.get("features").ok();
    let base_url: Option<String> = spec.get("base_url").ok();
    let supports_thinking: bool = spec.get("supports_thinking").unwrap_or(false);
    let accepts_arbitrary_models: bool = spec.get("accepts_arbitrary_models").unwrap_or(false);
    let context_window: u32 = spec.get("context_window").unwrap_or(0);
    let max_output_tokens: Option<u32> = spec.get("max_output_tokens").ok();
    let fallback_context_window = context_window;
    let capability_keys = parse_string_vec(&spec, "capability_keys")?;

    let codec: String = spec
        .get::<String>("codec")
        .unwrap_or_else(|_| "openai".to_string());
    if codec != "openai" {
        return Err(mlua::Error::runtime(CODEC_ERR));
    }
    let effort = parse_effort(&spec)?;
    let auth = parse_auth(&spec)?;
    let models = parse_models(lua, &spec, family)?;

    let on_request_key = spec
        .get::<Function>("on_request")
        .ok()
        .map(|f| lua.create_registry_value(f))
        .transpose()?;
    let on_usage_key = spec
        .get::<Function>("on_usage")
        .ok()
        .map(|f| lua.create_registry_value(f))
        .transpose()?;
    let usage_key = spec
        .get::<Function>("usage")
        .ok()
        .map(|f| lua.create_registry_value(f))
        .transpose()?;

    let event = lua
        .app_data_ref::<EventHandle>()
        .ok_or_else(|| mlua::Error::runtime(NET_ERR))?;
    let event = (*event).clone();

    let owner = owner_for(plugin.as_ref());
    let hooks = LuaHooks::new(
        event.clone(),
        Arc::clone(&slug),
        on_request_key.is_some(),
        on_usage_key.is_some(),
        usage_key.is_some(),
    );
    let spec = ProviderSpec {
        slug: Arc::clone(&slug),
        owner: Some(owner),
        display_name,
        family,
        features,
        protocol: Protocol::Openai,
        base_url,
        default_model: models
            .iter()
            .find(|m| m.default)
            .and_then(|m| m.prefixes.first().cloned()),
        auth,
        supports_thinking,
        accepts_arbitrary_models,
        fallback_max_output: max_output_tokens,
        fallback_context_window,
        capability_keys,
        models,
        effort,
        hooks: Some(hooks as Arc<dyn ProviderHooks>),
        build: build_lua_openai,
    };

    let store = lua
        .app_data_ref::<ProviderStore>()
        .ok_or_else(|| mlua::Error::runtime(NET_ERR))?;
    {
        let mut g = store.0.lock().unwrap();
        g.insert(
            Arc::clone(&slug),
            ProviderFns {
                on_request: on_request_key,
                on_usage: on_usage_key,
                usage: usage_key,
                gen_count: Arc::new(AtomicU64::new(0)),
            },
        );
    }
    registry::register(spec);
    Ok(())
}

/// Run a provider-definition chunk with rollback: if `fn` throws after
/// registering a spec, that spec (and its hook entry) is removed so a
/// half-configured provider never ships. Returns `true` on success, `false`
/// plus an error string otherwise.
///
/// @param name string Logical provider name (for logging only).
/// @param func function Chunk that calls `maki.api.register_provider`.
/// @return (boolean, string?) Success flag, or nil plus an error.
#[lua_fn]
fn provider_scope(
    lua: &Lua,
    #[ctx] plugin: Arc<str>,
    name: String,
    func: Function,
) -> LuaResult<(bool, Value)> {
    let owner = owner_for(plugin.as_ref());
    let prior_store: Vec<Arc<str>> = lua
        .app_data_ref::<ProviderStore>()
        .map(|s| s.0.lock().unwrap().keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let prior_slugs: Vec<Arc<str>> = registry::all()
        .into_iter()
        .filter(|s| s.owner.as_deref() == Some(owner.as_ref()))
        .map(|s| Arc::clone(&s.slug))
        .collect();

    match func.call::<()>(()) {
        Ok(()) => Ok((true, Value::Nil)),
        Err(e) => {
            let msg = e.to_string();
            let current: Vec<Arc<str>> = registry::all()
                .into_iter()
                .filter(|s| s.owner.as_deref() == Some(owner.as_ref()))
                .map(|s| Arc::clone(&s.slug))
                .collect();
            for slug in &current {
                if !prior_slugs.contains(slug) {
                    registry::remove(slug);
                }
            }
            if let Some(store) = lua.app_data_ref::<ProviderStore>() {
                let mut g = store.0.lock().unwrap();
                let keys: Vec<Arc<str>> = g.keys().cloned().collect();
                for slug in keys {
                    if !prior_store.contains(&slug) {
                        g.remove(&slug);
                    }
                }
            }
            tracing::warn!(provider = %name, error = %msg, "provider_scope rolled back");
            Ok((false, Value::String(lua.create_string(msg)?)))
        }
    }
}

fn parse_family(spec: &Table) -> LuaResult<ModelFamily> {
    let raw: String = spec
        .get::<String>("family")
        .unwrap_or_else(|_| "generic".to_string());
    ModelFamily::from_str_name(&raw)
        .ok_or_else(|| mlua::Error::runtime(format!("register_provider: unknown family '{raw}'")))
}

fn parse_string_vec(spec: &Table, key: &str) -> LuaResult<Vec<String>> {
    let Some(tbl) = spec.get::<Table>(key).ok() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for pair in tbl.sequence_values::<String>() {
        out.push(pair?);
    }
    Ok(out)
}

fn parse_effort(spec: &Table) -> LuaResult<Option<EffortSpec>> {
    let Some(tbl) = spec.get::<Table>("effort").ok() else {
        return Ok(None);
    };
    let supported_raw = parse_string_vec(&tbl, "supported")?;
    let mut supported = Vec::new();
    for s in supported_raw {
        let e = Effort::from_str(&s).map_err(|_| {
            mlua::Error::runtime(format!("register_provider: unknown effort '{s}'"))
        })?;
        supported.push(e);
    }
    let adaptive = tbl
        .get::<String>("adaptive")
        .ok()
        .and_then(|s| Effort::from_str(&s).ok());
    let off = tbl
        .get::<String>("off")
        .ok()
        .and_then(|s| (s == dialect::OFF).then_some(dialect::OFF));
    Ok(Some(EffortSpec {
        supported,
        adaptive,
        off,
    }))
}

fn parse_auth(spec: &Table) -> LuaResult<AuthSpec> {
    let tbl: Table = spec
        .get("auth")
        .ok()
        .ok_or_else(|| mlua::Error::runtime("register_provider: 'auth' is required"))?;
    let kind: String = tbl
        .get::<String>("kind")
        .unwrap_or_else(|_| "api_key".to_string());
    if kind != "api_key" {
        return Err(mlua::Error::runtime(AUTH_KIND_ERR));
    }
    let env: String = tbl
        .get("env")
        .ok()
        .ok_or_else(|| mlua::Error::runtime("register_provider: auth.env is required"))?;
    let login_url: Option<String> = tbl.get("login_url").ok();
    let needs_url: bool = tbl.get("needs_url").unwrap_or(false);
    Ok(AuthSpec::ApiKey {
        env,
        login_url,
        needs_url,
        plans: None,
    })
}

fn parse_models(lua: &Lua, spec: &Table, family: ModelFamily) -> LuaResult<Vec<ModelEntry>> {
    let Some(tbl) = spec.get::<Table>("models").ok() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in tbl.sequence_values::<Table>() {
        let entry = entry?;
        let prefixes: Vec<String> = parse_string_vec(&entry, "prefixes")?;
        if prefixes.is_empty() {
            return Err(mlua::Error::runtime(
                "register_provider: model entry needs 'prefixes'",
            ));
        }
        let tier: String = entry
            .get::<String>("tier")
            .unwrap_or_else(|_| "medium".to_string());
        let tier = ModelTier::from_str(&tier).map_err(|_| {
            mlua::Error::runtime(format!("register_provider: invalid tier '{tier}'"))
        })?;
        let entry_family = entry
            .get::<String>("family")
            .ok()
            .and_then(|s| ModelFamily::from_str_name(&s))
            .unwrap_or(family);
        let vision: bool = entry.get("vision").unwrap_or(false);
        let default: bool = entry.get("default").unwrap_or(false);
        let max_output_tokens: Option<u32> = entry
            .get("max_output_tokens")
            .ok()
            .or_else(|| spec.get("max_output_tokens").ok());
        let context_window: u32 = entry
            .get("context_window")
            .unwrap_or(spec.get("context_window").unwrap_or(0));
        let pricing = match entry.get::<Value>("pricing")? {
            Value::Nil => ModelPricing::default(),
            v => {
                let json = lua_to_json(lua, &v)?;
                serde_json::from_value(json).map_err(|e| {
                    mlua::Error::runtime(format!("register_provider: invalid pricing: {e}"))
                })?
            }
        };
        out.push(ModelEntry {
            prefixes,
            tier,
            family: entry_family,
            vision,
            default,
            pricing,
            max_output_tokens,
            context_window,
        });
    }
    Ok(out)
}

lua_table! {
    /// Provider registration: define a provider from Lua (codecs stay Rust).
    /// `register_provider` builds a [`ProviderSpec`](../providers) and feeds
    /// the openai-compat codec the spec's hooks; `provider_scope` runs a
    /// definition chunk with rollback so a throw after a partial registration
    /// leaves nothing installed.
    ///
    /// ```lua
    /// maki.api.provider_scope("deepseek", function() require("deepseek") end)
    /// ```
    extend "maki.api" => pub(crate) fn add_provider_fns(perms: &crate::plugin_permissions::PluginPermissions, plugin: Arc<str>), PROVIDER_DOCS [
        register_provider(perms, plugin),
        provider_scope(plugin),
    ]
}
