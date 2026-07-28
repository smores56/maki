use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use flume::Sender;
use serde_json::Value;
use tracing::{debug, warn};

use maki_storage::id::SessionRef;

use crate::model::{Model, ModelInfo};
use crate::providers::Timeouts;
use crate::providers::catalog::{
    OPENCODE_FAMILY_SLUGS, available_if_warm, catalog_providers, catalog_providers_if_available,
};
use crate::providers::dynamic;
use crate::{AgentError, Message, ProviderEvent, ProviderUsage, RequestOptions, StreamResponse, registry};


pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Provider: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>>;

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>>;

    /// Fetch provider-side usage quota (remaining percentage / reset times).
    /// `Ok(None)` means the provider does not expose a programmatic usage endpoint.
    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async { Ok(None) })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }

    fn adjust_model(&self, _model: &mut Model) {}
}

pub fn provider_for_slug(slug: &str, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    if let Some(spec) = registry::get(slug) {
        return (spec.build)(&spec, timeouts);
    }
    if dynamic::display_name(slug).is_some() {
        return dynamic::create(slug, timeouts);
    }
    if crate::providers::custom::base_kind(slug).is_some() {
        return crate::providers::custom::create(slug, timeouts);
    }
    if let Some(catalog) = crate::providers::catalog::try_create(slug, timeouts) {
        return catalog;
    }
    Err(AgentError::Config {
        message: format!("unknown provider '{slug}'"),
    })
}

pub fn provider_available(slug: &str) -> bool {
    provider_for_slug(slug, Timeouts::default()).is_ok()
}

/// Non-blocking variant of [`provider_available`] for offline model discovery:
/// catalog-backed slugs consult only the already-warm catalog, so a cold cache
/// reports them unavailable instead of blocking on a network fetch.
fn provider_available_offline(slug: &str) -> bool {
    if registry::get(slug).is_some()
        || dynamic::display_name(slug).is_some()
        || crate::providers::custom::base_kind(slug).is_some()
    {
        return provider_available(slug);
    }
    available_if_warm(slug)
}

pub fn from_model(model: &mut Model, timeouts: Timeouts) -> Result<Box<dyn Provider>, AgentError> {
    let provider = provider_for_slug(&model.provider, timeouts)?;
    provider.adjust_model(model);
    debug!(provider = %model.provider, model = %model.id, "provider created");
    Ok(provider)
}

pub fn from_model_fallback(model: &mut Model, timeouts: Timeouts) -> Box<dyn Provider> {
    match from_model(model, timeouts) {
        Ok(provider) => provider,
        Err(e) => {
            warn!(error = %e, "provider creation failed, using unconfigured provider");
            Box::new(UnconfiguredProvider)
        }
    }
}

struct UnconfiguredProvider;

const NOT_CONFIGURED: &str = "no provider configured — run /login or `maki auth login`";

impl Provider for UnconfiguredProvider {
    fn stream_message<'a>(
        &'a self,
        _model: &'a Model,
        _messages: &'a [Message],
        _system: &'a str,
        _tools: &'a Value,
        _event_tx: &'a Sender<ProviderEvent>,
        _opts: RequestOptions,
        _session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async {
            Err(AgentError::Config {
                message: NOT_CONFIGURED.to_string(),
            })
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async {
            Err(AgentError::Config {
                message: NOT_CONFIGURED.to_string(),
            })
        })
    }
}

pub async fn from_model_async(
    model: &mut Model,
    timeouts: Timeouts,
) -> Result<Box<dyn Provider>, AgentError> {
    let slug = Arc::clone(&model.provider);
    let id = model.id.clone();
    let provider = smol::unblock(move || provider_for_slug(&slug, timeouts)).await?;
    provider.adjust_model(model);
    debug!(provider = %model.provider, model = %id, "provider created");
    Ok(provider)
}

pub struct ModelBatch {
    pub models: Vec<String>,
    pub warnings: Vec<String>,
}

/// Offline version of model discovery: returns specs from static tables
/// and configured dynamic providers. See [`fetch_all_models`] for live lookups.
/// Never blocks on catalog download; catalog-backed providers appear only once
/// the catalog has warmed in the background.
pub fn available_model_specs() -> Vec<String> {
    let mut specs: Vec<String> = registry::all()
        .into_iter()
        .filter(|s| provider_available_offline(s.slug.as_ref()))
        .flat_map(|s| {
            let slug = Arc::clone(&s.slug);
            s.models
                .iter()
                .flat_map(|entry| entry.prefixes.iter().cloned())
                .map(move |p| format!("{slug}/{p}"))
                .collect::<Vec<_>>()
        })
        .collect();
    for slug in dynamic::discovered_slugs() {
        specs.extend(dynamic::dynamic_model_specs_for(slug));
    }
    for spec in crate::providers::custom::declared_model_specs() {
        if !specs.contains(&spec) {
            specs.push(spec);
        }
    }
    if let Some(catalog) = catalog_providers_if_available() {
        for cat in catalog {
            if registry::get(&cat.slug).is_some()
                || dynamic::base_for_slug(&cat.slug).is_some()
                || crate::providers::custom::base_kind(&cat.slug).is_some()
                || OPENCODE_FAMILY_SLUGS.contains(&cat.slug.as_str())
            {
                continue;
            }
            if !provider_available(&cat.slug) {
                continue;
            }
            for model_id in cat.models.keys() {
                let spec = format!("{}/{}", cat.slug, model_id);
                if !specs.contains(&spec) {
                    specs.push(spec);
                }
            }
        }
    }
    specs
}

