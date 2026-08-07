+++
title = "Providers"
weight = 7
[extra]
group = "Reference"
+++

# Providers

Maki talks to LLM providers over their HTTP APIs. Models are split into three tiers: **weak** (cheap and fast), **medium** (balanced), and **strong** (highest capability, highest cost). There is also a **compaction** tier for choosing a dedicated model to summarize context when the conversation grows long.

Open the model picker with `/model` and press `!`, `@`, `#`, or `$` on any row to assign it to strong, medium, weak, or compaction. Press the same key again to remove the assignment. Your overrides are saved to `~/.local/state/maki/model-tiers` and apply across sessions.

## Auth Reloading

Maki re-reads auth from storage and environment variables each time a new agent spawns (`/new`, retry, session load). If you run `maki auth login` in another terminal or change an env var, the next session picks it up without a restart.

You can set multiple API keys in one env var (`ANTHROPIC_API_KEY=sk-1,sk-2,sk-3`) and they rotate automatically on rate-limit or auth errors.

## Base URL Overrides

Every provider honors a `<SLUG>_BASE_URL` env var (`anthropic` -> `ANTHROPIC_BASE_URL`, `llama-cpp` -> `LLAMA_CPP_BASE_URL`). Set it to the origin of a proxy or a compatible endpoint and Maki appends the API paths itself:

```sh
ANTHROPIC_BASE_URL=https://my-proxy.internal maki
```

It wins over `providers.toml` and built-in defaults. `ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL` are the same names the official SDKs use, so an existing proxy setup carries over as is. One exception: `OPENAI_BASE_URL` only redirects the platform API, never the ChatGPT Coding Plan backend.

You can also set `base_url` for a built-in provider in `~/.config/maki/providers.toml`. It overrides the built-in default and loses to the env var above:

```toml
[openai]
base_url = "http://xxxx:1234/v1"
```

The built-in provider still owns the slug, so `protocol`, `api_key_env`, `discover_models` and `models` are ignored with a warning. Use a custom slug if you need those.

## Built-in Providers

### Anthropic

- **Env var**: `ANTHROPIC_API_KEY`
- **API**: `https://api.anthropic.com/v1/messages`
- **Features**: Prompt caching, thinking mode (adaptive/budgeted), advanced tool use

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **claude-haiku-4-5** (default) | $1.00 / $5.00 | 200K ctx / 64K out |
| Medium | claude-sonnet-4-5 | $3.00 / $15.00 | 200K ctx / 64K out |
| Medium | claude-sonnet-4-6 | $3.00 / $15.00 | 200K ctx / 64K out |
| Medium | **claude-sonnet-5** (default) | $2.00 / $10.00 | 200K ctx / 128K out |
| Medium | claude-sonnet-4 | $3.00 / $15.00 | 200K ctx / 64K out |
| Strong | claude-opus-4-5 | $5.00 / $25.00 | 200K ctx / 64K out |
| Strong | claude-opus-4-6 | $5.00 / $25.00 | 200K ctx / 128K out |
| Strong | claude-opus-4-7 | $5.00 / $25.00 | 200K ctx / 128K out |
| Strong | claude-opus-4-8 | $5.00 / $25.00 | 200K ctx / 128K out |
| Strong | **claude-opus-5** (default) | $5.00 / $25.00 | 200K ctx / 128K out |
| Strong | claude-fable-5 | $10.00 / $50.00 | 200K ctx / 128K out |
| Strong | claude-opus-4-0, claude-opus-4-1 | $15.00 / $75.00 | 200K ctx / 32K out |

Defaults: claude-haiku-4-5 (weak), claude-sonnet-5 (medium), claude-opus-5 (strong)

Add `-1m` to any Claude model, like `claude-sonnet-4-6-1m`, to use the 1M token context window.

#### Amazon Bedrock

If you already use Claude through AWS Bedrock, you can point Maki at it instead of the direct Anthropic API. Set `CLAUDE_CODE_USE_BEDROCK=1` and Maki will route all Anthropic requests through Bedrock. The same models, the same features, just a different door.

You will need `AWS_REGION` and one of the following for auth:

