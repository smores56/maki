use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

use maki_config::providers::Protocol;
use tracing::warn;

use crate::AgentError;
use crate::auth::{AuthSpec, BuildOptions};
use crate::builtin::builtin_provider;
use crate::model::{ModelEntry, ModelFamily};
use crate::provider::Provider;
use crate::providers::anthropic::Anthropic;
use crate::providers::anthropic::bedrock;
use crate::providers::copilot::Copilot;
use crate::providers::deepseek::DeepSeek;
use crate::providers::google::Google;
use crate::providers::local::{LLAMACPP, LocalEndpoint, OLLAMA};
use crate::providers::mistral::Mistral;
use crate::providers::openai::OpenAi;
use crate::providers::opencode::Opencode;
use crate::providers::openrouter::OpenRouter;
use crate::providers::synthetic::Synthetic;
use crate::providers::tensorx::TensorX;
use crate::providers::zai::Zai;

/// `None` for Rust builtins; the `/reload` teardown key owned by every other
/// `Source` below.
pub const SCRIPT_OWNER: &str = "<script>";
pub const TOML_OWNER: &str = "<providers.toml>";

/// Where a [`ProviderSpec`] came from. Drives register precedence: a higher
/// source replaces a lower one, an equal source is dropped (with a warning) so
/// the first registration wins ties.
///
/// Derived from [`ProviderSpec::owner`], never stored, so the registry never
/// has to be reentered to classify a spec (the `LazyLock` is not reentrant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Builtin,
    Script,
    Toml,
    User,
}

/// A spec's constructor. Resolves auth/bedrock at BUILD time (inside the fn),
/// never at spec-construction time, because the registry's `LazyLock` is not
/// reentrant — a `spec()`-time lookup would recurse into `builtin_specs`.
/// `BuildOptions` carries pre-resolved `auth` + `system_prefix` for script and
/// toml-produced specs; builtins ignore them and go through `KeyPool::resolve`.
pub type BuildFn = fn(&Arc<ProviderSpec>, BuildOptions) -> Result<Box<dyn Provider>, AgentError>;

pub struct ProviderSpec {
    pub slug: Arc<str>,
    pub owner: Option<Arc<str>>,
    pub display_name: String,
    pub family: ModelFamily,
    pub features: Option<String>,
    pub protocol: Protocol,
    pub base_url: Option<String>,
    pub default_model: Option<String>,
    pub auth: AuthSpec,
    pub supports_thinking: bool,
    pub accepts_arbitrary_models: bool,
    pub fallback_max_output: Option<u32>,
    pub fallback_context_window: u32,
    /// Declares which `/models` capability keys the codec retains. Builtins
    /// populate only the keys they actually consume (openrouter `reasoning`,
    /// tensorx `supported_openai_params`); an empty vec means discovery drops
    /// everything but the structural fields.
    pub capability_keys: Vec<String>,
    pub models: Vec<ModelEntry>,
    pub build: BuildFn,
}

impl ProviderSpec {
    pub fn source(&self) -> Source {
        match &self.owner {
            None => Source::Builtin,
            Some(o) if o.as_ref() == SCRIPT_OWNER => Source::Script,
            Some(o) if o.as_ref() == TOML_OWNER => Source::Toml,
            Some(_) => Source::User,
        }
    }
}

pub static REGISTRY: LazyLock<RwLock<HashMap<Arc<str>, Arc<ProviderSpec>>>> =
    LazyLock::new(|| RwLock::new(builtin_specs()));

/// Lazily registers providers.toml custom specs the first time the registry is
/// read. Runs after the `LazyLock` is populated, so it can write `REGISTRY`
/// without reentering `builtin_specs`. Script specs register themselves via
/// `dynamic::discover` on first read.
static EXTRA_REGISTERED: OnceLock<()> = OnceLock::new();

fn ensure_extra_registered() {
    EXTRA_REGISTERED.get_or_init(|| {
        crate::providers::custom::ensure_registered();
    });
}

pub fn get(slug: &str) -> Option<Arc<ProviderSpec>> {
    ensure_extra_registered();
    REGISTRY.read().unwrap().get(slug).cloned()
}

/// Reads the registry WITHOUT triggering `ensure_extra_registered`. Used during
/// custom spec registration (which itself runs inside `ensure_extra_registered`)
/// to check whether a slug already belongs to a builtin — reentering
/// `ensure_extra_registered` there would deadlock the `OnceLock`.
pub(crate) fn get_no_extra(slug: &str) -> Option<Arc<ProviderSpec>> {
    REGISTRY.read().unwrap().get(slug).cloned()
}

