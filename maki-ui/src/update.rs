use std::sync::OnceLock;

use maki_storage::version;

use crate::doorbell::Ringer;

static LATEST: OnceLock<String> = OnceLock::new();

pub use version::{CURRENT, is_newer};

pub fn latest_version() -> Option<&'static str> {
    LATEST.get().map(|s| s.as_str())
}

pub fn spawn_check(bell: Ringer) {
    smol::spawn(async move {
        match version::fetch_latest_async().await {
            Ok(v) if is_newer(&v, CURRENT) => {
                if LATEST.set(v).is_ok() {
                    bell.ring();
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "update check failed");
            }
        }
    })
    .detach();
}
