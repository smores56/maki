use maki_config::providers::{Protocol, ProviderDef};

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProviderPlan {
    pub display_name: &'static str,
    pub base_url: &'static str,
    pub default_model: Option<&'static str>,
    pub login_url: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuiltInProvider {
    pub slug: &'static str,
    pub display_name: &'static str,
    pub protocol: maki_config::providers::Protocol,
    pub default_base_url: &'static str,
    pub default_api_key_env: &'static str,
    pub default_model: &'static str,
    pub plans: Option<&'static [(&'static str, ProviderPlan)]>,
    pub login_url: Option<&'static str>,
    pub needs_url: bool,
}

inventory::collect!(BuiltInProvider);

pub fn builtin_provider(slug: &str) -> Option<&'static BuiltInProvider> {
    inventory::iter::<BuiltInProvider>()
        .into_iter()
        .find(|p| p.slug == slug)
}

pub fn all_builtins() -> Vec<&'static BuiltInProvider> {
    inventory::iter::<BuiltInProvider>().collect()
}

pub fn resolve_api_key_env(slug: &str, def: Option<&ProviderDef>) -> String {
    if let Some(d) = def
        && let Some(env) = &d.api_key_env
    {
        return env.clone();
    }
    if let Some(builtin) = builtin_provider(slug) {
        return builtin.default_api_key_env.to_string();
    }
    format!("{}_API_KEY", slug.to_uppercase().replace('-', "_"))
}

pub fn base_url_env_var(slug: &str) -> String {
    format!("{}_BASE_URL", slug.to_uppercase().replace('-', "_"))
}

pub fn base_url_override(slug: &str) -> Option<String> {
    std::env::var(base_url_env_var(slug))
        .ok()
        .filter(|url| !url.is_empty())
}

pub fn configured_base_url(slug: &str, def: Option<&ProviderDef>) -> Option<String> {
    if let Some(url) = base_url_override(slug) {
        return Some(url);
    }
    let def = def?;
    if let Some(url) = &def.base_url {
        return Some(url.clone());
    }
    let plan_name = def.plan.as_ref()?;
    builtin_provider(slug)?
        .plans?
        .iter()
        .find(|(key, _)| key == plan_name)
        .map(|(_, plan)| plan.base_url.to_string())
}

pub fn resolve_base_url(slug: &str, def: Option<&ProviderDef>) -> Option<String> {
    configured_base_url(slug, def)
        .or_else(|| builtin_provider(slug).map(|b| b.default_base_url.to_string()))
}

pub fn resolve_protocol(slug: &str, def: Option<&ProviderDef>) -> Option<Protocol> {
    if let Some(d) = def
        && let Some(p) = &d.protocol
    {
        return Some(*p);
    }
    builtin_provider(slug).map(|b| b.protocol)
}

pub fn resolve_display_name(slug: &str, def: Option<&ProviderDef>) -> String {
    if let Some(d) = def
        && let Some(name) = &d.display_name
    {
        return name.clone();
    }
    builtin_provider(slug)
        .map(|b| b.display_name.to_string())
        .unwrap_or_else(|| slug.to_string())
}

pub fn resolve_default_model(slug: &str, def: Option<&ProviderDef>) -> Option<String> {
    if let Some(d) = def {
        if let Some(m) = &d.default_model {
            return Some(m.clone());
        }
        if let Some(plan_name) = &d.plan
            && let Some(builtin) = builtin_provider(slug)
            && let Some(plans) = builtin.plans
        {
            for (key, plan) in plans {
                if key == plan_name
                    && let Some(m) = &plan.default_model
                {
                    return Some(m.to_string());
                }
            }
        }
    }
    builtin_provider(slug).map(|b| b.default_model.to_string())
}

pub fn resolve_login_url(slug: &str, plan: Option<&str>) -> Option<String> {
    if let Some(plan_name) = plan
        && let Some(builtin) = builtin_provider(slug)
        && let Some(plans) = builtin.plans
    {
        for (key, plan) in plans {
            if *key == plan_name
                && let Some(url) = plan.login_url
            {
                return Some(url.to_string());
            }
        }
    }
    builtin_provider(slug).and_then(|b| b.login_url.map(|u| u.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("anthropic", None => "ANTHROPIC_API_KEY".to_string(); "builtin_default")]
    #[test_case("my-custom", None => "MY_CUSTOM_API_KEY".to_string(); "custom_default")]
    fn resolve_api_key_env_tests(slug: &str, def: Option<&ProviderDef>) -> String {
        resolve_api_key_env(slug, def)
    }

    #[test]
    fn resolve_base_url_prefers_def_over_none() {
        let slug = "maki-test-def-over-none-slug";
        let def = ProviderDef {
            base_url: Some("http://proxy.local/v1".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_base_url(slug, Some(&def)).as_deref(),
            Some("http://proxy.local/v1")
        );
        assert_ne!(
            resolve_base_url(slug, Some(&def)),
            resolve_base_url(slug, None)
        );
    }

    #[test]
    fn resolve_base_url_empty_def_matches_none() {
        let slug = "maki-test-empty-def-slug";
        let def = ProviderDef::default();
        assert_eq!(
            resolve_base_url(slug, Some(&def)),
            resolve_base_url(slug, None)
        );
    }

    #[test]
    fn resolve_base_url_custom_slug_uses_def() {
        let slug = "maki-test-custom-base-url-slug";
        let def = ProviderDef {
            base_url: Some("http://xxxx:1234/v1".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_base_url(slug, Some(&def)).as_deref(),
            Some("http://xxxx:1234/v1")
        );
        assert_eq!(resolve_base_url(slug, None), None);
    }

    #[test]
    fn resolve_base_url_env_beats_def() {
        let slug = "maki-test-env-base-url-slug";
        let env_var = base_url_env_var(slug);
        unsafe {
            std::env::set_var(&env_var, "http://env.local/v1");
        }
        let def = ProviderDef {
            base_url: Some("http://toml.local/v1".into()),
            ..Default::default()
        };
        let got = resolve_base_url(slug, Some(&def));
        unsafe {
            std::env::remove_var(&env_var);
        }
        assert_eq!(got.as_deref(), Some("http://env.local/v1"));
    }
}
