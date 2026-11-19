//! The `ask_user` tool: asks the user a question mid-run and returns their answer.
//! Blocking: the call waits until the user answers. Only the main agent has it.

use crate::frontend::{Frontend, Question};
use crate::harness::{Approval, Context, ToolPolicy};
use rig_core::tool::{Tool, ToolContext};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const DESCRIPTION: &str = "Ask the user a question and wait for the answer. Use it when you are blocked on a decision that is the user's to make or on information only they have (a preference, a missing value, which of several approaches to take), and you cannot find it out yourself or choose a sensible default.

Do not use it for things you can look up or decide yourself, and do not ask more than needed; ask one focused question per call. If there are a few likely answers, list them in `options` (the user can still answer freely). The answer comes back as the tool result.";

#[derive(rig_core::serde::Deserialize, rig_core::schemars::JsonSchema)]
#[serde(crate = "rig_core::serde")]
#[schemars(crate = "rig_core::schemars")]
pub(crate) struct AskUserArgs {
    /// The question, self-contained and short.
    question: String,
    /// Optional suggested answers (2-4 short choices). The user may also answer in their own words.
    #[serde(default)]
    options: Vec<String>,
}

/// Asks through the frontend. It cannot be a `#[rig_tool]` function because it needs it.
pub(crate) struct AskUser {
    frontend: Arc<dyn Frontend>,
}

impl AskUser {
    pub(crate) fn new(frontend: Arc<dyn Frontend>) -> Self {
        Self { frontend }
    }
}

/// Asking needs no approval: the question is the interaction.
impl ToolPolicy for AskUser {
    fn prompt(_context: &Context) -> String {
        "Use `ask_user` to ask the user a question when you need a decision or information only they can give.".to_owned()
    }

    fn approval(args: &AskUserArgs, _context: &Context) -> Approval {
        Approval::auto(format!("[question] {}", args.question))
    }
}

impl Tool for AskUser {
    const NAME: &'static str = "ask_user";
    type Args = AskUserArgs;
    type Output = String;
    type Error = Infallible;

    fn description(&self) -> String {
        DESCRIPTION.to_string()
    }

    fn parameters(&self) -> rig_core::serde_json::Value {
        rig_core::schemars::schema_for!(AskUserArgs).to_value()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, Infallible> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let question = Question {
            id: format!("q{}", COUNTER.fetch_add(1, Ordering::Relaxed)),
            question: args.question,
            options: args.options,
        };
        Ok(match self.frontend.ask(&question).await {
            Some(answer) if !answer.trim().is_empty() => format!("The user answered: {answer}"),
            _ => "The user did not answer. Continue with a sensible default or say what you need."
                .to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Silent;

    #[tokio::test]
    async fn unanswered_question_tells_the_model() {
        let args = AskUserArgs {
            question: "Which?".into(),
            options: vec![],
        };
        let out = AskUser::new(Arc::new(Silent))
            .call(&mut ToolContext::new(), args)
            .await
            .unwrap();
        assert!(out.starts_with("The user did not answer"), "{out}");
    }
}
