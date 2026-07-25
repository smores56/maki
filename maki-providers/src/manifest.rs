use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use crate::model::{ModelEntry, ModelFamily, ModelTier};
use crate::providers::{
    anthropic, copilot, custom, dynamic, google, llama_cpp, mistral, ollama, openai, openrouter,
    synthetic, tensorx, zai,
};

/// Capability contract for a provider. The string fields (`slug`,
/// `display_name`, `qualities`) are owned `Arc<str>` so runtime manifests
/// built by the Lua loader allocate instead of leaking their prose into
/// `'static`. The manifest *shell* is leaked once at boot (see
/// `ManifestRegistry::register_owned_manifest` and the `static` builtin
/// tables) so the `get`/`builtins` API keeps returning `&'static` — the
/// `Arc<str>` payloads live as long as that leaked shell.
#[derive(Debug, Clone)]
pub struct ProviderManifest {
    pub slug: Arc<str>,
    pub display_name: Arc<str>,
    pub family: ModelFamily,
    pub supports_thinking: bool,
    pub accepts_arbitrary_models: bool,
    pub fallback_max_output: Option<u32>,
    pub fallback_context_window: u32,
    pub models: &'static [ModelEntry],
    pub qualities: Option<Box<[Arc<str>]>>,
}

static ANTHROPIC: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("anthropic"),
    display_name: Arc::from("Anthropic"),
    family: ModelFamily::Claude,
    supports_thinking: true,
    accepts_arbitrary_models: false,
    fallback_max_output: Some(128_000),
    fallback_context_window: 200_000,
    models: anthropic::models(),
    qualities: None,
});

static OPENAI: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("openai"),
    display_name: Arc::from("OpenAI"),
    family: ModelFamily::Gpt,
    supports_thinking: true,
    accepts_arbitrary_models: false,
    fallback_max_output: Some(100_000),
    fallback_context_window: 200_000,
    models: openai::models(),
    qualities: None,
});

static GOOGLE: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("google"),
    display_name: Arc::from("Google"),
    family: ModelFamily::Gemini,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(65_536),
    fallback_context_window: 1_000_000,
    models: google::models(),
    qualities: None,
});

static COPILOT: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("copilot"),
    display_name: Arc::from("Copilot"),
    family: ModelFamily::Generic,
    supports_thinking: false,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(100_000),
    fallback_context_window: 200_000,
    models: copilot::models(),
    qualities: None,
});

static OLLAMA: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("ollama"),
    display_name: Arc::from("Ollama"),
    family: ModelFamily::Generic,
    supports_thinking: false,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(16_384),
    fallback_context_window: 128_000,
    models: ollama::models(),
    qualities: None,
});

static LLAMA_CPP: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("llama-cpp"),
    display_name: Arc::from("LlamaCpp"),
    family: ModelFamily::Generic,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: None,
    fallback_context_window: 128_000,
    models: llama_cpp::models(),
    qualities: None,
});

static MISTRAL: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("mistral"),
    display_name: Arc::from("Mistral"),
    family: ModelFamily::Generic,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(32_000),
    fallback_context_window: 128_000,
    models: mistral::models(),
    qualities: None,
});

static ZAI: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("zai"),
    display_name: Arc::from("Z.AI"),
    family: ModelFamily::Glm,
    supports_thinking: false,
    accepts_arbitrary_models: false,
    fallback_max_output: Some(16_000),
    fallback_context_window: 128_000,
    models: zai::models(),
    qualities: None,
});

static OPENROUTER: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("openrouter"),
    display_name: Arc::from("OpenRouter"),
    family: ModelFamily::Generic,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(128_000),
    fallback_context_window: 200_000,
    models: openrouter::models(),
    qualities: None,
});

static SYNTHETIC: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("synthetic"),
    display_name: Arc::from("Synthetic"),
    family: ModelFamily::Synthetic,
    supports_thinking: true,
    accepts_arbitrary_models: false,
    fallback_max_output: Some(32_000),
    fallback_context_window: 128_000,
    models: synthetic::models(),
    qualities: None,
});

static TENSORX: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("tensorx"),
    display_name: Arc::from("TensorX"),
    family: ModelFamily::Generic,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: None,
    fallback_context_window: 200_000,
    models: tensorx::models(),
    qualities: None,
});

static OPENCODE: LazyLock<ProviderManifest> = LazyLock::new(|| ProviderManifest {
    slug: Arc::from("opencode"),
    display_name: Arc::from("Opencode"),
    family: ModelFamily::Generic,
    supports_thinking: true,
    accepts_arbitrary_models: true,
    fallback_max_output: Some(128_000),
    fallback_context_window: 256_000,
    models: &[],
    qualities: None,
});

static BUILTINS: LazyLock<Vec<&'static ProviderManifest>> = LazyLock::new(|| {
    vec![
        &*ANTHROPIC,
        &*OPENAI,
        &*GOOGLE,
        &*COPILOT,
        &*OLLAMA,
        &*LLAMA_CPP,
        &*MISTRAL,
        &*ZAI,
        &*OPENROUTER,
        &*SYNTHETIC,
        &*TENSORX,
        &*OPENCODE,
    ]
});

