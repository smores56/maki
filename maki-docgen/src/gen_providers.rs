use std::fmt::Write;
use std::str::FromStr;
use std::sync::Arc;

use maki_agent::tools::ToolRegistry;
use maki_lua::PluginHost;
use maki_providers::manifest::{ManifestRegistry, ProviderManifest};
use maki_providers::manifest_provider::{self, EngineSpec};
use maki_providers::model::{ModelEntry, ModelTier};
use maki_providers::provider::ProviderKind;
use strum::IntoEnumIterator;

const FRONT_MATTER: &str = r#"+++
title = "Providers"
weight = 5
[extra]
group = "Reference"
+++"#;

const TIER_PICKER_NOTE: &str = r#"Open the model picker with `/model` and press `!`, `@`, `#`, or `$` on any row to assign it to strong, medium, weak, or compaction. Press the same key again to remove the assignment. Your overrides are saved to `~/.local/state/maki/model-tiers` and apply across sessions."#;

const AUTH_RELOADING: &str = r#"## Auth Reloading

Maki re-reads auth from storage and environment variables each time a new agent spawns (`/new`, retry, session load). If you run `maki auth login` in another terminal or change an env var, the next session picks it up without a restart.

You can set multiple API keys in one env var (`ANTHROPIC_API_KEY=sk-1,sk-2,sk-3`) and they rotate automatically on rate-limit or auth errors."#;

const BASE_URL_OVERRIDES: &str = r#"## Base URL Overrides

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

The built-in provider still owns the slug, so `protocol`, `api_key_env`, `discover_models` and `models` are ignored with a warning. Use a custom slug if you need those."#;

const LONG_CONTEXT_NOTE: &str = r#"Add `-1m` to any Claude model, like `claude-sonnet-4-6-1m`, to use the 1M token context window."#;

const BEDROCK_NOTE: &str = r#"#### Amazon Bedrock

If you already use Claude through AWS Bedrock, you can point Maki at it instead of the direct Anthropic API. Set `CLAUDE_CODE_USE_BEDROCK=1` and Maki will route all Anthropic requests through Bedrock. The same models, the same features, just a different door.

You will need `AWS_REGION` and one of the following for auth:

