//! The `agent` tool: runs a task in a subagent with its own fresh context and returns
//! only the subagent's final report. Blocking: the call waits until the subagent is done.

use crate::frontend::Frontend;
use crate::harness::{self, Approval, INVALID_TOOL_CALL_RETRIES, ToolPolicy};
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_core::tool::{Tool, ToolContext};
use std::convert::Infallible;
use std::sync::Arc;

/// Added to the system prompt of every subagent.
const SUBAGENT_NOTE: &str = "\n\n# Subagent\n\nYou are a subagent. Complete the task autonomously and end with a concise final report of what you found or did; the report is all the caller sees. You cannot start further subagents (there is no `agent` tool for you) and you cannot ask the caller questions.";

const DESCRIPTION: &str = "Delegate a self-contained task to a subagent that works in its own fresh context and returns only its final report. The subagent has the same tools as you (`execute`, `execute_javascript`, `use_skill`) but does not see this conversation; the call blocks until it is done.

Use it for large searches or exploration, reading many files or long logs, and independent sub-tasks whose details you do not need afterwards. This keeps your own context small.
Do not use it for simple lookups (run `execute` yourself) or for tasks that need the conversation's context; if you delegate such a task anyway, put everything the subagent needs into `prompt`.

Write `prompt` as a complete briefing: the goal, relevant paths, constraints, and what the final report must contain (e.g. file paths with line numbers, a short summary, yes/no answers). Only the subagent's last message comes back to you.";

#[derive(rig_core::serde::Deserialize, rig_core::schemars::JsonSchema)]
#[serde(crate = "rig_core::serde")]
#[schemars(crate = "rig_core::schemars")]
pub(crate) struct AgentArgs {
    /// Short label (3-5 words) shown in the UI, e.g. "find config loading code".
    description: String,
    /// The complete, self-contained task. The subagent does not see the conversation, so include all context it needs and say what the final report must contain.
    prompt: String,
}

/// Starts a subagent on the same model. It cannot be a `#[rig_tool]` function because it
/// needs the model; `preamble` is the system prompt the subagent starts from. Its tool
/// calls are approved and reported through the same `frontend` as the caller's.
pub(crate) struct Subagent {
    model: DynModel<Completion>,
    preamble: String,
    frontend: Arc<dyn Frontend>,
}

impl Subagent {
    pub(crate) fn new(
        model: DynModel<Completion>,
        preamble: &str,
        frontend: Arc<dyn Frontend>,
    ) -> Self {
        Self {
            model,
            preamble: format!("{preamble}{SUBAGENT_NOTE}"),
            frontend,
        }
    }

    /// Runs `prompt` in a fresh agent (no `agent` tool, so no nesting) and returns its final text.
    async fn run(&self, prompt: &str) -> Result<String, String> {
        let agent = harness::subagent(self.model.clone(), &self.preamble, self.frontend.clone());
        let mut stream = agent
            .prompt(prompt)
            .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
            .stream();
        // The subagent's text is not shown, only its tool calls.
        let response = harness::run_stream(&mut stream, None).await?;
        Ok(response.output())
    }
}

/// Starting a subagent needs no approval: its own tool calls are approved one by one.
impl ToolPolicy for Subagent {
    fn approval(args: &AgentArgs, _sandboxed: bool) -> Approval {
        Approval::auto(format!("[subagent] {}", args.description))
    }
}

impl Tool for Subagent {
    const NAME: &'static str = "agent";
    type Args = AgentArgs;
    type Output = String;
    type Error = Infallible;

    fn description(&self) -> String {
        DESCRIPTION.to_string()
    }

    fn parameters(&self) -> rig_core::serde_json::Value {
        rig_core::schemars::schema_for!(AgentArgs).to_value()
    }

    /// Errors are returned as the tool output, so the calling agent can react to them.
    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, Infallible> {
        Ok(match self.run(&args.prompt).await {
            Ok(text) if text.trim().is_empty() => {
                "The subagent finished without writing a report.".to_string()
            }
            Ok(text) => text,
            Err(error) => format!("The subagent failed: {error}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Silent;
    use crate::harness::Session;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    fn turn(event: MockStreamEvent) -> Vec<MockStreamEvent> {
        vec![event, MockStreamEvent::final_response_with_total_tokens(1)]
    }

    fn tool_names(request: &rig_core::completion::CompletionRequest) -> Vec<String> {
        request.tools.iter().map(|t| t.name.to_string()).collect()
    }

    /// The main agent calls `agent`; the subagent runs on the same model in a fresh context
    /// and only its final text comes back as the tool result.
    #[tokio::test]
    async fn main_agent_gets_the_subagent_report() {
        let call = serde_args("count files", "Count the files in src.");
        let model = MockCompletionModel::from_stream_turns([
            turn(MockStreamEvent::tool_call("t1", "agent", call)),
            turn(MockStreamEvent::text("There are 7 files.")),
            turn(MockStreamEvent::text("done")),
        ]);
        let recorded = model.clone();
        let mut session = Session::new(model.into(), "BASE", Arc::new(Silent));
        session
            .prompt("how many files?".into())
            .await
            .expect("run must succeed");

        let requests = recorded.requests();
        assert_eq!(requests.len(), 3);
        // Main agent: has the `agent` tool. Subagent: no `agent`, but the other tools.
        assert!(tool_names(&requests[0]).contains(&"agent".to_string()));
        let sub_tools = tool_names(&requests[1]);
        assert!(!sub_tools.contains(&"agent".to_string()), "{sub_tools:?}");
        for name in ["execute", "execute_javascript", "use_skill"] {
            assert!(sub_tools.contains(&name.to_string()), "{sub_tools:?}");
        }
        // The subagent starts fresh: it sees its prompt, not the main conversation.
        let sub_request = format!("{:?}", requests[1].chat_history);
        assert!(
            sub_request.contains("Count the files in src."),
            "{sub_request}"
        );
        assert!(!sub_request.contains("how many files?"), "{sub_request}");
        // The report is the tool result the main agent sees in its next request.
        let main_followup = format!("{:?}", requests[2].chat_history);
        assert!(
            main_followup.contains("There are 7 files."),
            "{main_followup}"
        );
    }

    /// Runs `Subagent::call` on a model scripted with `turns` and returns the tool output.
    async fn call_with(turns: Vec<Vec<MockStreamEvent>>) -> String {
        let model = MockCompletionModel::from_stream_turns(turns);
        let args = AgentArgs {
            description: "x".into(),
            prompt: "y".into(),
        };
        let mut context = ToolContext::new();
        Subagent::new(model.into(), "BASE", Arc::new(Silent))
            .call(&mut context, args)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn empty_report_and_failure_become_tool_output() {
        let out = call_with(vec![turn(MockStreamEvent::text(""))]).await;
        assert_eq!(out, "The subagent finished without writing a report.");
        // No scripted turn: the subagent's model call fails.
        let out = call_with(vec![]).await;
        assert!(out.starts_with("The subagent failed:"), "{out}");
    }

    fn serde_args(description: &str, prompt: &str) -> rig_core::serde_json::Value {
        rig_core::serde_json::json!({"description": description, "prompt": prompt})
    }
}
