//! `agent(prompt)` in scripts: a script starts a subagent and gets its final report back as a
//! string. The loop over many items is then code, and only what the script prints reaches the
//! caller's context.
//!
//! The subagents are read-only (their scripts can't change data and they have no shell), can't
//! nest (no `agent` tool and no `agent()`), and run one after another, since the runtime blocks
//! in every host call. Time spent in `agent()` does not count against the script's
//! `timeout_seconds`; each call has its own limit instead. A stopped tool call stops the
//! subagent that is running.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::watch;

/// Subagents one script call may start.
pub const MAX_CALLS: usize = 20;
/// How long one `agent()` call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Starts a subagent for a script and returns its final report (or why there is none).
pub trait ScriptAgent: Send + Sync {
    fn report<'a>(&'a self, description: String, prompt: String) -> Pin<Box<dyn Future<Output = String> + 'a>>;
}

/// The end of a script's time, pushed back by the time it spends waiting for subagents.
#[derive(Clone)]
pub struct Deadline(Arc<Mutex<Instant>>);

impl Deadline {
    pub fn after(timeout: Duration) -> Self {
        Self(Arc::new(Mutex::new(Instant::now() + timeout)))
    }

    pub fn get(&self) -> Instant {
        *self.0.lock().unwrap()
    }

    pub fn passed(&self) -> bool {
        Instant::now() >= self.get()
    }

    fn extend(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

/// Closed when the tool call is dropped (the user stopped it, or a run timed out).
pub type Stopped = watch::Receiver<()>;

/// Whether the tool call that runs the script is gone.
pub fn is_stopped(stopped: &Stopped) -> bool {
    stopped.has_changed().is_err()
}

/// The `agent()` calls of one script.
pub struct Agents {
    pub agent: Arc<dyn ScriptAgent>,
    pub handle: Handle,
    pub deadline: Deadline,
    pub stopped: Stopped,
    pub calls: Cell<usize>,
}

impl Agents {
    /// Runs one subagent to its end. `Err` when the script may not start another one, or the
    /// subagent ran out of time or was stopped.
    pub fn call(&self, description: String, prompt: String) -> Result<String, String> {
        if prompt.trim().is_empty() {
            return Err("agent(prompt): the prompt is empty".into());
        }
        if self.calls.get() >= MAX_CALLS {
            return Err(format!(
                "agent(): a script may start at most {MAX_CALLS} subagents; process the rest in another call"
            ));
        }
        self.calls.set(self.calls.get() + 1);
        let started = Instant::now();
        let mut stopped = self.stopped.clone();
        let result = self.handle.block_on(async {
            tokio::select! {
                report = tokio::time::timeout(CALL_TIMEOUT, self.agent.report(description, prompt)) => {
                    report.map_err(|_| format!("agent(): the subagent did not finish within {} minutes", CALL_TIMEOUT.as_secs() / 60))
                }
                // Never sent: this only returns once the tool call is gone.
                _ = stopped.changed() => Err("agent(): stopped".to_owned()),
            }
        });
        self.deadline.extend(started.elapsed());
        result
    }
}