pub async fn fetch_all_models(
    mut on_ready: impl FnMut(ModelBatch),
    on_done: Option<Box<dyn FnOnce() + Send>>,
) {
    let (tx, rx) = flume::unbounded();
    let timeouts = Timeouts::default();

    for spec in registry::all() {
        let slug: Arc<str> = Arc::clone(&spec.slug);
        let slug_for_create = Arc::clone(&slug);
        let Ok(provider) =
            smol::unblock(move || provider_for_slug(&slug_for_create, timeouts)).await
        else {
            warn!(provider = %slug, "failed to create provider, skipping");
            continue;
        };
        let display_name = spec.display_name.clone();
        let accepts_arbitrary = spec.accepts_arbitrary_models;
        let static_models = spec.models.clone();
        let tx = tx.clone();
        smol::spawn(async move {
            let batch = match provider.list_models().await {
                Ok(models) => {
                    if accepts_arbitrary {
                        crate::model_registry::model_registry()
                            .write()
                            .unwrap()
                            .set_known_models(&slug, models.clone());
                    }
                    let mut specs: Vec<String> =
                        models.iter().map(|m| format!("{slug}/{}", m.id)).collect();
                    for entry in &static_models {
                        for prefix in &entry.prefixes {
                            let spec = format!("{slug}/{prefix}");
                            if !specs.contains(&spec) {
                                specs.push(spec);
                            }
                        }
                    }
                    ModelBatch {
                        models: specs,
                        warnings: Vec::new(),
                    }
                }
                Err(e) => {
                    warn!(provider = %slug, error = %e, "failed to list models, using static fallback");
                    let fallback: Vec<String> = static_models
                        .iter()
                        .flat_map(|entry| entry.prefixes.iter())
                        .map(|p| format!("{slug}/{p}"))
                        .collect();
                    ModelBatch {
                        models: fallback,
                        warnings: vec![format!(
                            "{display_name}: {e} (using static fallback)"
                        )],
                    }
                }
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }

    for slug in dynamic::discovered_slugs() {
        let tx = tx.clone();
        let slug = slug.to_string();
        smol::spawn(async move {
            let static_fallback = |reason: String| {
                warn!(
                    slug,
                    error = reason,
                    "dynamic model listing failed, using static fallback"
                );
                ModelBatch {
                    models: dynamic::dynamic_model_specs_for(&slug),
                    warnings: vec![format!("{slug}: {reason} (using static fallback)")],
                }
            };
            let batch = match dynamic::create(&slug, timeouts) {
                Ok(provider) => match provider.list_models().await {
                    Ok(models) => ModelBatch {
                        models: models.iter().map(|m| format!("{slug}/{}", m.id)).collect(),
                        warnings: Vec::new(),
                    },
                    Err(e) => static_fallback(e.to_string()),
                },
                Err(e) => static_fallback(e.to_string()),
            };
            let _ = tx.send_async(batch).await;
        })
        .detach();
    }

    let tx_catalog = tx.clone();
    smol::spawn(async move {
        let catalog = smol::unblock(catalog_providers).await;
        for cat in catalog {
            if registry::get(&cat.slug).is_some()
                || dynamic::base_for_slug(&cat.slug).is_some()
                || OPENCODE_FAMILY_SLUGS.contains(&cat.slug.as_str())
            {
                continue;
            }
            if !provider_available(&cat.slug) {
                continue;
            }
            let slug = cat.slug;
            let models: Vec<String> = cat.models.keys().map(|id| format!("{slug}/{id}")).collect();
            let _ = tx_catalog
                .send_async(ModelBatch {
                    models,
                    warnings: Vec::new(),
                })
                .await;
        }
    })
    .detach();

    let custom_timeouts = timeouts;
    let tx_custom = tx.clone();
    smol::spawn(async move {
        let declared = crate::providers::custom::declared_model_specs();
        if !declared.is_empty() {
            let _ = tx_custom
                .send_async(ModelBatch {
                    models: declared,
                    warnings: Vec::new(),
                })
                .await;
        }
        let custom_specs =
            smol::unblock(move || crate::providers::custom::discover_models(custom_timeouts)).await;
        if !custom_specs.is_empty() {
            let _ = tx_custom
                .send_async(ModelBatch {
                    models: custom_specs,
                    warnings: Vec::new(),
                })
                .await;
        }
    })
    .detach();

    drop(tx);

    while let Ok(batch) = rx.recv_async().await {
        on_ready(batch);
    }
    if let Some(done) = on_done {
        done();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_for_slug_unknown_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        crate::providers::catalog::warm_empty_catalog_for_tests(maki_storage::StateDir::from_path(
            tmp.path().to_path_buf(),
        ));
        let result = provider_for_slug("nonexistent-provider-xyz", Timeouts::default());
        match result {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("unknown provider"),
                    "expected 'unknown provider' message, got: {msg}"
                );
            }
            Ok(_) => panic!("expected error for unknown provider"),
        }
    }
}
