//! Goldens and semantics for Lua-registered providers, booted through the
//! real plugin host so `on_request` / `on_usage` dispatch on the runtime
//! thread exactly as the agent sees them. Goldens assert parsed `Value`s
//! (order-independent) so a codec or hook drift fails loudly.

use std::sync::{Arc, Mutex};

use maki_agent::tools::ToolRegistry;
use maki_lua::PluginHost;
use maki_providers::{
    ContentBlock, Message, Model, ProviderSpec, Role, ThinkingConfig, TokenUsage, build_lua_body,
    registry,
};
use serde_json::{Value, json};

const SYSTEM: &str = "";
const USER_TEXT: &str = "hi";
const ASSISTANT_TEXT: &str = "ok";
const MAX_OUTPUT: u32 = 384_000;

/// Deepseek is a process-global registry entry whose `hooks` capture the host
/// that registered it. The deepseek tests each boot their own host, so they
/// serialize against each other to keep the live spec pointing at the live
/// host while `build_lua_body` / `on_usage` dispatch.
static DEEPSEEK_LOCK: Mutex<()> = Mutex::new(());

fn host() -> PluginHost {
    let reg = Arc::new(ToolRegistry::new());
    PluginHost::new(reg).expect("host boots")
}

fn host_with_providers() -> PluginHost {
    let host = host();
    host.load_bundled_providers().expect("providers load");
    host
}

fn deepseek_spec() -> Arc<ProviderSpec> {
    registry::get("deepseek").expect("deepseek registered")
}

fn conversation() -> Vec<Message> {
    vec![
        Message::user(USER_TEXT.into()),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: ASSISTANT_TEXT.into(),
            }],
            ..Default::default()
        },
    ]
}

fn tools_empty() -> Value {
    json!([])
}

fn run_body(spec: &Arc<ProviderSpec>, model: &Model, thinking: ThinkingConfig) -> Value {
    smol::block_on(build_lua_body(
        spec,
        model,
        &conversation(),
        SYSTEM,
        &tools_empty(),
        thinking,
        None,
    ))
    .expect("build_lua_body")
}

fn assistant_msg(with_reasoning: bool) -> Value {
    let mut m = json!({"role": "assistant", "content": ASSISTANT_TEXT});
    if with_reasoning {
        m["reasoning_content"] = json!("");
    }
    m
}

/// Base openai-compat body the codec emits for this conversation, before the
/// deepseek hook adds `thinking` (and the codec adds `reasoning_effort`).
fn base_body(model: &str, assistant: Value) -> Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": USER_TEXT},
            assistant,
        ],
        "stream": true,
        "max_tokens": MAX_OUTPUT,
        "stream_options": {"include_usage": true},
    })
}

#[test]
fn golden_thinking_off_disables_and_skips_effort() {
    let _g = DEEPSEEK_LOCK.lock().unwrap();
    let _host = host_with_providers();
    let spec = deepseek_spec();
    let model = Model::from_spec("deepseek/deepseek-v4-flash").expect("model");
    let body = run_body(&spec, &model, ThinkingConfig::Off);

    let mut expected = base_body("deepseek-v4-flash", assistant_msg(false));
    expected["thinking"] = json!({"type": "disabled"});
    assert_eq!(body, expected);
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn golden_thinking_on_v4_backfills_reasoning_content() {
    let _g = DEEPSEEK_LOCK.lock().unwrap();
    let _host = host_with_providers();
    let spec = deepseek_spec();
    let model = Model::from_spec("deepseek/deepseek-v4-flash").expect("model");
    // Adaptive sends no reasoning_effort (deepseek declares no adaptive mapping);
    // the V4-specific behavior under test is the reasoning_content backfill.
    let body = run_body(&spec, &model, ThinkingConfig::Adaptive);

    let mut expected = base_body("deepseek-v4-flash", assistant_msg(true));
    expected["thinking"] = json!({"type": "enabled"});
    assert_eq!(body, expected);
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn golden_thinking_on_non_v4_skips_backfill() {
    let _g = DEEPSEEK_LOCK.lock().unwrap();
    let _host = host_with_providers();
    let spec = deepseek_spec();
    // "deepseek-r1" matches no registered prefix and lacks the V4 substring,
    // so the hook sets thinking=enabled then returns without backfilling.
    let model = Model::from_spec("deepseek/deepseek-r1").expect("model");
    let body = run_body(&spec, &model, ThinkingConfig::Adaptive);

    let mut expected = base_body("deepseek-r1", assistant_msg(false));
    expected["thinking"] = json!({"type": "enabled"});
    assert_eq!(body, expected);
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn golden_budget_snaps_to_max_only_level() {
    let _g = DEEPSEEK_LOCK.lock().unwrap();
    let _host = host_with_providers();
    let spec = deepseek_spec();
    let model = Model::from_spec("deepseek/deepseek-v4-flash").expect("model");
    let body = run_body(&spec, &model, ThinkingConfig::Budget(1_000));

    let mut expected = base_body("deepseek-v4-flash", assistant_msg(true));
    expected["thinking"] = json!({"type": "enabled"});
    expected["reasoning_effort"] = json!("max");
    assert_eq!(
        body, expected,
        "Budget must snap to the only supported level"
    );
}

fn run_on_usage(raw: Value) -> TokenUsage {
    let _g = DEEPSEEK_LOCK.lock().unwrap();
    let _host = host_with_providers();
    let spec = deepseek_spec();
    let hooks = spec.hooks.as_ref().expect("hooks present");
    let fut = hooks.on_usage(&raw).expect("has on_usage");
    smol::block_on(fut).expect("on_usage succeeds")
}

#[test]
fn on_usage_prompt_cache_hit_tokens_only() {
    let raw = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "prompt_cache_hit_tokens": 30,
    });
    let usage = run_on_usage(raw);
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_creation
        ),
        (70, 50, 30, 0),
    );
}

