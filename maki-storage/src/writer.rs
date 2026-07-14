//! Coalescing write-behind cache with incremental JSONL persistence.
//!
//! The UI posts session snapshots and the writer thread picks up only the latest.
//! A `bounded(1)` notify channel ensures rapid saves collapse rather than queue.
//! The writer thread owns the `SessionLog` and performs O(delta) appends.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::StateDir;
use crate::sessions::{SESSIONS_DIR, Session, SessionError, SessionLog, TitleSource};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::warn;
type LatestSession<M, U, T> = Arc<Mutex<Option<Box<Session<M, U, T>>>>>;

pub struct StorageWriter<M, U, T> {
    latest: LatestSession<M, U, T>,
    notify: flume::Sender<()>,
    done_rx: flume::Receiver<()>,
}

impl<M, U, T> StorageWriter<M, U, T>
where
    M: Serialize + DeserializeOwned + TitleSource + Send + 'static,
    U: Serialize + DeserializeOwned + Default + Send + 'static,
    T: Serialize + DeserializeOwned + Send + 'static,
{
    pub fn new(dir: StateDir) -> Self {
        let latest: LatestSession<M, U, T> = Arc::new(Mutex::new(None));
        let writer_latest = Arc::clone(&latest);
        let (notify, notify_rx) = flume::bounded::<()>(1);
        let (done_tx, done_rx) = flume::bounded::<()>(1);

        std::thread::Builder::new()
            .name("storage-writer".into())
            .spawn(move || {
                let mut log: Option<SessionLog> = None;

                while notify_rx.recv().is_ok() {
                    let session = writer_latest.lock().unwrap().take();
                    let Some(session) = session else { continue };
                    let sessions_dir = match dir.ensure_subdir(SESSIONS_DIR) {
                        Ok(d) => d,
                        Err(e) => {
                            warn!(error = %e, "failed to ensure sessions dir");
                            continue;
                        }
                    };

                    let is_current = log.as_ref().is_some_and(|l| l.session_id() == session.id);
                    if !is_current {
                        match open_or_create_log::<M, U, T>(&sessions_dir, &session) {
                            Ok(l) => log = Some(l),
                            Err(e) => {
                                warn!(error = %e, "session log open failed");
                                continue;
                            }
                        }
                    }
                    let l = log.as_mut().unwrap();
                    match l.append(&*session) {
                        Ok(()) => {}
                        Err(SessionError::CursorAhead { .. }) => {
                            match l.compact(&sessions_dir, &*session) {
                                Ok(()) => {}
                                Err(e) => {
                                    warn!(error = %e, "compact fallback failed");
                                    log = None;
                                }
                            }
                        }
                        Err(e) => warn!(error = %e, "append failed"),
                    }
                }
                let _ = done_tx.send(());
            })
            .expect("failed to spawn storage writer thread");

        Self {
            latest,
            notify,
            done_rx,
        }
    }

    pub fn send(&self, session: Box<Session<M, U, T>>) {
        *self.latest.lock().unwrap() = Some(session);
        let _ = self.notify.try_send(());
    }

    pub fn shutdown(self, timeout: Duration) {
        drop(self.notify);
        if self.done_rx.recv_timeout(timeout).is_err() {
            warn!("storage writer did not drain within {timeout:?}");
        }
    }
}

fn open_or_create_log<M, U, T>(
    sessions_dir: &Path,
    session: &Session<M, U, T>,
) -> Result<SessionLog, SessionError>
where
    M: Serialize + DeserializeOwned + TitleSource,
    U: Serialize + DeserializeOwned + Default,
    T: Serialize + DeserializeOwned,
{
    let jsonl_path = sessions_dir.join(format!("{}.jsonl", session.id));
    if jsonl_path.exists() {
        let (_loaded, log) = SessionLog::open::<M, U, T>(sessions_dir, &session.id)?;
        Ok(log)
    } else {
        Session::migrate_to_jsonl(sessions_dir, session)
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::sessions::TitleSource;
    use serde::Deserialize;

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct TestMsg;

    impl TitleSource for TestMsg {
        fn first_user_text(&self) -> Option<&str> {
            None
        }
    }

    #[derive(Serialize, Deserialize, Default)]
    struct TestUsage;

    #[derive(Serialize, Deserialize)]
    struct TestOut;

    #[test]
    fn shutdown_drains_pending_session() {
        let dir = StateDir::from_path(env::temp_dir().join("maki-test-sw"));
        let writer = StorageWriter::<TestMsg, TestUsage, TestOut>::new(dir);
        writer.send(Box::new(Session::new("test-model", "/tmp")));
        writer.shutdown(Duration::from_secs(2));
    }
}