| Method | Env vars |
|--------|----------|
| IAM credentials | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`) |
| Credentials file | `AWS_PROFILE` (defaults to `default`), reads `~/.aws/credentials` |
| Bearer token | `AWS_BEARER_TOKEN_BEDROCK` |
| Gateway proxy | `CLAUDE_CODE_SKIP_BEDROCK_AUTH=1` + `ANTHROPIC_BEDROCK_BASE_URL` (skips signing, useful behind a proxy that handles auth) |

You can override the model with `ANTHROPIC_MODEL` and the endpoint with `ANTHROPIC_BEDROCK_BASE_URL`. These env var names match Claude Code, so if you were already using Bedrock there, the same setup works here."#;

const OPENCODE_FREE_MODELS_NOTE: &str = r#"By default Maki hides free models from the Opencode catalog. To list free models (they use a public fallback, no API key needed), add this to `~/.config/maki/providers.toml`:

```toml
[opencode]
enable_free_models = true
```

The default is `false`."#;

const MODEL_IDENTIFIERS: &str = r#"## Model Identifiers

Models are referenced as `provider/model_id`:

```
anthropic/claude-sonnet-4-6
openai/gpt-4.1
zai/glm-4.7
```

If the model name is unique across providers, the prefix can be omitted."#;

fn dynamic_providers_section() -> String {
    let mut valid_values: Vec<String> = ManifestRegistry::builtins()
        .iter()
        .map(|m| format!("`{}`", m.slug.as_ref()))
        .collect();
    valid_values.sort();

    format!(
        r#"## Dynamic Providers

To add a custom provider or proxy, drop an executable script into `~/.config/maki/providers/`. The script must handle these subcommands:

| Subcommand | Timeout | What it does |
|------------|---------|--------|
| `info` | 5s | Return JSON with `display_name`, `base` provider, `has_auth` |
| `models` | 5s | Return JSON array of model entries (optional) |
| `resolve` | 30s | Return auth JSON (`base_url`, `headers`) |
| `login` | interactive | OAuth or credential flow |
| `logout` | interactive | Clear credentials |
| `refresh` | 30s | Refresh auth tokens |

`resolve` is called each time a new agent spawns, so scripts should read tokens from disk instead of caching them in memory. That way auth changes from other processes get picked up.

The `base` field specifies which built-in provider to inherit the model catalog from. Valid values: {}.

If your provider serves models not in the base catalog, add a `models` subcommand returning:

```json
[{{"id": "my-model-v2", "tier": "strong", "context_window": 200000, "max_output_tokens": 16384}}]
```

Only `id` is required. Optional fields: `tier` (default `medium`), `context_window` (128K), `max_output_tokens` (16K), `pricing` (`{{input, output, cache_write, cache_read}}`, all per 1M tokens), `supports_tool_examples` (defaults to the base provider's setting), `supports_thinking` (defaults to the base provider's setting), `supports_vision` (defaults to the base provider's setting; when false, image input and the `view_image` tool are disabled). The first model listed per tier is used for sub-agents. Without this subcommand, the base provider's models are used.

Dynamic provider models are namespaced as `{{slug}}/{{model_id}}` (e.g. `myproxy/claude-sonnet-4-6`).

### Script Name Rules

- Must start with a letter or digit
- Only letters, digits, underscores, and hyphens after that
- Can't reuse a built-in provider's slug
- Must be executable"#,
        valid_values.join(", "),
    )
}

fn tier_label(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Weak => "Weak",
        ModelTier::Medium => "Medium",
        ModelTier::Strong => "Strong",
        ModelTier::Compaction => "Compaction",
    }
}

fn format_pricing(entry: &ModelEntry) -> String {
    format!("${:.2} / ${:.2}", entry.pricing.input, entry.pricing.output)
}

fn format_context(entry: &ModelEntry) -> String {
    let ctx_k = entry.context_window / 1_000;
    let out_k = entry.max_output_tokens / 1_000;
    format!("{ctx_k}K ctx / {out_k}K out")
}

struct ProviderSection {
    kind: ProviderKind,
    name: &'static str,
    auth_line: String,
    urls: Vec<&'static str>,
    features: Option<&'static str>,
    entries: &'static [ModelEntry],
}

fn format_auth(kind: ProviderKind) -> String {
    let env = kind.api_key_env();
    if kind == ProviderKind::Ollama {
        format!("`OLLAMA_HOST` for local/remote (e.g. `http://localhost:11434`), `{env}` for auth")
    } else {
        format!("`{env}`")
    }
}

fn build_sections() -> Vec<ProviderSection> {
    let mut sections = Vec::new();

    for kind in ProviderKind::iter() {
        match kind {
            ProviderKind::Zai => {
                sections.push(ProviderSection {
                    kind: ProviderKind::Zai,
                    name: "Z.AI",
                    auth_line: format!(
                        "{} (shared across both endpoints)",
                        format_auth(ProviderKind::Zai)
                    ),
                    urls: vec![
                        ProviderKind::Zai.base_url(),
                        "https://api.z.ai/api/coding/paas/v4",
                    ],
                    features: ProviderKind::Zai.features(),
                    entries: ManifestRegistry::get("zai").unwrap().models,
                });
            }
            ProviderKind::OpenAi => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format!("{} (also supports OAuth device flow)", format_auth(kind)),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: ManifestRegistry::get(&kind.to_string()).unwrap().models,
                });
            }
            ProviderKind::Copilot => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format!(
                        "{} (or run `maki auth login copilot` to import a token from gh)",
                        format_auth(kind)
                    ),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: ManifestRegistry::get(&kind.to_string()).unwrap().models,
                });
            }
            _ => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format_auth(kind),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: ManifestRegistry::get(&kind.to_string()).unwrap().models,
                });
            }
        }
    }

    sections
}