| Method | Env vars |
|--------|----------|
| IAM credentials | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`) |
| Credentials file | `AWS_PROFILE` (defaults to `default`), reads `~/.aws/credentials` |
| Bearer token | `AWS_BEARER_TOKEN_BEDROCK` |
| Gateway proxy | `CLAUDE_CODE_SKIP_BEDROCK_AUTH=1` + `ANTHROPIC_BEDROCK_BASE_URL` (skips signing, useful behind a proxy that handles auth) |

You can override the model with `ANTHROPIC_MODEL` and the endpoint with `ANTHROPIC_BEDROCK_BASE_URL`. These env var names match Claude Code, so if you were already using Bedrock there, the same setup works here.

### Copilot

- **Env var**: `GH_COPILOT_TOKEN` (or run `maki auth login copilot` to import a token from gh)
- **API**: `https://api.githubcopilot.com (or GraphQL-discovered Copilot API endpoint)`
- **Features**: Native Copilot Chat HTTP API with model endpoint discovery

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gpt-5-mini, gpt-5 mini, claude-haiku-4.5** (default) | $0.00 / $0.00 | 200K ctx / 100K out |
| Medium | **gpt-5.2, gpt-4.1, claude-sonnet-4.5** (default) | $0.00 / $0.00 | 200K ctx / 100K out |
| Strong | **gpt-5.4, gpt-5.3-codex, claude-opus-4.6, grok-code-fast-1** (default) | $0.00 / $0.00 | 200K ctx / 100K out |
| Strong | claude-opus-4.7 | $0.00 / $0.00 | 264K ctx / 64K out |

Defaults: gpt-5-mini (weak), gpt-5.2 (medium), gpt-5.4 (strong)

### DeepSeek

- **Env var**: `DEEPSEEK_API_KEY`
- **API**: `https://api.deepseek.com`
- **Features**: Thinking mode toggle (on/off), open-weight models

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Medium | **deepseek-v4-flash** (default) | $0.14 / $0.28 | 1000K ctx / 384K out |
| Strong | **deepseek-v4-pro** (default) | $0.43 / $0.87 | 1000K ctx / 384K out |

Defaults: deepseek-v4-flash (medium), deepseek-v4-pro (strong)

### Google

- **Env var**: `GEMINI_API_KEY`
- **API**: `https://generativelanguage.googleapis.com/v1beta`
- **Features**: Native Gemini API with thinking support

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gemini-2.0-flash-lite** (default) | $0.07 / $0.30 | 1048K ctx / 65K out |
| Medium | **gemini-2.5-flash** (default) | $0.15 / $0.60 | 1048K ctx / 65K out |
| Strong | **gemini-2.5-pro** (default) | $1.25 / $5.00 | 1048K ctx / 65K out |

Defaults: gemini-2.5-pro (strong), gemini-2.5-flash (medium), gemini-2.0-flash-lite (weak)

### LlamaCpp

- **Env var**: `LLAMA_CPP_API_KEY`
- **API**: `http://localhost:8080/v1`
- **Features**: Local or remote inference via LLAMA_CPP_HOST, set optional key via LLAMA_CPP_API_KEY

Connects to any OpenAI-compatible `/v1` endpoint. Point `LLAMA_CPP_HOST` to your server address (defaults to `http://localhost:8080`).

### Mistral

- **Env var**: `MISTRAL_API_KEY`
- **API**: `https://api.mistral.ai/v1`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **ministral-14b-latest, ministral-14b-2512** (default) | $0.20 / $0.20 | 262K ctx |
| Medium | **mistral-small-latest, mistral-small-2603** (default) | $0.15 / $0.60 | 262K ctx |
| Strong | **mistral-medium-latest, mistral-medium-3.5, mistral-medium-2604** (default) | $1.50 / $7.50 | 262K ctx |

Defaults: mistral-medium-latest (strong), mistral-small-latest (medium), ministral-14b-latest (weak)

### Ollama

- **Env var**: `OLLAMA_HOST` for local/remote (e.g. `http://localhost:11434`), `OLLAMA_API_KEY` for auth
- **API**: `http://localhost:11434/v1`
- **Features**: Local or remote inference via OLLAMA_HOST, cloud fallback via OLLAMA_API_KEY

This provider talks the OpenAI-compatible `/v1` API, so it also works with llama.cpp's server, LocalAI, or anything else that speaks the same protocol. Just point `OLLAMA_HOST` to the right address (e.g. `http://localhost:8080` for llama.cpp).

### OpenAI