#[test]
fn on_usage_prompt_tokens_details_only() {
    let raw = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "prompt_tokens_details": {"cached_tokens": 30},
    });
    let usage = run_on_usage(raw);
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_creation
        ),
        (70, 50, 30, 0),
    );
}

#[test]
fn on_usage_both_sources_max_wins() {
    // prompt_cache_hit_tokens (40) beats prompt_tokens_details.cached_tokens (20).
    let raw = json!({
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "prompt_cache_hit_tokens": 40,
        "prompt_tokens_details": {"cached_tokens": 20},
    });
    let usage = run_on_usage(raw);
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_creation
        ),
        (60, 50, 40, 0),
    );
}

#[test]
fn on_usage_neither_source_reports_no_cache() {
    let raw = json!({"prompt_tokens": 100, "completion_tokens": 50});
    let usage = run_on_usage(raw);
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_creation
        ),
        (100, 50, 0, 0),
    );
}

/// Body/ctx handle semantics: nested `get` returns a handle, `has` performs no
/// conversion, `set` round-trips, and mutations survive into the serialised
/// body. Assertion failures inside `on_request` surface as `build_lua_body`
/// errors, so a clean return proves every `assert` held.
#[test]
fn handle_semantics_hold_through_on_request() {
    let host = host();
    host.load_source(
        "handle-semantics",
        r#"
        maki.api.register_provider({
          slug = "handle-test",
          display_name = "Handle Test",
          family = "generic",
          codec = "openai",
          base_url = "https://example.invalid",
          auth = { kind = "api_key", env = "HANDLE_TEST_KEY" },
          models = { { prefixes = { "handle-test-m" }, tier = "medium", default = true } },
          on_request = function(body, ctx)
            -- nested get returns a handle, not a converted value
            local msg = ctx.messages:get(1)
            assert(msg ~= nil, "messages:get(1) returns a handle")
            -- leaf get converts to a Lua value
            local role = msg:get("role")
            assert(role == "system", "leaf get converts: " .. tostring(role))
            -- has returns a boolean, never the value
            assert(msg:has("role") == true, "has returns true for present key")
            assert(msg:has("missing_key_xyz") == false, "has returns false for absent key")
            -- set round-trips on an existing leaf
            msg:set("content", "patched")
            assert(msg:get("content") == "patched", "set round-trips")
            -- set inserts a missing key (deepseek relies on this for `thinking`)
            body:set("custom_marker", "yes")
            assert(body:get("custom_marker") == "yes", "body set inserts and round-trips")
            -- a mutation the Rust side can observe in the serialised body
            body:set("stream", false)
          end,
        })
        "#,
    )
    .expect("register handle-test");

    let spec = registry::get("handle-test").expect("handle-test registered");
    let model = Model::from_spec("handle-test/handle-test-m").expect("model");
    let body = run_body(&spec, &model, ThinkingConfig::Off);

    assert_eq!(
        body["stream"],
        json!(false),
        "mutation survives into wire body"
    );
    assert_eq!(body["custom_marker"], json!("yes"), "inserted key survives");
}

