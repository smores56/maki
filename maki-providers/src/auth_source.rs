use std::sync::{Arc, Mutex};

use tracing::debug;

use crate::AgentError;
use crate::providers::KeyPool;
use crate::providers::ResolvedAuth;

pub trait AuthSource: Send + Sync {
    fn resolve(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError>;
    fn reload(&self, _auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        Ok(())
    }
    fn refresh(&self, _auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        Ok(())
    }
    fn rotate_key(&self, _auth: &Arc<Mutex<ResolvedAuth>>) -> Result<bool, AgentError> {
        Ok(false)
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
    fn resolve(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        let pool = self.pool()?;
        *auth.lock().unwrap() = (self.build)(pool.current());
        debug!(slug = self.slug, keys = pool.len(), "resolved env auth");
        Ok(())
    }

    fn reload(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        let pool = (self.resolve_pool)(self.slug, self.env_var)?;
        *self.pool.lock().unwrap() = Some(pool.clone());
        *auth.lock().unwrap() = (self.build)(pool.current());
        debug!(slug = self.slug, "reloaded env auth");
        Ok(())
    }

    fn rotate_key(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<bool, AgentError> {
        let pool = self.pool()?;
        Ok(pool.rotate_auth(auth, self.build))
    }
}

fn full_resolve(slug: &'static str, env_var: &'static str) -> Result<KeyPool, AgentError> {
    KeyPool::resolve(slug, env_var)
}
