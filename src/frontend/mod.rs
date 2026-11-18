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
use std::fmt;

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

/// How full the main agent's context is after its latest model call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContextUsage {
    /// What the next request sends again: the last call's input and output tokens.
    pub tokens: u64,
    /// The most tokens the model reads in one request, if known.
    pub window: Option<u64>,
}

impl ContextUsage {
    /// How much of the window is used, in whole percent (above 100 when over it).
    pub fn percent(&self) -> Option<u64> {
        let window = self.window.filter(|&w| w > 0)?;
        Some((self.tokens.saturating_mul(100) + window / 2) / window)
    }
}

/// `42k / 200k tokens (21%)`, or `42k tokens` when the window is unknown.
impl fmt::Display for ContextUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", compact(self.tokens))?;
        if let (Some(window), Some(percent)) = (self.window, self.percent()) {
            write!(f, " / {}", compact(window))?;
            return write!(f, " tokens ({percent}%)");
        }
        write!(f, " tokens")
    }
}

/// A token count in a few characters: `950`, `4.1k`, `42k`, `1M`, `1.1M`.
fn compact(n: u64) -> String {
    // One decimal below 10 units, unless it is zero.
    let short = |unit: u64, suffix: &str| {
        let tenths = (n + unit / 20) / (unit / 10);
        if tenths < 100 && !tenths.is_multiple_of(10) {
            format!("{}.{}{suffix}", tenths / 10, tenths % 10)
        } else {
            format!("{}{suffix}", (n + unit / 2) / unit)
        }
    };
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => short(1_000, "k"),
        _ => short(1_000_000, "M"),
    }
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
    /// A model call of the main agent finished (subagents have their own context).
    Context(ContextUsage),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_are_compact() {
        let cases = [
            (0, "0"),
            (950, "950"),
            (1_000, "1k"),
            (1_040, "1k"),
            (4_096, "4.1k"),
            (9_960, "10k"),
            (42_600, "43k"),
            (200_000, "200k"),
            (999_499, "999k"),
            (999_500, "1M"),
            (1_048_576, "1M"),
            (1_050_000, "1.1M"),
            (12_345_678, "12M"),
        ];
        for (n, expected) in cases {
            assert_eq!(compact(n), expected, "{n}");
        }
    }

    #[test]
    fn context_usage_shows_the_window_when_known() {
        let usage = ContextUsage {
            tokens: 42_000,
            window: Some(200_000),
        };
        assert_eq!(usage.to_string(), "42k / 200k tokens (21%)");
        let over = ContextUsage {
            tokens: 5_000,
            window: Some(4_096),
        };
        assert_eq!(over.to_string(), "5k / 4.1k tokens (122%)");
        let unknown = ContextUsage {
            tokens: 1_234,
            window: None,
        };
        assert_eq!(unknown.to_string(), "1.2k tokens");
        assert_eq!(unknown.percent(), None);
        let empty = ContextUsage {
            tokens: 10,
            window: Some(0),
        };
        assert_eq!(empty.to_string(), "10 tokens");
    }
}