/// A handle stashed during `on_request` errors once the hook returns: the gen
/// counter bumps on return, so any later touch is rejected (§5.3 — a stashed
/// handle can never reach the next request's body). The first call stashes;
/// the second call touches the stale handle and asserts it errored.
#[test]
fn stashed_handle_expires_after_hook_returns() {
    let host = host();
    host.load_source(
        "gen-expiry",
        r#"
        local stashed
        maki.api.register_provider({
          slug = "gen-test",
          display_name = "Gen Test",
          family = "generic",
          codec = "openai",
          base_url = "https://example.invalid",
          auth = { kind = "api_key", env = "GEN_TEST_KEY" },
          models = { { prefixes = { "gen-test-m" }, tier = "medium", default = true } },
          on_request = function(body, _ctx)
            if stashed then
              local ok, err = pcall(function()
                return stashed:get("model")
              end)
              assert(not ok, "stashed handle must error after the hook returns")
              assert(tostring(err):find("expired"),
                     "error must mention expiry: " .. tostring(err))
              return
            end
            stashed = body
          end,
        })
        "#,
    )
    .expect("register gen-test");

    let spec = registry::get("gen-test").expect("gen-test registered");
    let model = Model::from_spec("gen-test/gen-test-m").expect("model");
    let _ = run_body(&spec, &model, ThinkingConfig::Off);
    // Second call: `stashed` is now stale (gen bumped after the first return).
    let _ = run_body(&spec, &model, ThinkingConfig::Off);
}

/// `provider_scope` rolls back: a chunk that registers then throws leaves
/// nothing installed (registry and hook store both clean).
#[test]
fn provider_scope_rolls_back_partial_registration() {
    let host = host();
    host.load_source(
        "rollback",
        r#"
        local ok, err = maki.api.provider_scope("rb", function()
          maki.api.register_provider({
            slug = "rb-test",
            display_name = "RB",
            family = "generic",
            codec = "openai",
            base_url = "https://example.invalid",
            auth = { kind = "api_key", env = "RB_KEY" },
            models = { { prefixes = { "rb-m" }, tier = "medium", default = true } },
          })
          error("boom")
        end)
        assert(not ok, "provider_scope reports the throw as failure")
        assert(tostring(err):find("boom"), "error surfaces the message: " .. tostring(err))
        "#,
    )
    .expect("rollback chunk runs");

    assert!(
        registry::get("rb-test").is_none(),
        "rolled-back spec is gone"
    );
}

/// Unknown top-level keys are rejected so a typo (e.g. `pricing_input`) cannot
/// silently mis-cost every request; `models`/`on_error` are honestly rejected
/// rather than ignored.
#[test]
fn unknown_keys_are_rejected() {
    let host = host();
    host.load_source(
        "unknown-keys",
        r#"
        local function rejects(spec, needle)
          local ok, err = pcall(maki.api.register_provider, spec)
          assert(not ok, "should reject: " .. tostring(needle))
          assert(tostring(err):find(needle),
                 "error should mention " .. needle .. ": " .. tostring(err))
        end

        rejects({
          slug = "bad-pricing", codec = "openai",
          auth = { kind = "api_key", env = "X" }, models = {},
          pricing_input = 0.5,
        }, "unknown key")

        rejects({
          slug = "bad-on-error", codec = "openai",
          auth = { kind = "api_key", env = "X" }, models = {},
          on_error = function() end,
        }, "unknown key")
        "#,
    )
    .expect("unknown keys rejected");
}

/// `models` as a function is not supported yet and must be rejected explicitly
/// (a silent ignore would let a plugin believe its dynamic model list works).
#[test]
fn models_as_function_is_rejected() {
    let host = host();
    host.load_source(
        "models-fn",
        r#"
        local ok, err = pcall(maki.api.register_provider, {
          slug = "bad-models", codec = "openai",
          auth = { kind = "api_key", env = "X" },
          models = function() return {} end,
        })
        assert(not ok, "models-as-function must reject")
        assert(tostring(err):find("models"), "error must mention models: " .. tostring(err))
        "#,
    )
    .expect("models fn rejected");
}

/// Only the openai codec ships today; any other value is rejected at
/// registration so a half-built provider never reaches the agent.
#[test]
fn non_openai_codec_is_rejected() {
    let host = host();
    host.load_source(
        "bad-codec",
        r#"
        local ok, err = pcall(maki.api.register_provider, {
          slug = "bad-codec", codec = "anthropic",
          auth = { kind = "api_key", env = "X" }, models = {},
        })
        assert(not ok, "non-openai codec must reject")
        assert(tostring(err):find("codec"), "error must mention codec: " .. tostring(err))
        "#,
    )
    .expect("non-openai codec rejected");
}
