use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tracing::debug;

use crate::AgentError;
use crate::providers::KeyPool;
use crate::providers::ResolvedAuth;
use crate::providers::oauth::OAuthConfig;

pub(crate) type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait AuthSource: Send + Sync {
    fn resolve<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>>;
    fn reload<'a>(
        &'a self,
        _auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
    fn refresh<'a>(
        &'a self,
        _auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
    fn rotate_key<'a>(
        &'a self,
        _auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
    fn is_oauth(&self) -> bool {
        false
    }
}

pub struct EnvAuthSource {
    slug: &'static str,
    env_var: &'static str,
    build: fn(&str) -> ResolvedAuth,
    resolve_pool: fn(&'static str, &'static str) -> Result<KeyPool, AgentError>,
    pool: Mutex<Option<KeyPool>>,
}

impl EnvAuthSource {
    pub fn new(
        slug: &'static str,
        env_var: &'static str,
        build: fn(&str) -> ResolvedAuth,
    ) -> Self {
        Self::with_resolver(slug, env_var, build, full_resolve)
    }

    pub fn with_resolver(
        slug: &'static str,
        env_var: &'static str,
        build: fn(&str) -> ResolvedAuth,
        resolve_pool: fn(&'static str, &'static str) -> Result<KeyPool, AgentError>,
    ) -> Self {
        Self {
            slug,
            env_var,
            build,
            resolve_pool,
            pool: Mutex::new(None),
        }
    }

    fn pool(&self) -> Result<KeyPool, AgentError> {
        let mut guard = self.pool.lock().unwrap();
        if let Some(pool) = guard.as_ref() {
            return Ok(pool.clone());
        }
        let pool = (self.resolve_pool)(self.slug, self.env_var)?;
        *guard = Some(pool.clone());
        Ok(pool)
    }
}

impl AuthSource for EnvAuthSource {
    fn resolve<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            *auth.lock().unwrap() = (self.build)(pool.current());
            debug!(slug = self.slug, keys = pool.len(), "resolved env auth");
            Ok(())
        })
    }

    fn reload<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let pool = (self.resolve_pool)(self.slug, self.env_var)?;
            *self.pool.lock().unwrap() = Some(pool.clone());
            *auth.lock().unwrap() = (self.build)(pool.current());
            debug!(slug = self.slug, "reloaded env auth");
            Ok(())
        })
    }

    fn rotate_key<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<bool, AgentError>> {
        Box::pin(async move {
            let pool = self.pool()?;
            Ok(pool.rotate_auth(auth, self.build))
        })
    }
}

fn full_resolve(slug: &'static str, env_var: &'static str) -> Result<KeyPool, AgentError> {
    KeyPool::resolve(slug, env_var)
}

pub struct OAuthAuthSource {
    cfg: &'static OAuthConfig,
    dir: maki_storage::StateDir,
}

impl OAuthAuthSource {
    pub fn new(cfg: &'static OAuthConfig, dir: maki_storage::StateDir) -> Self {
        Self { cfg, dir }
    }
}

impl AuthSource for OAuthAuthSource {
    fn resolve<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let cfg = self.cfg;
            let dir = self.dir.clone();
            let resolved = smol::unblock(move || crate::providers::oauth::resolve(cfg, &dir)).await?;
            *auth.lock().unwrap() = resolved;
            Ok(())
        })
    }

    fn reload<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let cfg = self.cfg;
            let dir = self.dir.clone();
            let resolved = smol::unblock(move || crate::providers::oauth::resolve(cfg, &dir)).await?;
            *auth.lock().unwrap() = resolved;
            debug!(provider = cfg.provider, "reloaded OAuth auth");
            Ok(())
        })
    }

    fn refresh<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> AuthFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            let cfg = self.cfg;
            let dir = self.dir.clone();
            let resolved = smol::unblock(move || -> Result<ResolvedAuth, AgentError> {
                let tokens = maki_storage::auth::load_tokens(&dir, cfg.provider).ok_or_else(|| {
                    AgentError::Api {
                        status: 401,
                        message: format!("{} OAuth tokens not found on disk", cfg.provider),
                    }
                })?;
                match crate::providers::oauth::refresh_tokens(cfg, &tokens) {
                    Ok(fresh) => {
                        maki_storage::auth::save_tokens(&dir, cfg.provider, &fresh)?;
                        debug!(provider = cfg.provider, "refreshed OAuth tokens");
                        Ok(crate::providers::oauth::build_resolved(&fresh))
                    }
                    Err(e) => {
                        tracing::warn!(provider = cfg.provider, error = %e, "OAuth refresh failed, clearing stale tokens");
                        let _ = maki_storage::auth::delete_tokens(&dir, cfg.provider);
                        Err(e)
                    }
                }
            })
            .await?;
            *auth.lock().unwrap() = resolved;
            Ok(())
        })
    }

    fn is_oauth(&self) -> bool {
        true
    }
}