/// All registered specs, sorted by slug for docgen determinism.
pub fn all() -> Vec<Arc<ProviderSpec>> {
    ensure_extra_registered();
    let guard = REGISTRY.read().unwrap();
    let mut specs: Vec<Arc<ProviderSpec>> = guard.values().cloned().collect();
    specs.sort_by(|a, b| a.slug.cmp(&b.slug));
    specs
}

pub fn register(spec: ProviderSpec) {
    let mut guard = REGISTRY.write().unwrap();
    let slug = Arc::clone(&spec.slug);
    let new_source = spec.source();
    match guard.get(slug.as_ref()) {
        Some(existing) => {
            let existing_source = existing.source();
            if new_source > existing_source {
                guard.insert(slug, Arc::new(spec));
            } else if new_source == existing_source {
                warn!(slug = %slug, source = ?new_source, "duplicate provider spec; keeping first");
            }
        }
        None => {
            guard.insert(slug, Arc::new(spec));
        }
    }
}

/// Removes only specs whose `owner` matches `owner`. Used by `/reload` to drop
/// specs owned by a reloaded plugin/config before re-registering them.
pub fn clear_owner(owner: &str) {
    let mut guard = REGISTRY.write().unwrap();
    guard.retain(|_, spec| spec.owner.as_deref() != Some(owner));
}

fn build_anthropic(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    if let Some(auth) = opts.auth {
        return Ok(Box::new(
            Anthropic::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix),
        ));
    }
    if bedrock::is_enabled() {
        Ok(Box::new(bedrock::Bedrock::new(opts.timeouts)?))
    } else {
        Ok(Box::new(Anthropic::new(opts.timeouts)?))
    }
}

