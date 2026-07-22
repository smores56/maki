use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use tracing::debug;

use crate::AgentError;
use crate::providers::KeyPool;
use crate::providers::ResolvedAuth;
use crate::providers::oauth::OAuthConfig;

type ProviderLocks = Mutex<HashMap<&'static str, Arc<smol::lock::Mutex<()>>>>;

// Per-process single-flight refresh lock, keyed by OAuth provider slug. The
// lock must outlive any single `OAuthAuthSource` instance: a fresh
// `ManifestProvider` for the same slug (e.g. cleared auth state forcing a new
// `build_oauth`) would otherwise get a brand-new `Mutex` and lose the
// serialization this lock exists to provide — two concurrent 401s would both
// load the same refresh token, race to save, and (if the issuer rotates
// refresh tokens) the loser would wipe the winner's freshly-stored tokens.
static OAUTH_REFRESH_LOCKS: LazyLock<ProviderLocks> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn oauth_refresh_lock(provider: &'static str) -> Arc<smol::lock::Mutex<()>> {
    let mut map = OAUTH_REFRESH_LOCKS.lock().unwrap();
    map.entry(provider)
        .or_insert_with(|| Arc::new(smol::lock::Mutex::new(())))
        .clone()
}

pub trait AuthSource: Send + Sync {
    // Sync signatures suffice for Env/OAuth (env var read, on-disk token load).
    // Only `refresh` is async because the HTTP token-refresh path blocks on
    // the network. A future Lua-backed AuthSource (over the Lua host thread)
    // would block the caller; resolve/reload must become async BoxFutures
    // before that impl lands.
    fn resolve(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError>;
    fn reload(&self, _auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        Ok(())
    }
    fn refresh<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> crate::provider::BoxFuture<'a, Result<(), AgentError>> {
        let _ = auth;
        Box::pin(async { Ok(()) })
    }
    fn rotate_key(&self, _auth: &Arc<Mutex<ResolvedAuth>>) -> Result<bool, AgentError> {
        Ok(false)
    }
    // `is_oauth` signals "on auth error, retry after refresh" to the centralized
    // `with_oauth_retry` in ManifestProvider. A future Lua-backed AuthSource
    // join may flip this to a trait method returning an enum (e.g.
    // `AuthKind::OAuth`), replacing the bool flag.
    fn is_oauth(&self) -> bool {
        false
    }
}

pub struct EnvAuthSource {
    slug: &'static str,
    env_var: &'static str,
    build: Arc<dyn Fn(&str) -> ResolvedAuth + Send + Sync>,
    resolve_pool: fn(&'static str, &'static str) -> Result<KeyPool, AgentError>,
    state: Mutex<EnvAuthState>,
}

#[derive(Default)]
struct EnvAuthState {
    pool: Option<KeyPool>,
    revision: u64,
}

struct PendingRotation {
    key: String,
    revision: u64,
}

impl EnvAuthState {
    fn install(&mut self, pool: KeyPool) {
        self.pool = Some(pool);
        self.revision = self.revision.wrapping_add(1);
    }

    fn prepare_rotation(&mut self) -> Option<PendingRotation> {
        let pool = self.pool.as_ref()?;
        if !pool.rotate() {
            return None;
        }
        self.revision = self.revision.wrapping_add(1);
        Some(PendingRotation {
            key: pool.current().to_owned(),
            revision: self.revision,
        })
    }

    fn is_current(&self, revision: u64) -> bool {
        self.revision == revision
    }
}

impl EnvAuthSource {
    pub(crate) fn new(
        slug: &'static str,
        env_var: &'static str,
        build: impl Fn(&str) -> ResolvedAuth + Send + Sync + 'static,
    ) -> Self {
        Self::with_resolver(slug, env_var, build, KeyPool::resolve)
    }

    pub(crate) fn with_resolver(
        slug: &'static str,
        env_var: &'static str,
        build: impl Fn(&str) -> ResolvedAuth + Send + Sync + 'static,
        resolve_pool: fn(&'static str, &'static str) -> Result<KeyPool, AgentError>,
    ) -> Self {
        Self {
            slug,
            env_var,
            build: Arc::new(build),
            resolve_pool,
            state: Mutex::default(),
        }
    }

    fn pool(&self) -> Result<KeyPool, AgentError> {
        let mut state = self.state.lock().unwrap();
        if let Some(pool) = state.pool.as_ref() {
            return Ok(pool.clone());
        }
        let pool = (self.resolve_pool)(self.slug, self.env_var)?;
        state.install(pool.clone());
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
        // Bump revision so any in-flight `prepare_rotation` whose key has not
        // yet been written to `auth` is invalidated before publish — its
        // captured revision no longer matches state.
        let key = pool.current().to_owned();
        {
            let mut state = self.state.lock().unwrap();
            state.install(pool);
        }
        *auth.lock().unwrap() = (self.build)(&key);
        debug!(slug = self.slug, "reloaded env auth");
        Ok(())
    }

