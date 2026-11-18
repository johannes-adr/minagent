//! What the harness reports to a frontend and asks it, plus the frontends themselves:
//! the terminal UI ([`tui`]), the Agent Client Protocol server ([`acp`]) and the
//! browser UI ([`web`]).
//!
//! The harness (agent runs, approval hook) only talks to a [`Frontend`]; tools don't
//! talk to frontends at all, their results are reported by the hook.

pub mod acp;
pub mod tui;
pub mod web;

use futures::future::BoxFuture;
use rig_core::serde_json::Value;

/// A tool call the model made, as shown to the user.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// Unique within the session; correlates the events of one call.
    pub id: String,
    /// The tool's name, e.g. `execute`.
    pub name: String,
    /// Coarse category for display (ACP `ToolKind`: `execute`, `read`, `other`, ...).
    pub kind: &'static str,
    /// Human-readable description of this call; may span several lines (e.g. code).
    pub title: String,
    /// Whether the user has to approve it before it runs.
    pub ask: bool,
    /// The arguments as the model sent them.
    pub args: Value,
}

/// Something a frontend has to display.
#[derive(Clone, Debug)]
pub enum Event {
    /// A streamed fragment of the assistant's answer.
    Text(String),
    /// A one-line note, e.g. a call to a tool that doesn't exist.
    Note(String),
    /// The call was approved (or needed no approval) and runs now.
    ToolStarted(ToolCall),
    /// The user denied the call; it does not run.
    ToolDenied(ToolCall),
    /// The call finished; `output` is what the model gets back.
    ToolFinished {
        id: String,
        kind: &'static str,
        output: String,
        failed: bool,
    },
}

/// A frontend the harness reports to. One frontend may serve several agents
/// (a main agent and its subagents).
pub trait Frontend: Send + Sync + 'static {
    fn emit(&self, event: Event);
    /// Asks the user whether `call` may run; anything but an explicit yes is `false`.
    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool>;
}

/// Ignores events and denies every approval (tests).
#[cfg(test)]
pub struct Silent;

#[cfg(test)]
impl Frontend for Silent {
    fn emit(&self, _: Event) {}
    fn approve(&self, _: &ToolCall) -> BoxFuture<'static, bool> {
        Box::pin(async { false })
    }
}