fn write_model_table(out: &mut String, entries: &[ModelEntry]) {
    let _ = writeln!(
        out,
        "| Tier | Models | Pricing (in/out per 1M tokens) | Context |"
    );
    let _ = writeln!(
        out,
        "|------|--------|-------------------------------|---------|"
    );

    // A row per model, not per tier: prices and context sizes differ inside a
    // tier, so one merged row would quote a single model's numbers for all.
    for tier in [ModelTier::Weak, ModelTier::Medium, ModelTier::Strong] {
        for entry in entries.iter().filter(|e| e.tier == tier) {
            let names = entry.prefixes.join(", ");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                tier_label(tier),
                if entry.default {
                    format!("**{names}** (default)")
                } else {
                    names
                },
                format_pricing(entry),
                format_context(entry),
            );
        }
    }

    let defaults: Vec<String> = entries
        .iter()
        .filter(|e| e.default)
        .map(|e| {
            format!(
                "{} ({})",
                e.prefixes.first().unwrap_or(&"?"),
                tier_label(e.tier).to_lowercase(),
            )
        })
        .collect();

    if !defaults.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Defaults: {}", defaults.join(", "));
    }
}

fn no_catalog_note(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Ollama => {
            "This provider talks the OpenAI-compatible `/v1` API, so it also works with \
             llama.cpp's server, LocalAI, or anything else that speaks the same protocol. \
             Just point `OLLAMA_HOST` to the right address \
             (e.g. `http://localhost:8080` for llama.cpp)."
        }
        ProviderKind::LlamaCpp => {
            "Connects to any OpenAI-compatible `/v1` endpoint. Point `LLAMA_CPP_HOST` \
             to your server address (defaults to `http://localhost:8080`)."
        }
        ProviderKind::OpenRouter => {
            "OpenRouter aggregates models from many providers behind a single API key. \
             Browse available models at [openrouter.ai/models](https://openrouter.ai/models). \
             Use any model ID directly (e.g. `openrouter/anthropic/claude-sonnet-4`)."
        }
        _ => "No hardcoded model catalog. Use any model ID supported by this provider.",
    }
}

fn write_section(out: &mut String, section: &ProviderSection) {
    let _ = writeln!(out, "### {}\n", section.name);
    let _ = writeln!(out, "- **Env var**: {}", section.auth_line);

    if section.urls.len() == 1 {
        let _ = writeln!(out, "- **API**: `{}`", section.urls[0]);
    } else {
        let _ = writeln!(out, "- **API endpoints**:");
        for url in &section.urls {
            let _ = writeln!(out, "  - `{url}`");
        }
    }

    if let Some(features) = section.features {
        let _ = writeln!(out, "- **Features**: {features}");
    }

    let _ = writeln!(out);

    if section.entries.is_empty() {
        let _ = writeln!(out, "{}", no_catalog_note(section.kind));
    } else {
        write_model_table(out, section.entries);
    }

    if section.name == "Anthropic" {
        let _ = writeln!(out, "\n{LONG_CONTEXT_NOTE}");
        let _ = writeln!(out, "\n{BEDROCK_NOTE}");
    }

    if section.kind == ProviderKind::Opencode {
        let _ = writeln!(out, "\n{OPENCODE_FREE_MODELS_NOTE}");
    }
}

