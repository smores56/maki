use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event};
use tracing::warn;

const INPUT_POLL_TICK: Duration = Duration::from_millis(50);
const PAUSE_ACK_TIMEOUT: Duration = Duration::from_millis(500);

pub struct InputSource {
    pub rx: flume::Receiver<Event>,
    gate: Arc<Gate>,
}

/// Resumes the reader thread on drop.
pub struct PauseGuard<'a>(&'a InputSource);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GateState {
    Running,
    PauseRequested,
    Parked,
}

struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

fn lock(gate: &Mutex<GateState>) -> MutexGuard<'_, GateState> {
    gate.lock().unwrap_or_else(|e| e.into_inner())
}

impl InputSource {
    pub fn spawn() -> Self {
        let (tx, rx) = flume::unbounded();
        let gate = Arc::new(Gate {
            state: Mutex::new(GateState::Running),
            cv: Condvar::new(),
        });
        let thread_gate = Arc::clone(&gate);
        thread::Builder::new()
            .name("input-reader".into())
            .spawn(move || read_loop(tx, thread_gate))
            .expect("spawn input-reader");
        Self { rx, gate }
    }

    /// Returns once the reader thread is parked and guaranteed not to
    /// consume tty bytes, or after PAUSE_ACK_TIMEOUT if the thread is
    /// wedged (degraded: logged, caller proceeds — see validation notes).
    /// Call before handing the terminal to a child process or suspending.
    pub fn pause(&self) -> PauseGuard<'_> {
        let mut st = lock(&self.gate.state);
        assert_eq!(*st, GateState::Running, "nested input pause");
        *st = GateState::PauseRequested;
        while *st != GateState::Parked {
            let (guard, timeout) = self
                .gate
                .cv
                .wait_timeout(st, PAUSE_ACK_TIMEOUT)
                .unwrap_or_else(|e| e.into_inner());
            st = guard;
            if timeout.timed_out() {
                warn!("input-reader did not park within {PAUSE_ACK_TIMEOUT:?}");
                break;
            }
        }
        PauseGuard(self)
    }

    fn resume(&self) {
        let mut st = lock(&self.gate.state);
        *st = GateState::Running;
        self.gate.cv.notify_all();
    }
}

impl Drop for PauseGuard<'_> {
    fn drop(&mut self) {
        self.0.resume();
    }
}

fn read_loop(tx: flume::Sender<Event>, gate: Arc<Gate>) {
    loop {
        {
            let mut st = lock(&gate.state);
            if *st == GateState::PauseRequested {
                *st = GateState::Parked;
                gate.cv.notify_all();
                while *st == GateState::Parked {
                    st = gate.cv.wait(st).unwrap_or_else(|e| e.into_inner());
                }
                continue;
            }
        }
        match event::poll(INPUT_POLL_TICK) {
            Ok(true) => {
                if *lock(&gate.state) != GateState::Running {
                    continue;
                }
                match event::read() {
                    Ok(ev) => {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            Ok(false) => {}
            Err(_) => return,
        }
    }
}