- **Env var**: `OPENAI_API_KEY` (also supports OAuth device flow)
- **API**: `https://api.openai.com/v1`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gpt-5.6-luna** (default) | $1.00 / $6.00 | 372K ctx / 128K out |
| Weak | gpt-5.4-nano | $0.20 / $1.25 | 400K ctx / 128K out |
| Weak | gpt-5.4-mini | $0.75 / $4.50 | 400K ctx / 128K out |
| Weak | gpt-4.1-nano | $0.10 / $0.40 | 1047K ctx / 32K out |
| Medium | **gpt-5.6-terra** (default) | $2.50 / $15.00 | 372K ctx / 128K out |
| Medium | gpt-4.1-mini | $0.40 / $1.60 | 1047K ctx / 32K out |
| Medium | gpt-4.1 | $2.00 / $8.00 | 1047K ctx / 32K out |
| Medium | o4-mini | $1.10 / $4.40 | 200K ctx / 100K out |
| Medium | gpt-5.1-codex-mini | $0.25 / $2.00 | 400K ctx / 128K out |
| Strong | **gpt-5.6-sol** (default) | $5.00 / $30.00 | 372K ctx / 128K out |
| Strong | gpt-5.5 | $5.00 / $30.00 | 1050K ctx / 128K out |
| Strong | gpt-5.4 | $2.50 / $15.00 | 1050K ctx / 128K out |
| Strong | o3 | $2.00 / $8.00 | 200K ctx / 100K out |
| Strong | gpt-5.3-codex | $1.75 / $14.00 | 400K ctx / 128K out |
| Strong | gpt-5.2-codex | $1.75 / $14.00 | 400K ctx / 128K out |
| Strong | gpt-5.1-codex-max | $1.25 / $10.00 | 400K ctx / 128K out |
| Strong | gpt-5.1-codex | $1.25 / $10.00 | 400K ctx / 128K out |

Defaults: gpt-5.6-luna (weak), gpt-5.6-terra (medium), gpt-5.6-sol (strong)

### Opencode

- **Env var**: `OPENCODE_API_KEY`
- **API**: `https://opencode.ai/zen/v1`
- **Features**: Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Zen API

No hardcoded model catalog. Use any model ID supported by this provider.

By default Maki hides free models from the Opencode catalog. To list free models (they use a public fallback, no API key needed), add this to `~/.config/maki/providers.toml`:

```toml
[opencode]
enable_free_models = true
```

The default is `false`.

### Opencode Go

- **Env var**: `OPENCODE_API_KEY`
- **API**: `https://opencode.ai/zen/go/v1`
- **Features**: Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Go API

No hardcoded model catalog. Use any model ID supported by this provider.

### OpenRouter

- **Env var**: `OPENROUTER_API_KEY`
- **API**: `https://openrouter.ai/api/v1`
- **Features**: 300+ models from all providers, prompt caching, provider routing

