use std::sync::{Arc, Mutex};

use crate::AgentError;
use crate::builtin::ProviderPlan;
use crate::providers::ResolvedAuth;
use crate::providers::Timeouts;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// How a spec gets its credentials. `ApiKey` is the common builtin case: a
/// single env var whose presence [`AuthSpec::is_configured`] checks
/// synchronously — a spec-driven `build` always succeeds, so the credential
/// gate for auto-detect has to live here, not in `build`. `External` covers
/// the six script subcommands (`resolve`, `refresh`, `reload`, `rotate`,
/// `login`, `logout`) and the future Lua resolver.
pub enum AuthSpec {
    ApiKey {
        env: String,
        login_url: Option<String>,
        needs_url: bool,
        /// Static plans from the builtin inventory. `None` for builtins
        /// without plan tables. Stored as `&'static` so a registered spec
        /// never clones.
        plans: Option<&'static [(&'static str, ProviderPlan)]>,
    },
    External(Arc<dyn AuthResolver>),
}

impl AuthSpec {
    /// Sync, no I/O. Drives `provider_available` so an unconfigured provider
    /// never auto-selects and ships the conversation before the 401. See §4.3.
    pub fn is_configured(&self, slug: &str) -> bool {
        match self {
            AuthSpec::ApiKey { env, .. } => crate::providers::KeyPool::resolve(slug, env).is_ok(),
            AuthSpec::External(r) => r.is_configured(),
        }
    }
}

/// The six subcommands `dynamic.rs` runs against a script. Mirrored by Lua in
/// Step 3; kept as a trait (not an enum) so a Lua host implements each
/// individually.
pub trait AuthResolver: Send + Sync {
    fn resolve(&self) -> BoxFuture<'_, Result<ResolvedAuth, AgentError>>;
    fn refresh(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
    fn reload(&self) -> BoxFuture<'_, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }
    fn rotate(&self) -> BoxFuture<'_, Result<bool, AgentError>> {
        Box::pin(async { Ok(false) })
    }
    fn login(&self) -> Result<(), AgentError> {
        Err(AgentError::Config {
            message: "provider does not support login".into(),
        })
    }
    fn logout(&self) -> Result<(), AgentError> {
        Err(AgentError::Config {
            message: "provider does not support logout".into(),
        })
    }
    /// Sync, for auto-detect. Never performs I/O.
    fn is_configured(&self) -> bool {
        true
    }
}

/// Everything a spec's `build` fn needs beyond the spec itself. Builtins use
/// only `timeouts`; script/custom providers pass a pre-resolved `auth` and the
/// `system_prefix` the script declares. See §4.1.
#[derive(Clone)]
pub struct BuildOptions {
    pub timeouts: Timeouts,
    pub auth: Option<Arc<Mutex<ResolvedAuth>>>,
    pub system_prefix: Option<String>,
}

impl BuildOptions {
    pub fn from_timeouts(timeouts: Timeouts) -> Self {
        Self {
            timeouts,
            auth: None,
            system_prefix: None,
        }
    }
}

impl From<Timeouts> for BuildOptions {
    fn from(timeouts: Timeouts) -> Self {
        Self::from_timeouts(timeouts)
    }
}