fn build_openai(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => OpenAi::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix),
        None => OpenAi::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_google(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => Google::with_auth(auth, opts.timeouts),
        None => Google::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_copilot(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            Copilot::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => Copilot::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_ollama(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => LocalEndpoint::with_auth(&OLLAMA, auth, opts.timeouts)
            .with_system_prefix(opts.system_prefix),
        None => LocalEndpoint::new(&OLLAMA, opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_llama_cpp(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => LocalEndpoint::with_auth(&LLAMACPP, auth, opts.timeouts)
            .with_system_prefix(opts.system_prefix),
        None => LocalEndpoint::new(&LLAMACPP, opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_mistral(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            Mistral::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => Mistral::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_zai(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => Zai::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix),
        None => Zai::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_deepseek(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            DeepSeek::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => DeepSeek::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_openrouter(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            OpenRouter::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => OpenRouter::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_synthetic(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            Synthetic::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => Synthetic::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_tensorx(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            TensorX::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => TensorX::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

fn build_opencode(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    let provider = match opts.auth {
        Some(auth) => {
            Opencode::with_auth(auth, opts.timeouts).with_system_prefix(opts.system_prefix)
        }
        None => Opencode::new(opts.timeouts)?,
    };
    Ok(Box::new(provider))
}

/// `opencode-go` streams through the models.dev catalog as a sibling of
/// `opencode`. Built as a catalog provider at build time (the catalog may be
/// cold, in which case [`catalog::try_create`] returns a lazy provider).
fn build_opencode_go(
    _spec: &Arc<ProviderSpec>,
    opts: BuildOptions,
) -> Result<Box<dyn Provider>, AgentError> {
    match crate::providers::catalog::try_create("opencode-go", opts.timeouts) {
        Some(result) => result,
        None => Err(AgentError::Config {
            message: "opencode-go catalog provider unavailable".to_string(),
        }),
    }
}

struct BuiltinSpec {
    slug: &'static str,
    display_name: &'static str,
    family: ModelFamily,
    features: Option<&'static str>,
    supports_thinking: bool,
    accepts_arbitrary_models: bool,
    fallback_max_output: Option<u32>,
    fallback_context_window: u32,
    capability_keys: &'static [&'static str],
    models: fn() -> Vec<ModelEntry>,
    build: BuildFn,
}

const BUILTINS: &[BuiltinSpec] = &[
    BuiltinSpec {
        slug: "anthropic",
        display_name: "Anthropic",
        family: ModelFamily::Claude,
        features: Some("Prompt caching, thinking mode (adaptive/budgeted), advanced tool use"),
        supports_thinking: true,
        accepts_arbitrary_models: false,
        fallback_max_output: Some(128_000),
        fallback_context_window: 200_000,
        capability_keys: &[],
        models: crate::providers::anthropic::models,
        build: build_anthropic,
    },
    BuiltinSpec {
        slug: "openai",
        display_name: "OpenAI",
        family: ModelFamily::Gpt,
        features: None,
        supports_thinking: true,
        accepts_arbitrary_models: false,
        fallback_max_output: Some(100_000),
        fallback_context_window: 200_000,
        capability_keys: &[],
        models: crate::providers::openai::models,
        build: build_openai,
    },
    BuiltinSpec {
        slug: "google",
        display_name: "Google",
        family: ModelFamily::Gemini,
        features: Some("Native Gemini API with thinking support"),
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(65_536),
        fallback_context_window: 1_000_000,
        capability_keys: &[],
        models: crate::providers::google::models,
        build: build_google,
    },
    BuiltinSpec {
        slug: "copilot",
        display_name: "Copilot",
        family: ModelFamily::Generic,
        features: Some("Native Copilot Chat HTTP API with model endpoint discovery"),
        supports_thinking: false,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(100_000),
        fallback_context_window: 200_000,
        capability_keys: &[],
        models: crate::providers::copilot::models,
        build: build_copilot,
    },
    BuiltinSpec {
        slug: "ollama",
        display_name: "Ollama",
        family: ModelFamily::Generic,
        features: Some(
            "Local or remote inference via OLLAMA_HOST, cloud fallback via OLLAMA_API_KEY",
        ),
        supports_thinking: false,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(16_384),
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::ollama::models,
        build: build_ollama,
    },
    BuiltinSpec {
        slug: "llama-cpp",
        display_name: "LlamaCpp",
        family: ModelFamily::Generic,
        features: Some(
            "Local or remote inference via LLAMA_CPP_HOST, set optional key via LLAMA_CPP_API_KEY",
        ),
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: None,
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::llama_cpp::models,
        build: build_llama_cpp,
    },
    BuiltinSpec {
        slug: "mistral",
        display_name: "Mistral",
        family: ModelFamily::Generic,
        features: None,
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(32_000),
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::mistral::models,
        build: build_mistral,
    },
    BuiltinSpec {
        slug: "zai",
        display_name: "Z.AI",
        family: ModelFamily::Glm,
        features: None,
        supports_thinking: false,
        accepts_arbitrary_models: false,
        fallback_max_output: Some(16_000),
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::zai::models,
        build: build_zai,
    },
    BuiltinSpec {
        slug: "deepseek",
        display_name: "DeepSeek",
        family: ModelFamily::Generic,
        features: Some("Thinking mode toggle (on/off), open-weight models"),
        supports_thinking: true,
        accepts_arbitrary_models: false,
        fallback_max_output: Some(384_000),
        fallback_context_window: 1_000_000,
        capability_keys: &[],
        models: crate::providers::deepseek::models,
        build: build_deepseek,
    },
    BuiltinSpec {
        slug: "openrouter",
        display_name: "OpenRouter",
        family: ModelFamily::Generic,
        features: Some("300+ models from all providers, prompt caching, provider routing"),
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(128_000),
        fallback_context_window: 200_000,
        capability_keys: &["reasoning"],
        models: crate::providers::openrouter::models,
        build: build_openrouter,
    },
    BuiltinSpec {
        slug: "synthetic",
        display_name: "Synthetic",
        family: ModelFamily::Synthetic,
        features: Some("Reasoning effort support (low/medium/high), open-weight models"),
        supports_thinking: true,
        accepts_arbitrary_models: false,
        fallback_max_output: Some(32_000),
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::synthetic::models,
        build: build_synthetic,
    },
    BuiltinSpec {
        slug: "tensorx",
        display_name: "TensorX",
        family: ModelFamily::Generic,
        features: Some("Open-weight models, zero data retention, prompt caching"),
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: None,
        fallback_context_window: 200_000,
        capability_keys: &["supported_openai_params"],
        models: crate::providers::tensorx::models,
        build: build_tensorx,
    },
    BuiltinSpec {
        slug: "opencode",
        display_name: "Opencode",
        family: ModelFamily::Generic,
        features: Some(
            "Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Zen API",
        ),
        supports_thinking: true,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(128_000),
        fallback_context_window: 256_000,
        capability_keys: &[],
        models: crate::providers::opencode::models,
        build: build_opencode,
    },
    BuiltinSpec {
        slug: "opencode-go",
        display_name: "Opencode Go",
        family: ModelFamily::Generic,
        features: Some(
            "Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Go API",
        ),
        supports_thinking: false,
        accepts_arbitrary_models: true,
        fallback_max_output: Some(64_000),
        fallback_context_window: 128_000,
        capability_keys: &[],
        models: crate::providers::opencode::models,
        build: build_opencode_go,
    },
];

/// Builtins not in the [`builtin_provider`] inventory (`openrouter`,
/// `opencode`) fold their BuiltInProvider-style fields here. Every other
/// builtin copies them straight from the inventory at spec-build time.
struct OmitData {
    protocol: Protocol,
    base_url: &'static str,
    api_key_env: &'static str,
    login_url: Option<&'static str>,
    needs_url: bool,
}

fn omit_for(slug: &str) -> Option<OmitData> {
    Some(match slug {
        "openrouter" => OmitData {
            protocol: Protocol::Openai,
            base_url: "https://openrouter.ai/api/v1",
            api_key_env: "OPENROUTER_API_KEY",
            login_url: None,
            needs_url: false,
        },
        "opencode" => OmitData {
            protocol: Protocol::Openai,
            base_url: "https://opencode.ai/zen/v1",
            api_key_env: "OPENCODE_API_KEY",
            login_url: None,
            needs_url: false,
        },
        "opencode-go" => OmitData {
            protocol: Protocol::Openai,
            base_url: "https://opencode.ai/zen/go/v1",
            api_key_env: "OPENCODE_API_KEY",
            login_url: None,
            needs_url: false,
        },
        _ => return None,
    })
}

fn builtin_spec(b: &'static BuiltinSpec) -> ProviderSpec {
    let (protocol, base_url, default_model, auth) = match builtin_provider(b.slug) {
        Some(bp) => (
            bp.protocol,
            Some(bp.default_base_url.to_string()),
            Some(bp.default_model.to_string()),
            AuthSpec::ApiKey {
                env: bp.default_api_key_env.to_string(),
                login_url: bp.login_url.map(String::from),
                needs_url: bp.needs_url,
                plans: bp.plans,
            },
        ),
        None => {
            let omit = omit_for(b.slug).expect("builtin without inventory entry must be listed");
            (
                omit.protocol,
                Some(omit.base_url.to_string()),
                None,
                AuthSpec::ApiKey {
                    env: omit.api_key_env.to_string(),
                    login_url: omit.login_url.map(String::from),
                    needs_url: omit.needs_url,
                    plans: None,
                },
            )
        }
    };
    ProviderSpec {
        slug: Arc::from(b.slug),
        owner: None,
        display_name: b.display_name.to_string(),
        family: b.family,
        features: b.features.map(String::from),
        protocol,
        base_url,
        default_model,
        auth,
        supports_thinking: b.supports_thinking,
        accepts_arbitrary_models: b.accepts_arbitrary_models,
        fallback_max_output: b.fallback_max_output,
        fallback_context_window: b.fallback_context_window,
        capability_keys: b
            .capability_keys
            .iter()
            .copied()
            .map(String::from)
            .collect(),
        models: (b.models)(),
        build: b.build,
    }
}

fn builtin_specs() -> HashMap<Arc<str>, Arc<ProviderSpec>> {
    BUILTINS
        .iter()
        .map(|b| {
            let spec = Arc::new(builtin_spec(b));
            (Arc::clone(&spec.slug), spec)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelTier;

    fn user_spec(slug: &str) -> ProviderSpec {
        ProviderSpec {
            slug: Arc::from(slug),
            owner: Some(Arc::from("user-a")),
            display_name: String::new(),
            family: ModelFamily::Generic,
            features: None,
            protocol: Protocol::Openai,
            base_url: None,
            default_model: None,
            auth: AuthSpec::ApiKey {
                env: String::new(),
                login_url: None,
                needs_url: false,
                plans: None,
            },
            supports_thinking: false,
            accepts_arbitrary_models: false,
            fallback_max_output: None,
            fallback_context_window: 0,
            capability_keys: Vec::new(),
            models: Vec::new(),
            build: build_openai,
        }
    }

    fn script_spec(slug: &str, owner: &str) -> ProviderSpec {
        let mut spec = user_spec(slug);
        spec.owner = Some(Arc::from(owner));
        spec
    }

    #[test]
    fn all_is_sorted_by_slug() {
        let specs = all();
        let slugs: Vec<&str> = specs.iter().map(|s| s.slug.as_ref()).collect();
        let mut sorted = slugs.clone();
        sorted.sort();
        assert_eq!(slugs, sorted);
    }

    #[test]
    fn all_contains_every_builtin() {
        let specs = all();
        let mut actual: Vec<&str> = specs.iter().map(|s| s.slug.as_ref()).collect();
        actual.sort();
        let mut expected: Vec<&str> = BUILTINS.iter().map(|b| b.slug).collect();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn builtin_specs_have_no_owner() {
        for spec in all() {
            assert!(spec.owner.is_none(), "{} has an owner", spec.slug);
            assert_eq!(spec.source(), Source::Builtin, "{}", spec.slug);
        }
    }

    #[test]
    fn builtin_family_matches_expected() {
        let cases: &[(&str, ModelFamily)] = &[
            ("anthropic", ModelFamily::Claude),
            ("openai", ModelFamily::Gpt),
            ("google", ModelFamily::Gemini),
            ("zai", ModelFamily::Glm),
            ("synthetic", ModelFamily::Synthetic),
            ("ollama", ModelFamily::Generic),
            ("opencode", ModelFamily::Generic),
        ];
        for (slug, family) in cases {
            assert_eq!(get(slug).unwrap().family, *family, "{slug}");
        }
    }

    #[test]
    fn builtin_capability_keys() {
        assert_eq!(
            get("openrouter").unwrap().capability_keys,
            vec!["reasoning".to_string()]
        );
        assert_eq!(
            get("tensorx").unwrap().capability_keys,
            vec!["supported_openai_params".to_string()]
        );
        let empty: Vec<String> = Vec::new();
        assert_eq!(get("anthropic").unwrap().capability_keys, empty);
    }

    #[test]
    fn one_default_per_tier_per_spec() {
        for tier in [ModelTier::Weak, ModelTier::Medium, ModelTier::Strong] {
            for spec in all() {
                if spec.accepts_arbitrary_models {
                    continue;
                }
                if spec.slug.as_ref() == "deepseek" && tier == ModelTier::Weak {
                    continue;
                }
                let count = spec
                    .models
                    .iter()
                    .filter(|e| e.default && e.tier == tier)
                    .count();
                assert!(count <= 1, "{}/{}: {count} defaults", spec.slug, tier);
            }
        }
    }

    #[test]
    fn context_window_at_least_max_output() {
        for spec in all() {
            for entry in &spec.models {
                let Some(max_output) = entry.max_output_tokens else {
                    continue;
                };
                assert!(
                    entry.context_window >= max_output,
                    "{}/{}: ctx {} < out {max_output}",
                    spec.slug,
                    entry.prefixes.first().map(String::as_str).unwrap_or("?"),
                    entry.context_window
                );
            }
        }
    }

    #[test]
    fn higher_source_replaces_lower() {
        let slug = "test-higher-replaces";
        register(user_spec(slug));
        assert!(get(slug).is_some());
        assert_eq!(get(slug).unwrap().source(), Source::User);
    }

    #[test]
    fn lower_source_never_replaces_higher() {
        let slug = "test-lower-never";
        register(user_spec(slug));
        let mut script = script_spec(slug, SCRIPT_OWNER);
        script.display_name = String::from("script-should-lose");
        register(script);
        let got = get(slug).unwrap();
        assert_eq!(got.source(), Source::User);
        assert_eq!(got.display_name, "");
    }

    #[test]
    fn equal_source_keeps_first() {
        let slug = "test-equal-source";
        let mut first = user_spec(slug);
        first.display_name = String::from("first");
        register(first);
        let mut second = user_spec(slug);
        second.display_name = String::from("second");
        register(second);
        assert_eq!(get(slug).unwrap().display_name, "first");
    }

    #[test]
    fn user_beats_builtin_registered_first() {
        let slug = "anthropic";
        let mut user = user_spec(slug);
        user.display_name = String::from("user-anthropic");
        register(user);
        let got = get(slug).unwrap();
        assert_eq!(got.source(), Source::User);
        assert_eq!(got.display_name, "user-anthropic");
    }

    #[test]
    fn clear_owner_removes_only_matching_specs() {
        let slug_a = "test-clear-owner-a";
        let slug_b = "test-clear-owner-b";
        let mut a = user_spec(slug_a);
        a.owner = Some(Arc::from("owner-a"));
        let mut b = user_spec(slug_b);
        b.owner = Some(Arc::from("owner-b"));
        register(a);
        register(b);
        clear_owner("owner-a");
        assert!(get(slug_a).is_none());
        assert!(get(slug_b).is_some());
        clear_owner("owner-b");
        assert!(get(slug_b).is_none());
    }

    #[test]
    fn clear_owner_spares_builtins() {
        clear_owner(SCRIPT_OWNER);
        clear_owner(TOML_OWNER);
        assert!(get("anthropic").is_some());
    }
}