/// Features line for a manifest-owned provider. A manifest's `qualities` (an
/// author-owned prose list, e.g. "open-weight models") wins verbatim when
/// present; otherwise the line is derived from capability fields (thinking,
/// vision, arbitrary models). Mirrors the prose style of `ProviderKind::features`.
fn derive_manifest_features(manifest: &ProviderManifest) -> Option<String> {
    if let Some(qs) = &manifest.qualities
        && !qs.is_empty()
    {
        let mut s = qs.join(", ");
        s[0..1].make_ascii_uppercase();
        return Some(s);
    }
    let mut feats: Vec<&str> = Vec::new();
    if manifest.supports_thinking {
        feats.push("thinking mode");
    }
    if manifest.models.iter().any(|m| m.vision) {
        feats.push("vision input");
    }
    if manifest.accepts_arbitrary_models {
        feats.push("arbitrary model IDs");
    }
    if feats.is_empty() {
        return None;
    }
    let mut s = feats.join(", ");
    s[0..1].make_ascii_uppercase();
    Some(s)
}

/// One section per manifest-owned builtin (DeepSeek today). Skips slugs the
/// native `ProviderKind` already documents (so the two paths never double-print
/// a provider) and derives env/url from `EngineSpec`, the authoritative source
/// of both for a Lua-registered provider.
fn write_manifest_sections(out: &mut String) {
    let mut owned: Vec<&ProviderManifest> = ManifestRegistry::builtins()
        .into_iter()
        .filter(|m| ProviderKind::from_str(m.slug.as_ref()).is_err())
        .collect();
    owned.sort_by(|a, b| a.slug.cmp(&b.slug));

    for manifest in owned {
        let _ = writeln!(out, "### {}\n", manifest.display_name.as_ref());
        let mut auth_lines: Vec<String> = Vec::new();
        let mut urls: Vec<String> = Vec::new();
        if let Some(EngineSpec::OpenaiCompat {
            api_key_env,
            base_url,
            ..
        }) = manifest_provider::engine_spec(manifest.slug.as_ref())
        {
            auth_lines.push(format!("`{api_key_env}`"));
            urls.push(base_url);
        }
        for line in &auth_lines {
            let _ = writeln!(out, "- **Env var**: {line}");
        }
        if urls.len() == 1 {
            let _ = writeln!(out, "- **API**: `{}`", urls[0]);
        } else {
            let _ = writeln!(out, "- **API endpoints**:");
            for url in &urls {
                let _ = writeln!(out, "  - `{url}`");
            }
        }
        if let Some(features) = derive_manifest_features(manifest) {
            let _ = writeln!(out, "- **Features**: {features}");
        }
        let _ = writeln!(out);
        write_model_table(out, manifest.models);
        let _ = writeln!(out);
    }
}

pub fn generate() -> String {
    let mut out = String::with_capacity(4096);

    let _ = writeln!(out, "{FRONT_MATTER}\n");
    let _ = writeln!(out, "# Providers\n");
    let _ = writeln!(
        out,
        "Maki talks to LLM providers over their HTTP APIs. \
         Models are split into three tiers: **weak** (cheap and fast), \
         **medium** (balanced), and **strong** (highest capability, highest cost). \
         There is also a **compaction** tier for choosing a dedicated model to summarize context when the conversation grows long.\n"
    );
    let _ = writeln!(out, "{TIER_PICKER_NOTE}\n");
    let _ = writeln!(out, "{AUTH_RELOADING}\n");
    let _ = writeln!(out, "{BASE_URL_OVERRIDES}\n");
    let _ = writeln!(out, "## Built-in Providers\n");

    // Boot the Lua host so runtime-owned manifests (DeepSeek) register into
    // `ManifestRegistry` before `build_sections` reads them. The leaked
    // `'static` manifest data outlives the host, but we keep it alive across
    // generation to mirror the live `maki models` flow.
    let _host = PluginHost::with_all_builtins(Arc::new(ToolRegistry::new()))
        .expect("loading builtin plugins");

    for section in &build_sections() {
        write_section(&mut out, section);
        let _ = writeln!(out);
    }

    // Manifest-owned builtins (DeepSeek) are loaded by the Lua host above and
    // register into `ManifestRegistry`; render their sections out of the
    // engine/auth specs since they have no Rust `ProviderKind`.
    write_manifest_sections(&mut out);

    let _ = writeln!(out, "{MODEL_IDENTIFIERS}\n");
    let _ = writeln!(out, "{}", dynamic_providers_section());
    let _ = writeln!(out, "{AUTHORING_GUIDE}");

    out
}

