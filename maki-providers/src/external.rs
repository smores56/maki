use std::sync::{Arc, Mutex, OnceLock};

use flume::Sender;
use maki_storage::StateDir;
use maki_storage::id::SessionRef;
use serde_json::Value;
use tracing::debug;
use tracing::warn;

use crate::AgentError;
use crate::auth_source::{AuthSource, EnvAuthSource, OAuthAuthSource};
use crate::manifest::{AuthKind, ManifestRegistry};
use crate::model::{Model, ModelInfo};
use crate::provider::{BoxFuture, Provider};
use crate::providers::oauth::oauth_config;
use crate::providers::ResolvedAuth;
use crate::providers::{
    anthropic, deepseek, google, mistral, openrouter, synthetic, tensorx, zai, KeyPool, Timeouts,
};
use crate::providers::openai::OpenAi;
use crate::{Message, ProviderEvent, ProviderUsage, RequestOptions, StreamResponse};

pub struct ExternalProvider {
    slug: &'static str,
    auth: Arc<Mutex<ResolvedAuth>>,
    auth_source: Box<dyn AuthSource>,
    engine: OnceLock<Box<dyn Provider>>,
    timeouts: Timeouts,
}

impl ExternalProvider {
    pub fn try_for_slug(
        slug: &str,
        timeouts: Timeouts,
    ) -> Result<Option<Box<dyn Provider>>, AgentError> {
        let Some(manifest) = ManifestRegistry::get(slug) else {
            return Ok(None);
        };
        let provider = match manifest.auth_kind {
            AuthKind::Env => build_env(manifest.slug, timeouts)?,
            AuthKind::OAuth => build_oauth(manifest.slug, timeouts)?,
        };
        Ok(Some(Box::new(provider)))
    }

    fn engine_sync(&self) -> Result<&dyn Provider, AgentError> {
        if let Some(engine) = self.engine.get() {
            return Ok(engine.as_ref());
        }
        self.auth_source.resolve(&self.auth)?;
        let built = build_engine(self.slug, self.auth.clone(), self.timeouts)?;
        if self.engine.set(built).is_err() {
            // Another caller won the race; use its engine.
            return Ok(self
                .engine
                .get()
                .expect("engine set by concurrent init")
                .as_ref());
        }
        debug!(slug = self.slug, "built external provider engine");
        Ok(self.engine.get().expect("just set").as_ref())
    }
}

fn build_env(slug: &'static str, timeouts: Timeouts) -> Result<ExternalProvider, AgentError> {
    let auth_source: Box<dyn AuthSource> = match slug {
        "anthropic" => Box::new(EnvAuthSource::new(
            "anthropic",
            anthropic::ENV_VAR,
            anthropic::resolve_auth_from_key,
        )),
        "google" => Box::new(EnvAuthSource::new(
            "google",
            google::ENV_VAR,
            google::resolve_auth_from_key,
        )),
        "mistral" => Box::new(EnvAuthSource::new(
            "mistral",
            mistral::CONFIG.api_key_env,
            ResolvedAuth::bearer,
        )),
        "deepseek" => Box::new(EnvAuthSource::new(
            "deepseek",
            deepseek::CONFIG.api_key_env,
            ResolvedAuth::bearer,
        )),
        "zai" => Box::new(EnvAuthSource::new(
            "zai",
            zai::CONFIG_STANDARD.api_key_env,
            ResolvedAuth::bearer,
        )),
        "synthetic" => Box::new(EnvAuthSource::new(
            "synthetic",
            synthetic::CONFIG.api_key_env,
            ResolvedAuth::bearer,
        )),
        "tensorx" => Box::new(EnvAuthSource::new(
            "tensorx",
            tensorx::CONFIG.api_key_env,
            ResolvedAuth::bearer,
        )),
        "openrouter" => Box::new(EnvAuthSource::with_resolver(
            "openrouter",
            openrouter::CONFIG.api_key_env,
            ResolvedAuth::bearer,
            env_only,
        )),
        other => {
            return Err(AgentError::Config {
                message: format!("no env auth source registered for '{other}'"),
            });
        }
    };
    Ok(ExternalProvider {
        slug,
        auth: Arc::new(Mutex::new(ResolvedAuth {
            base_url: None,
            headers: Vec::new(),
        })),
        auth_source,
        engine: OnceLock::new(),
        timeouts,
    })
}