    fn rotate_key(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<bool, AgentError> {
        let pending = {
            let mut state = self.state.lock().unwrap();
            state.prepare_rotation()
        };
        let Some(pending) = pending else {
            return Ok(false);
        };
        // Between prepare and publish, a reload may have replaced the pool
        // and bumped the revision. In that case the rotated key is stale —
        // surface no-op rather than overwriting fresh auth with a key from the
        // previous pool.
        let still_current = self.state.lock().unwrap().is_current(pending.revision);
        if still_current {
            *auth.lock().unwrap() = (self.build)(&pending.key);
        }
        Ok(still_current)
    }
}

pub struct OAuthAuthSource {
    cfg: &'static OAuthConfig,
    dir: maki_storage::StateDir,
    /// Serializes concurrent refresh attempts on the same provider. Without it,
    /// two simultaneous 401s both load the same refresh token; if the issuer
    /// rotates refresh tokens, the loser's request fails (invalid_grant) and
    /// would wipe the winner's freshly-saved tokens. Shared across all
    /// `OAuthAuthSource` instances for this slug (see `OAUTH_REFRESH_LOCKS`).
    refresh_lock: Arc<smol::lock::Mutex<()>>,
}

impl OAuthAuthSource {
    pub fn new(cfg: &'static OAuthConfig, dir: maki_storage::StateDir) -> Self {
        Self {
            cfg,
            dir,
            refresh_lock: oauth_refresh_lock(cfg.provider),
        }
    }
}

impl AuthSource for OAuthAuthSource {
    fn resolve(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        let resolved = crate::providers::oauth::resolve(self.cfg, &self.dir)?;
        *auth.lock().unwrap() = resolved;
        Ok(())
    }

    fn reload(&self, auth: &Arc<Mutex<ResolvedAuth>>) -> Result<(), AgentError> {
        let resolved = crate::providers::oauth::resolve(self.cfg, &self.dir)?;
        *auth.lock().unwrap() = resolved;
        debug!(provider = self.cfg.provider, "reloaded OAuth auth");
        Ok(())
    }

    fn refresh<'a>(
        &'a self,
        auth: &'a Arc<Mutex<ResolvedAuth>>,
    ) -> crate::provider::BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            // Single-flight: a concurrent 401 waits here while the first
            // caller refreshes, then re-loads whatever the first saved.
            let _guard = self.refresh_lock.lock().await;
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
                        Ok(crate::providers::oauth::build_resolved(cfg, &fresh))
                    }
                    Err(e) => {
                        // Only wipe stored tokens when the refresh token is
                        // confirmed revoked (invalid_grant / 401). Transient
                        // 5xx or network failures keep the file so the next
                        // request can retry instead of forcing re-login.
                        if e.is_auth_error() {
                            tracing::warn!(provider = cfg.provider, error = %e, "OAuth refresh token revoked, clearing stored tokens");
                            let _ = maki_storage::auth::delete_tokens(&dir, cfg.provider);
                        } else {
                            tracing::warn!(provider = cfg.provider, error = %e, "OAuth refresh failed (transient), keeping stored tokens");
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer_header_value(auth: &Arc<Mutex<ResolvedAuth>>) -> String {
        auth.lock()
            .unwrap()
            .headers
            .iter()
            .find_map(|(name, value)| (name == "authorization").then(|| value.clone()))
            .unwrap()
    }

    #[test]
    fn reload_refreshes_cached_key_pool_so_rotate_key_uses_new_env() {
        let env_var: &'static str =
            Box::leak(format!("MAKI_TEST_RELOAD_{}", fastrand::u32(..)).into_boxed_str());
        unsafe { std::env::set_var(env_var, "sk-1, sk-2") };

        let source = EnvAuthSource::new("test", env_var, ResolvedAuth::bearer);
        let auth = Arc::new(Mutex::new(ResolvedAuth {
            base_url: None,
            headers: Vec::new(),
        }));
        source.resolve(&auth).unwrap();
        assert_eq!(bearer_header_value(&auth), "Bearer sk-1");

        // Reload must replace the pool so rotation cannot revive a removed key.
        unsafe { std::env::set_var(env_var, "sk-3") };
        source.reload(&auth).unwrap();
        assert_eq!(bearer_header_value(&auth), "Bearer sk-3");

        let rotated = source.rotate_key(&auth).unwrap();
        assert!(
            !rotated,
            "rotate_key must report no rotation for a single-key pool"
        );
        assert_eq!(bearer_header_value(&auth), "Bearer sk-3");

        unsafe { std::env::remove_var(env_var) };
    }

    #[test]
    fn reload_invalidates_pending_rotation() {
        let mut state = EnvAuthState::default();
        state.install(KeyPool::from_keys(vec!["old-1".into(), "old-2".into()]));
        let pending = state.prepare_rotation().unwrap();

        state.install(KeyPool::from_keys(vec!["new-1".into()]));

        assert!(!state.is_current(pending.revision));
        assert_eq!(state.pool.unwrap().current(), "new-1");
    }
}
