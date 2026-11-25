//! The `agent` tool: runs a task in a subagent with its own fresh context and returns
//! only the subagent's final report. Blocking: the call waits until the subagent is done.

use crate::frontend::{Event, Frontend, Scoped};
use crate::harness::{self, Approval, Context, Toolset, INVALID_TOOL_CALL_RETRIES, ToolPolicy};
use rig_core::DynModel;
use rig_core::operation::Completion;
use rig_core::tool::{Tool, ToolContext};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Added to the system prompt of every subagent.
const SUBAGENT_NOTE: &str = "\n\n# Subagent\n\nYou are a subagent. Complete the task autonomously and end with a concise final report of what you found or did; the report is all the caller sees. You cannot start further subagents (there is no `agent` tool for you) and you cannot ask the caller questions.";

/// Added for the subagents a script starts with `agent()`.
#[cfg(feature = "javascript")]
const SCRIPT_NOTE: &str = "\n\nA script started you with `agent()`: your final report is returned to that script as a string, so write it exactly in the format the task asks for (nothing before or after it). You can't change data: calls of `execute_javascript` with `writes: true` are refused, and there is no shell.";

const DESCRIPTION: &str = "Delegate a self-contained task to a subagent that works in its own fresh context and returns only its final report. The subagent has the same tools as you (`execute`, `use_skill`) but does not see this conversation; the call blocks until it is done.

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

/// A subagent id that is unique across restarts too, as sessions are saved with theirs.
fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("sub{millis:x}{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Starts a subagent on the same model. It cannot be a `#[rig_tool]` function because it
/// needs the model; `preamble` is the system prompt the subagent starts from. Its tool
/// calls are approved and reported through the same `frontend` as the caller's.
pub(crate) struct Subagent {
    model: DynModel<Completion>,
    preamble: String,
    frontend: Arc<dyn Frontend>,
    /// The tools of the caller; the subagent gets the same ones (except `agent`).
    enabled: Toolset,
}

impl Subagent {
    pub(crate) fn new(
        model: DynModel<Completion>,
        preamble: &str,
        frontend: Arc<dyn Frontend>,
        enabled: Toolset,
    ) -> Self {
        Self {
            model,
            preamble: format!("{preamble}{SUBAGENT_NOTE}"),
            frontend,
            enabled,
        }
    }

    /// The subagents of `agent()` in scripts: read-only and without a shell (see
    /// [`Toolset::for_script_agents`]).
    #[cfg(feature = "javascript")]
    pub(crate) fn for_scripts(
        model: DynModel<Completion>,
        preamble: &str,
        frontend: Arc<dyn Frontend>,
        enabled: &Toolset,
    ) -> Self {
        let mut agent = Self::new(model, preamble, frontend, enabled.for_script_agents());
        agent.preamble.push_str(SCRIPT_NOTE);
        agent
    }

    /// Runs a subagent and returns its report, or a sentence saying why there is none. The
    /// frontend sees it start and finish.
    async fn report(&self, description: String, prompt: String) -> String {
        let id = new_id();
        self.frontend.emit(Event::SubagentStarted {
            agent: id.clone(),
            description,
            prompt: prompt.clone(),
        });
        let result = self.run(&id, &prompt).await;
        self.frontend.emit(Event::SubagentFinished {
            agent: id,
            failed: result.is_err(),
        });
        match result {
            Ok(text) if text.trim().is_empty() => {
                "The subagent finished without writing a report.".to_string()
            }
            Ok(text) => text,
            Err(error) => format!("The subagent failed: {error}"),
        }
    }

    /// Runs `prompt` in a fresh agent (no `agent` tool, so no nesting) and returns its final text.
    /// What the subagent does is reported to the frontend under `id`.
    async fn run(&self, id: &str, prompt: &str) -> Result<String, String> {
        let frontend: Arc<dyn Frontend> =
            Arc::new(Scoped::new(self.frontend.clone(), id.to_owned()));
        let agent = harness::subagent(
            self.model.clone(),
            &self.preamble,
            frontend.clone(),
            &self.enabled,
        );
        let mut stream = agent
            .prompt(prompt)
            .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
            .stream();
        let report = harness::Report {
            frontend: &*frontend,
            context_window: None,
            partial: None,
        };
        let response = harness::run_stream(&mut stream, Some(report)).await?;
        Ok(response.output())
    }
}

/// Starting a subagent needs no approval: its own tool calls are approved one by one.
impl ToolPolicy for Subagent {
    fn prompt(_context: &Context) -> String {
        "Use `agent` to delegate self-contained tasks (large searches, reading many files) to a subagent that only reports back its result.".to_owned()
    }

    fn approval(args: &AgentArgs, _context: &Context) -> Approval {
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
        Ok(self.report(args.description, args.prompt).await)
    }
}

#[cfg(feature = "javascript")]
impl crate::tools::execute_javascript::ScriptAgent for Subagent {
    fn report<'a>(
        &'a self,
        description: String,
        prompt: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + 'a>> {
        Box::pin(Subagent::report(self, description, prompt))
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
        for name in ["execute", "use_skill"] {
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
        Subagent::new(model.into(), "BASE", Arc::new(Silent), Toolset::default())
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

    /// A script of the main agent loops over items with `agent()`: each item gets a fresh,
    /// read-only subagent (no `agent`, no shell, its writing call is refused without asking),
    /// and only what the script prints reaches the main agent.
    #[cfg(feature = "javascript")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_script_loops_over_read_only_subagents() {
        use rig_core::serde_json::json;
        let code = r#"for (const p of ["first item", "second item"]) console.log("got", await agent(p))"#;
        let write = json!({"code": "console.log(1)", "hosts": ["paperless.example"], "writes": true});
        let model = MockCompletionModel::from_stream_turns([
            turn(MockStreamEvent::tool_call("t1", "execute_javascript", json!({"code": code}))),
            turn(MockStreamEvent::tool_call("s1", "execute_javascript", write)),
            turn(MockStreamEvent::text("report one")),
            turn(MockStreamEvent::text("report two")),
            turn(MockStreamEvent::text("done")),
        ]);
        let recorded = model.clone();
        let mut session = Session::new(model.into(), "BASE", Arc::new(Silent));
        session.prompt("sort them".into()).await.expect("run must succeed");

        let requests = recorded.requests();
        assert_eq!(requests.len(), 5);
        let sub_tools = tool_names(&requests[1]);
        assert!(sub_tools.contains(&"execute_javascript".to_string()), "{sub_tools:?}");
        for name in ["agent", "execute", "ask_user"] {
            assert!(!sub_tools.contains(&name.to_string()), "{sub_tools:?}");
        }
        let first = format!("{:?}", requests[1].chat_history);
        assert!(first.contains("first item") && !first.contains("sort them"), "{first}");
        assert!(first.contains("A script started you"), "{first}");
        // The subagent's writing call was refused, and it went on.
        let refused = format!("{:?}", requests[2].chat_history);
        assert!(refused.contains("this agent may not change data"), "{refused}");
        // The second item starts fresh, too.
        let second = format!("{:?}", requests[3].chat_history);
        assert!(second.contains("second item") && !second.contains("first item"), "{second}");
        // The main agent sees the script's output, not the subagents' conversations.
        let main = format!("{:?}", requests[4].chat_history);
        assert!(main.contains("got report one") && main.contains("got report two"), "{main}");
        assert!(!main.contains("may not change data"), "{main}");
    }

    fn serde_args(description: &str, prompt: &str) -> rig_core::serde_json::Value {
        rig_core::serde_json::json!({"description": description, "prompt": prompt})
    }
}