const AUTHORING_GUIDE: &str = r#"## Authoring a Provider

Built-in providers like DeepSeek ship as Lua manifests under `plugins/providers/`. The `providers` plugin is a normal builtin: its `init.lua` requires one module per provider (e.g. `require("deepseek")` loads `plugins/providers/deepseek.lua`), and each manifest is plain Lua that calls `maki.provider.register` to wire a slug to an engine, login metadata, and a static model list. To add a provider, drop a `<slug>.lua` file next to the others and add a `require("<slug>")` line to `plugins/providers/init.lua`. No Rust change.

A manifest has several pieces:

- **`slug`** (string): the identifier used in model specs (`<slug>/<model_id>`) and `maki auth login`.
- **`engine`** (table from `maki.provider.openai_compat{...}`): the OpenAI-compatible endpoint. Keys: `base_url`, `api_key_env`, `max_tokens_field`, `include_stream_usage`, `provider_name`, `thinking` (e.g. `"deepseek"`, for the reasoning toggle shape), and `usage_url` (optional balance/quota endpoint fetched by Rust).
- **`login_url`** (optional string): URL the login flow points users at to acquire an API key. Paired with `api_key_env` (on the engine), this is all the auth surface needs for env-key scope.
- **`needs_url`** (optional boolean, default false): prompt for a custom base URL during `maki auth login`.
- **`models`** (list): static model entries with `id`, `tier`, `default`, `pricing`, `context_window`, `max_output_tokens`, `supports_thinking`, `supports_vision`.
- **`qualities`** (optional list of strings): descriptive prose shown in the provider docs Features line (e.g. `"open-weight models"`). When omitted, the line is derived from capability fields (`supports_thinking`, vision, `accepts_arbitrary_models`).
- **`usage`** (optional function): `function(body) -> { plan = string?, limits = table }`, where Rust fetches `engine.usage_url` with the provider's auth and hands the response body string to this callback. `limits` is a list of `{ label, percentage?, reset_at?, detail? }`. When either `usage_url` or `usage` is absent, the provider reports no programmatic usage.

```lua
maki.provider.register({
  slug = "my-provider",
  display_name = "My Provider",
  family = "generic",
  supports_thinking = false,
  accepts_arbitrary_models = false,
  fallback_max_output = 8192,
  fallback_context_window = 128000,
  login_url = "https://platform.my-provider.com/api_keys",
  qualities = { "open-weight models" },
  engine = maki.provider.openai_compat({
    base_url = "https://api.my-provider.com",
    api_key_env = "MY_PROVIDER_API_KEY",
    max_tokens_field = "max_tokens",
    include_stream_usage = false,
    provider_name = "MyProvider",
    usage_url = "https://api.my-provider.com/balance",
  }),
  usage = function(body)
    local data = maki.json.decode(body)
    return { limits = { { label = "Balance", detail = data and data.balance or nil } } }
  end,
  models = {
    {
      id = "my-model-v2",
      tier = "strong",
      default = true,
      pricing = { input = 1.0, output = 2.0, cache_write = 0.0, cache_read = 0.0 },
      max_output_tokens = 16384,
      context_window = 200000,
    },
  },
})
```

Only `slug`, `engine`, and `models` are required; `display_name`, `family`, `supports_thinking`, `accepts_arbitrary_models`, `fallback_max_output`, `fallback_context_window`, `qualities`, `login_url`/`needs_url`, and `usage`/`usage_url` are optional and default to generic-safe values (or, for `qualities`, a derived features line). Because manifests run at startup as a side-effect of plugin load, no return value is needed.

The `DeepSeek` bundled manifest (`plugins/providers/deepseek.lua`) is a complete, copy-pasteable reference, including the `thinking = "deepseek"` engine flag that wires up the deepseek reasoning toggle."#;