OpenRouter aggregates models from many providers behind a single API key. Browse available models at [openrouter.ai/models](https://openrouter.ai/models). Use any model ID directly (e.g. `openrouter/anthropic/claude-sonnet-4`).

### Synthetic

- **Env var**: `SYNTHETIC_API_KEY`
- **API**: `https://api.synthetic.new/openai/v1`
- **Features**: Reasoning effort support (low/medium/high), open-weight models

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **hf:zai-org/GLM-4.7-Flash** (default) | $0.10 / $0.50 | 200K ctx / 131K out |
| Medium | **hf:deepseek-ai/DeepSeek-V3.2** (default) | $0.56 / $1.68 | 200K ctx / 131K out |
| Strong | **hf:moonshotai/Kimi-K2.5** (default) | $0.45 / $3.40 | 200K ctx / 131K out |

Defaults: hf:moonshotai/Kimi-K2.5 (strong), hf:deepseek-ai/DeepSeek-V3.2 (medium), hf:zai-org/GLM-4.7-Flash (weak)

### TensorX

- **Env var**: `TENSORX_API_KEY`
- **API**: `https://api.tensorx.ai/v1`
- **Features**: Open-weight models, zero data retention, prompt caching

No hardcoded model catalog. Use any model ID supported by this provider.

### Z.AI

- **Env var**: `ZHIPU_API_KEY` (shared across both endpoints)
- **API endpoints**:
  - `https://api.z.ai/api/paas/v4`
  - `https://api.z.ai/api/coding/paas/v4`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **glm-4.7-flash** (default) | $0.00 / $0.00 | 200K ctx / 131K out |
| Weak | glm-4.5-flash | $0.00 / $0.00 | 131K ctx / 98K out |
| Weak | glm-4.5-air | $0.20 / $1.10 | 131K ctx / 98K out |
| Medium | **glm-4.7, glm-4.6** (default) | $0.60 / $2.20 | 200K ctx / 131K out |
| Medium | glm-4.5 | $0.60 / $2.20 | 131K ctx / 98K out |
| Strong | **glm-5-code** (default) | $1.20 / $5.00 | 200K ctx / 131K out |
| Strong | glm-5.2 | $1.00 / $3.20 | 1000K ctx / 131K out |
| Strong | glm-5.1, glm-5 | $1.00 / $3.20 | 200K ctx / 131K out |

Defaults: glm-5-code (strong), glm-4.7-flash (weak), glm-4.7 (medium)

### Opencode Go

- **Env var**: `OPENCODE_API_KEY`
- **API**: `https://opencode.ai/zen/go/v1`
- **Features**: Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Go API

No hardcoded model catalog. Use any model ID supported by this provider. An API key is required.


## Model Identifiers

Models are referenced as `provider/model_id`:

```
anthropic/claude-sonnet-4-6
openai/gpt-4.1
zai/glm-4.7
```

If the model name is unique across providers, the prefix can be omitted.

## providers.toml

`providers.toml` lives in the config directory (`~/.config/maki/providers.toml` on Linux/macOS, `%APPDATA%\maki\providers.toml` on Windows). It is the file for provider overrides and custom HTTP providers. Two jobs:

1. Tweak a built-in (pick a plan, change its base URL, set `enable_free_models` for Opencode).
2. Declare a custom provider that speaks OpenAI, Anthropic, or Google wire format.

```toml
# Point a built-in at a proxy. Env vars still win over this file.
[anthropic]
base_url = "https://my-proxy.internal"

# Full custom provider. Slug becomes the `provider/` prefix in model specs.
[my-proxy]
display_name = "My Proxy"
protocol = "openai"            # openai | openai-responses | anthropic | google
base_url = "https://llm.example.com/v1"
api_key_env = "MY_PROXY_API_KEY"
default_model = "my-proxy/fast-v1"
discover_models = true         # also list models via the provider's /models endpoint

[[my-proxy.models]]
id = "fast-v1"
tier = "weak"
context_window = 128000
max_output_tokens = 16384
pricing_input = 0.5
pricing_output = 1.5

[[my-proxy.models]]
id = "smart-v1"
tier = "strong"
context_window = 200000
max_output_tokens = 32000
supports_thinking = true
supports_vision = false
```

### Provider fields

| Field | Type | Notes |
|-------|------|-------|
| `display_name` | string | Shown in pickers and auth status |
| `protocol` | string | `openai`, `openai-responses`, `anthropic`, or `google`. Required for custom slugs |
| `base_url` | string | Origin of the API. Maki appends the protocol paths |
| `plan` | string | Built-in plan key (see Plans below). Sets base URL and default model |
| `api_key_env` | string | Env var that holds the key. Defaults to `<SLUG>_API_KEY` |
| `api_key` | string | Inline key (prefer the env var or `maki auth login`) |
| `default_model` | string | Used after login when no model is saved yet |
| `discover_models` | bool | When true, also probe the provider's model list endpoint (default false) |
| `enable_free_models` | bool | Opencode only. Show free catalog models (default false) |
| `models` | array | Declared models for custom providers (see below) |

### Model fields

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `id` | string | required | Model id. Spec becomes `{slug}/{id}` |
| `tier` | string | `medium` | `weak`, `medium`, `strong`, or `compaction` |
| `context_window` | u32 | protocol default | Tokens of context |
| `max_output_tokens` | u32 | protocol default | Max completion tokens |
| `supports_tool_examples` | bool | protocol default | |
| `supports_thinking` | bool | protocol default | |
| `supports_vision` | bool | protocol default | When false, image input and `view_image` are off |
| `pricing_input` / `pricing_output` | f64 | 0 | USD per 1M tokens |
| `pricing_cache_write` / `pricing_cache_read` | f64 | 0 | USD per 1M tokens |
| `pricing_fast_input` / `pricing_fast_output` | f64 | unset | Fast-mode pricing when the provider supports it |

Custom slugs must not reuse a built-in provider name. A bad TOML parse exits with code 2 at startup so a typo cannot silently empty the registry.

You can also create a custom provider interactively with `maki auth login` and choosing the custom option. That writes a starter entry to this file.

### Plans

Some built-ins ship multiple plans (different base URLs or default models). `maki auth login <provider>` asks which plan to use when more than one exists. You can also set it in TOML:

```toml
[mistral]
plan = "coding"

[zai]
plan = "coding"
```

Current plans:

| Provider | Plan | What it does |
|----------|------|--------------|
| Mistral | `standard` | Standard at `https://api.mistral.ai/v1`, default `mistral/mistral-medium-latest` |
| Mistral | `coding` | Vibe / Coding at `https://api.mistral.ai/v1`, default `mistral/mistral-vibe-cli-latest` |
| Z.AI | `standard` | Pay-as-you-go at `https://api.z.ai/api/paas/v4`, default `zai/glm-5.1` |
| Z.AI | `coding` | Coding plan at `https://api.z.ai/api/coding/paas/v4`, default `zai/glm-5-code` |

Env `<SLUG>_BASE_URL` still wins over both the plan and a `base_url` in this file.

## Dynamic Providers

To add a custom provider or proxy, drop an executable script into the config `providers/` directory (`~/.config/maki/providers/` on Linux/macOS, `%APPDATA%\maki\providers\` on Windows). The script must handle these subcommands:

| Subcommand | Timeout | What it does |
|------------|---------|--------|
| `info` | 5s | Return JSON with `display_name`, `base` provider, `has_auth` |
| `models` | 5s | Return JSON array of model entries (optional) |
| `resolve` | 30s | Return auth JSON (`base_url`, `headers`) |
| `login` | interactive | OAuth or credential flow |
| `logout` | interactive | Clear credentials |
| `refresh` | 30s | Refresh auth tokens |

`resolve` is called each time a new agent spawns, so scripts should read tokens from disk instead of caching them in memory. That way auth changes from other processes get picked up.

The `base` field specifies which built-in provider to inherit the model catalog from. Valid values: `anthropic`, `copilot`, `google`, `llama-cpp`, `mistral`, `ollama`, `openai`, `opencode`, `opencode-go`, `openrouter`, `synthetic`, `tensorx`, `zai`.

If your provider serves models not in the base catalog, add a `models` subcommand returning:

```json
[{"id": "my-model-v2", "tier": "strong", "context_window": 200000, "max_output_tokens": 16384}]
```

Only `id` is required. Optional fields: `tier` (default `medium`), `context_window` (128K), `max_output_tokens` (16K), `pricing` (`{input, output, cache_write, cache_read}`, all per 1M tokens), `supports_tool_examples` (defaults to the base provider's setting), `supports_thinking` (defaults to the base provider's setting), `supports_vision` (defaults to the base provider's setting; when false, image input and the `view_image` tool are disabled). The first model listed per tier is used for sub-agents. Without this subcommand, the base provider's models are used.

Dynamic provider models are namespaced as `{slug}/{model_id}` (e.g. `myproxy/claude-sonnet-4-6`).

### Script Name Rules

- Must start with a letter or digit
- Only letters, digits, underscores, and hyphens after that
- Can't reuse a built-in provider's slug
- Must be executable

## Lua Providers

A provider can be defined entirely in Lua, without Rust. Today this is how DeepSeek is configured, and other OpenAI-compatible providers can follow. The definition lives in Lua; the actual HTTP codec stays in Rust, so you inherit request streaming, SSE parsing, and tool formatting for free.

Call `maki.api.register_provider(spec)` in a bundled provider file. The spec is a table:

| Field | Type | Notes |
|-------|------|-------|
| `slug` | string | Required. Identifies the provider (e.g. `deepseek`). |
| `display_name` | string | Shown in the model picker. |
| `family` | string | `"generic"`, or a known family name. |
| `codec` | string or table | `"openai"` is the only supported codec in this version. A table form lets a future codec take options. |
| `base_url` | string | The API root. `<SLUG>_BASE_URL` overrides it. |
| `auth` | table | See the auth table below. |
| `supports_thinking` | boolean | Whether the model exposes a thinking mode. |
| `context_window`, `max_output_tokens` | integer | Fallback sizes. |
| `effort` | table | `{ supported = { ... } }`. When set, the codec applies `reasoning_effort` from the thinking config itself. |
| `models` | table | Array of model entries (prefixes, tier, default, vision, pricing, sizes). |
| `on_request`, `on_usage`, `usage` | function | Hooks. See below. |

Unknown keys are an error. A typo in a pricing field would silently mis-cost every request, so the spec is strict. `models` (as a hook that lists models) and `on_error` are deliberately unsupported in this version.

### The auth table

```lua
auth = { kind = "api_key", env = "DEEPSEEK_API_KEY",
         login_url = "https://platform.deepseek.com/api_keys", needs_url = false }
```

`kind = "api_key"` reads the key from `env`, or from stored credentials after `maki auth login <slug>`. `login_url` is printed during login. Only the `api_key` kind ships in this version.

### Hooks

Hooks shape requests and map responses without moving the codec into Lua. They are optional.

- `on_request(body, ctx)`: mutate the request body before it is sent. `body` is a handle, not a Lua table (see the marshalling rules below).
- `on_usage(raw)`: map the raw usage `Value` from the stream to a token-usage table. Wired for the `openai` codec only. Declaring `on_usage` with any other codec is rejected at registration.
- `usage(ctx)`: fetch provider-side quota (balance, limits) via `ctx:request(path)`.

A dead Lua host is an error, never a skipped hook. If the host that registered the provider is gone, the request fails instead of silently sending an unshaped body.

### Marshalling: handles, not tables

The request body is shared between Lua and the Rust codec as JSON. To avoid scrambling key order on the wire, hooks never receive a converted Lua table. They receive a handle.

```lua
on_request = function(body, ctx)
  body:set("thinking", { type = "enabled" })   -- a small literal, converted in
  local n = body:get("max_tokens")              -- a scalar, converted out
  if body:has("tools") then ... end             -- no conversion, just a check
end
```

Two handle types back every container:

| Method | What it does |
|--------|--------------|
| `obj:get(key)` | Scalar values come back as Lua values. Containers come back as nested handles. |
| `obj:set(key, value)` | A Lua value is written into the JSON tree. |
| `obj:has(key)` | True if the key exists. Performs no conversion. An assistant turn's `reasoning_content` can run to thousands of tokens; use `has` instead of `get` to check for it. |
| `arr:len()` | Number of elements. Also `#arr`. |
| `arr:get(i)` | The element at 1-based index `i`, as a nested handle. |

Mutations survive into the serialized body. The body, `ctx.messages`, `ctx.tools`, and `ctx.headers` all reach the same shared tree through handles.

### ctx

```lua
ctx.model        -- { id, provider, tier, family, max_output_tokens, context_window,
                 --   supports_thinking, supports_vision, capabilities }
ctx.thinking     -- { enabled, mode, effort, budget }
ctx.session_id   -- string or nil
ctx.messages     -- JsonArray (the body's messages)
ctx.tools        -- JsonArray (the body's tools)
ctx.headers      -- JsonObject (extra request headers)
ctx:request(path, opts)
```

`ctx:request(path)` does an authenticated HTTP request against the provider's `base_url`. The path is relative (starts with `/`). Auth is the provider's resolved headers. Redirects are disabled, so `Authorization` and other headers never leak to a different host. The SSRF guard is on, waived only when `base_url` itself points at a loopback or private address (for local servers like ollama). Errors redact the URL and cap the response body.

Every handle and `ctx` carries a generation counter. Once `on_request` returns, stashed handles stop working. A provider file cannot save `ctx` and reuse it later, which closes a prompt-injection path where a model could reach an authenticated fetch.

### Loading and rollback

Bundled providers load through `plugins/providers/init.lua`, which wraps each `require` in `maki.api.provider_scope`:

```lua
local ok, err = maki.api.provider_scope("deepseek", function() require("deepseek") end)
```

A file that registers a spec and then throws leaves nothing behind: `provider_scope` rolls back to the state before the chunk ran. A bare `pcall` would isolate the error but keep a half-configured spec installed.

Lua providers are bundled-only in this version. User plugins and `<cwd>/.maki/init.lua` run without the `net` permission, and `register_provider` requires it, so a repo-supplied provider cannot intercept the API key, the conversation, or network egress.