fn build_oauth(slug: &'static str, timeouts: Timeouts) -> Result<ExternalProvider, AgentError> {
    let cfg = oauth_config(slug).ok_or_else(|| AgentError::Config {
        message: format!("no OAuth config registered for '{slug}'"),
    })?;
    let dir = StateDir::resolve()?;
    let auth_source: Box<dyn AuthSource> = Box::new(OAuthAuthSource::new(cfg, dir));
    Ok(ExternalProvider {
        slug,
        auth: Arc::new(Mutex::new(ResolvedAuth {
            base_url: None,
            headers: Vec::new(),
        })),
        auth_source,
        engine: OnceLock::new(),
        timeouts,
    })
}

fn env_only(_slug: &'static str, env_var: &'static str) -> Result<KeyPool, AgentError> {
    KeyPool::from_env(env_var)
}

fn build_engine(
    slug: &str,
    auth: Arc<Mutex<ResolvedAuth>>,
    timeouts: Timeouts,
) -> Result<Box<dyn Provider>, AgentError> {
    let prefix = ManifestRegistry::get(slug)
        .and_then(|m| m.system_prefix)
        .map(str::to_string);
    match slug {
        "openai" => Ok(Box::new(
            OpenAi::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "anthropic" => Ok(Box::new(
            anthropic::Anthropic::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "google" => Ok(Box::new(google::Google::with_auth(auth, timeouts))),
        "mistral" => Ok(Box::new(
            mistral::Mistral::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "deepseek" => Ok(Box::new(
            deepseek::DeepSeek::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "zai" => Ok(Box::new(
            zai::Zai::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "synthetic" => Ok(Box::new(
            synthetic::Synthetic::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "tensorx" => Ok(Box::new(
            tensorx::TensorX::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        "openrouter" => Ok(Box::new(
            openrouter::OpenRouter::with_auth(auth, timeouts).with_system_prefix(prefix),
        )),
        other => Err(AgentError::Config {
            message: format!("no external engine builder for '{other}'"),
        }),
    }
}

impl Provider for ExternalProvider {
    fn stream_message<'a>(
        &'a self,
        model: &'a Model,
        messages: &'a [Message],
        system: &'a str,
        tools: &'a Value,
        event_tx: &'a Sender<ProviderEvent>,
        opts: RequestOptions,
        session_id: Option<&'a SessionRef>,
    ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
        Box::pin(async move {
            let engine = self.engine_sync()?;
            let result = engine
                .stream_message(model, messages, system, tools, event_tx, opts, session_id)
                .await;
            if matches!(&result, Err(e) if e.is_auth_error()) && self.auth_source.is_oauth() {
                if self.auth_source.refresh(&self.auth).await.is_ok() {
                    return engine
                        .stream_message(model, messages, system, tools, event_tx, opts, session_id)
                        .await;
                }
                warn!(slug = self.slug, "auth refresh failed, surfacing original error");
            }
            result
        })
    }

    fn list_models(&self) -> BoxFuture<'_, Result<Vec<ModelInfo>, AgentError>> {
        Box::pin(async move {
            let engine = self.engine_sync()?;
            engine.list_models().await
        })
    }

    fn fetch_usage(&self) -> BoxFuture<'_, Result<Option<ProviderUsage>, AgentError>> {
        Box::pin(async move {
            let engine = self.engine_sync()?;
            engine.fetch_usage().await
        })
    }

    fn refresh_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async move { self.auth_source.refresh(&self.auth).await })
    }

    fn reload_auth(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { self.auth_source.reload(&self.auth) })
    }

    fn rotate_key(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { self.auth_source.rotate_key(&self.auth) })
    }

    fn adjust_model(&self, model: &mut Model) {
        if let Ok(engine) = self.engine_sync() {
            engine.adjust_model(model);
        }
    }
}