/// Runtime-owned manifests registered by the Lua loader at boot (e.g.
/// DeepSeek). Stored as leaked `&'static ProviderManifest` shells whose
/// `Arc<str>` field data stays alive for as long as the shell, so `get`/
/// `builtins` keep their `&'static` return types without borrowing through
/// the mutex guard. Deduped by slug so re-registration never duplicates.
static OWNED_MANIFESTS: OnceLock<Mutex<Vec<&'static ProviderManifest>>> = OnceLock::new();

fn owned_manifests() -> &'static Mutex<Vec<&'static ProviderManifest>> {
    OWNED_MANIFESTS.get_or_init(|| Mutex::new(Vec::new()))
}

pub struct ManifestRegistry;

impl ManifestRegistry {
    pub fn register_owned_manifest(m: ProviderManifest) {
        let manifest: &'static ProviderManifest = Box::leak(Box::new(m));
        let mut guard = owned_manifests().lock().unwrap();
        if let Some(slot) = guard
            .iter_mut()
            .find(|existing| existing.slug == manifest.slug)
        {
            *slot = manifest;
        } else {
            guard.push(manifest);
        }
    }

    pub fn get(slug: &str) -> Option<&'static ProviderManifest> {
        BUILTINS
            .iter()
            .copied()
            .find(|m| m.slug.as_ref() == slug)
            .or_else(|| {
                owned_manifests()
                    .lock()
                    .unwrap()
                    .iter()
                    .copied()
                    .find(|m| m.slug.as_ref() == slug)
            })
    }

    /// Like `get`, but resolves dynamic and custom (providers.toml) slugs to
    /// their base provider's manifest so capability lookups (thinking, display
    /// name, tier defaults) still work for stubs that declare no models. `None`
    /// for an unknown slug, so callers pick a fallback instead of silently
    /// inheriting a zeroed manifest.
    pub fn for_slug(slug: &str) -> Option<&'static ProviderManifest> {
        Self::get(slug)
            .or_else(|| dynamic::base_for_slug(slug).and_then(|base| Self::get(base)))
            .or_else(|| custom::base_kind(slug).and_then(|base| Self::get(&base.to_string())))
    }

    pub fn builtins() -> Vec<&'static ProviderManifest> {
        BUILTINS
            .iter()
            .copied()
            .chain(owned_manifests().lock().unwrap().iter().copied())
            .collect()
    }

    pub fn find_default_for_tier(slug: &str, tier: ModelTier) -> Option<&'static ModelEntry> {
        Self::for_slug(slug)?
            .models
            .iter()
            .find(|e| e.default && e.tier == tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderKind;
    use maki_config::providers::BuiltInProvider;
    use std::str::FromStr;

    #[test]
    fn every_builtin_manifest_matches_provider_kind_for_mirrored_fields() {
        for manifest in BUILTINS.iter().copied() {
            let slug = manifest.slug.as_ref();
            let kind = ProviderKind::from_str(slug)
                .unwrap_or_else(|_| panic!("manifest slug {slug} has no ProviderKind"));
            assert_eq!(kind.to_string(), slug);
            assert_eq!(manifest.display_name.as_ref(), kind.display_name());
            assert_eq!(manifest.family, kind.family());
            assert_eq!(manifest.fallback_max_output, kind.fallback_max_output());
            assert_eq!(
                manifest.fallback_context_window,
                kind.fallback_context_window()
            );
        }
    }

    #[test]
    fn for_slug_returns_none_for_unknown_slug() {
        assert!(ManifestRegistry::for_slug("totally-unknown-slug").is_none());
    }

    #[test]
    fn provider_kind_slug_matches_strum_display() {
        use strum::IntoEnumIterator;
        for kind in ProviderKind::iter() {
            assert_eq!(
                kind.slug(),
                kind.to_string(),
                "ProviderKind::slug() drifted from strum Display for {:?}",
                kind,
            );
        }
    }

    #[test]
    fn for_slug_returns_builtin_directly() {
        let manifest = ManifestRegistry::for_slug("anthropic").unwrap();
        assert_eq!(manifest.slug.as_ref(), "anthropic");
        assert_eq!(manifest.display_name.as_ref(), "Anthropic");
    }

    #[test]
    fn every_builtin_manifest_has_provider_kind() {
        for manifest in BUILTINS.iter().copied() {
            let slug = manifest.slug.as_ref();
            assert!(
                ProviderKind::from_str(slug).is_ok(),
                "manifest slug {slug} has no matching ProviderKind",
            );
        }
    }

    #[test]
    fn every_builtin_provider_inventory_entry_has_matching_manifest() {
        for builtin in inventory::iter::<BuiltInProvider>() {
            let manifest = ManifestRegistry::get(builtin.slug).unwrap_or_else(|| {
                panic!(
                    "BuiltInProvider slug {:?} has no ProviderManifest",
                    builtin.slug,
                )
            });
            assert_eq!(
                manifest.display_name.as_ref(),
                builtin.display_name,
                "display_name mismatch between manifest and BuiltInProvider for slug {:?}",
                builtin.slug,
            );
        }
    }
}
