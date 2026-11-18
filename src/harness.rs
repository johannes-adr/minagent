//! The harness: which tools an agent gets, how their calls are approved and reported,
//! and how a conversation runs. Frontends (TUI, ACP) only use [`Session`] and implement
//! [`Frontend`].
//!
//! Adding a tool: write it in `src/tools/`, implement [`ToolPolicy`] for it and add it
//! in [`tools`].

use crate::frontend::{Event, Frontend, ToolCall};
use crate::sandbox;
use crate::tools::agent::Subagent;
use crate::tools::execute::Execute;
use crate::tools::skills::{self, use_skill::UseSkill};
use futures::StreamExt;
use rig_agent::AgentBuilder;
use rig_agent::agent::{
    Agent, AgentHook, DispatchAction, DispatchEvent, HookContext, MultiTurnStreamItem,
    OutcomeAction, OutcomeEvent, StreamingResult,
};
use rig_agent::run::policy::{
    InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason,
};
use rig_agent::run::response::PromptResponse;
use rig_agent::streaming::{Item, StreamEvent};
use rig_core::DynModel;
use rig_core::effect::{Answered, EffectKind};
use rig_core::message::Message;
use rig_core::operation::Completion;
use rig_core::serde_json::{self, Value};
use rig_core::tool::{Tool, ToolOutput};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// How often the model may retry after calling a tool that doesn't exist, per prompt.
pub const INVALID_TOOL_CALL_RETRIES: usize = 3;
/// Model calls per prompt before a run is aborted.
const MAX_TURNS: usize = 200;
const SUBAGENT_MAX_TURNS: usize = 100;

/// How a tool call is shown to the user and whether it needs their approval.
#[derive(Clone, Debug, PartialEq)]
pub struct Approval {
    pub title: String,
    pub ask: bool,
}

impl Approval {
    /// Runs without asking.
    pub fn auto(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ask: false,
        }
    }

    /// The user must approve it first.
    pub fn ask(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ask: true,
        }
    }
}

/// What a tool needs besides [`Tool`] to be offered to the model.
pub trait ToolPolicy: Tool + 'static {
    /// ACP tool kind (`execute`, `read`, `edit`, `search`, `fetch`, `think`, `other`, ...).
    const KIND: &'static str = "other";
    /// How a call with `args` is shown and whether the user must approve it.
    /// `sandboxed` is whether commands are contained: the OS sandbox is available or the
    /// harness runs in an isolated container.
    fn approval(args: &Self::Args, sandboxed: bool) -> Approval;
}

#[derive(Clone, Copy)]
struct Policy {
    kind: &'static str,
    /// `None` when the arguments don't parse.
    decide: fn(&str, bool) -> Option<Approval>,
}

fn decide<T: ToolPolicy>(args: &str, sandboxed: bool) -> Option<Approval> {
    let args = serde_json::from_str::<T::Args>(args).ok()?;
    Some(T::approval(&args, sandboxed))
}

/// An agent under construction together with the policies of its tools.
struct Tools {
    builder: AgentBuilder,
    policies: HashMap<&'static str, Policy>,
}

impl Tools {
    fn add<T: ToolPolicy>(mut self, tool: T) -> Self {
        let policy = Policy {
            kind: T::KIND,
            decide: decide::<T>,
        };
        self.policies.insert(T::NAME, policy);
        self.builder = self.builder.tool(tool);
        self
    }

    fn build(self, frontend: Arc<dyn Frontend>, max_turns: usize) -> Agent {
        let hook = Approve {
            frontend,
            policies: self.policies,
        };
        self.builder
            .add_hook(hook)
            .default_max_turns(max_turns)
            .build()
    }
}

/// The tools every agent (main agent and subagents) has. Add new tools here.
fn tools(model: &DynModel<Completion>, preamble: &str) -> Tools {
    let builder = AgentBuilder::new(model.clone()).preamble(preamble);
    Tools {
        builder,
        policies: HashMap::new(),
    }
    .add(Execute)
    .add(UseSkill)
}

/// The main agent: all tools plus `agent` to start subagents.
pub fn main_agent(
    model: DynModel<Completion>,
    preamble: &str,
    frontend: Arc<dyn Frontend>,
) -> Agent {
    tools(&model, preamble)
        .add(Subagent::new(model.clone(), preamble, frontend.clone()))
        .build(frontend, MAX_TURNS)
}

/// A subagent: the same tools without `agent`, so subagents can't nest.
pub fn subagent(model: DynModel<Completion>, preamble: &str, frontend: Arc<dyn Frontend>) -> Agent {
    tools(&model, preamble).build(frontend, SUBAGENT_MAX_TURNS)
}

/// Reports tool calls to the frontend and asks it for approval where the tool's
/// policy requires it.
struct Approve {
    frontend: Arc<dyn Frontend>,
    policies: HashMap<&'static str, Policy>,
}

fn unknown_tool_feedback(tool: &str, available: &[String], is_skill: bool) -> String {
    let mut text = format!(
        "There is no tool named `{tool}`. Available tools: {}. Retry with one of these \
         (use `execute` for shell commands).",
        available.join(", ")
    );
    if is_skill {
        text.push_str(&format!(
            " `{tool}` is a skill, not a tool: load it by calling the `use_skill` tool with name `{tool}`."
        ));
    }
    text
}

/// The text of a tool's output as the model sees it.
fn output_text(output: &ToolOutput) -> String {
    match output.as_text() {
        Some(text) => text.to_owned(),
        None => serde_json::to_string(output).unwrap_or_default(),
    }
}

impl Approve {
    fn kind(&self, name: &str) -> &'static str {
        self.policies.get(name).map_or("other", |p| p.kind)
    }

    fn tool_call(&self, id: String, name: &str, args: &str, sandboxed: bool) -> ToolCall {
        let approval = self
            .policies
            .get(name)
            .and_then(|policy| (policy.decide)(args, sandboxed))
            .unwrap_or_else(|| Approval::ask(format!("{name} {args}")));
        ToolCall {
            id,
            name: name.to_owned(),
            kind: self.kind(name),
            title: approval.title,
            ask: approval.ask,
            args: serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_owned())),
        }
    }
}

impl AgentHook for Approve {
    async fn on_invalid_tool_call(
        &self,
        _: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        match event.reason {
            InvalidToolCallReason::UnknownTool => {
                self.frontend
                    .emit(Event::Note(format!("[unknown tool] {}", event.tool_name)));
                let is_skill = skills::discover(Path::new(skills::DIR))
                    .skills
                    .iter()
                    .any(|skill| skill.name == event.tool_name);
                Some(InvalidToolCallAction::retry(unknown_tool_feedback(
                    &event.tool_name,
                    &event.available_tools,
                    is_skill,
                )))
            }
            _ => None,
        }
    }

    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::ToolCall { name, args } = event.kind else {
            return DispatchAction::proceed();
        };
        let id = event
            .call_id
            .map_or_else(|| event.id.to_string(), ToString::to_string);
        let contained = sandbox::supported() || sandbox::isolated();
        let call = self.tool_call(id, name, args, contained);
        if call.ask && !self.frontend.approve(&call).await {
            self.frontend.emit(Event::ToolDenied(call));
            return DispatchAction::skip("the user denied this tool call");
        }
        self.frontend.emit(Event::ToolStarted(call));
        DispatchAction::proceed()
    }

    async fn on_outcome(&self, _: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        let EffectKind::ToolCall { name, .. } = event.kind else {
            return OutcomeAction::Proceed;
        };
        let (output, failed) = match event.outcome {
            // A skipped call never ran: `on_dispatch` already reported it as denied.
            Ok(Answered::ToolResult { result }) if result.is_skipped() => {
                return OutcomeAction::Proceed;
            }
            Ok(Answered::ToolResult { result }) => {
                (output_text(result.output()), !result.is_success())
            }
            Ok(_) => return OutcomeAction::Proceed,
            Err(report) => (report.message.clone(), true),
        };
        self.frontend.emit(Event::ToolFinished {
            id: event
                .call_id
                .map_or_else(|| event.id.to_string(), ToString::to_string),
            kind: self.kind(name),
            output,
            failed,
        });
        OutcomeAction::Proceed
    }
}

/// Drives `stream` to its final response; answer text goes to `frontend` if given.
pub async fn run_stream(
    stream: &mut StreamingResult,
    frontend: Option<&dyn Frontend>,
) -> Result<PromptResponse, String> {
    while let Some(item) = stream.next().await {
        match item.map_err(|e| e.to_string())? {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            })) => {
                if let Some(frontend) = frontend {
                    frontend.emit(Event::Text(text));
                }
            }
            MultiTurnStreamItem::FinalResponse(response) => return Ok(response),
            _ => {}
        }
    }
    Err("the stream ended without a final response".into())
}

/// A conversation with the main agent.
pub struct Session {
    agent: Agent,
    history: Vec<Message>,
    frontend: Arc<dyn Frontend>,
}

impl Session {
    pub fn new(model: DynModel<Completion>, preamble: &str, frontend: Arc<dyn Frontend>) -> Self {
        Self {
            agent: main_agent(model, preamble, frontend.clone()),
            history: Vec::new(),
            frontend,
        }
    }

    /// Runs one prompt to the end; on success it becomes part of the history.
    /// Dropping the future cancels the prompt and leaves the history unchanged.
    pub async fn prompt(&mut self, input: String) -> Result<(), String> {
        let mut stream = self
            .agent
            .prompt(input)
            .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
            .history(self.history.iter().cloned())
            .stream();
        let response = run_stream(&mut stream, Some(&*self.frontend)).await?;
        drop(stream);
        self.history.extend(response.messages);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Silent;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
    use std::sync::Mutex;

    /// Records events and answers approvals with a fixed value.
    struct Recorder {
        events: Mutex<Vec<Event>>,
        approve: bool,
    }

    impl Frontend for Recorder {
        fn emit(&self, event: Event) {
            self.events.lock().unwrap().push(event);
        }
        fn approve(&self, call: &ToolCall) -> futures::future::BoxFuture<'static, bool> {
            self.events
                .lock()
                .unwrap()
                .push(Event::Note(format!("ask {}", call.title)));
            let answer = self.approve;
            Box::pin(async move { answer })
        }
    }

    fn approve_hook() -> Approve {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let tools = tools(&model.into(), "");
        Approve {
            frontend: Arc::new(Silent),
            policies: tools.policies,
        }
    }

    #[test]
    fn calls_get_their_tools_policy() {
        let hook = approve_hook();
        let call = hook.tool_call("1".into(), "execute", r#"{"command":"ls"}"#, true);
        assert_eq!(
            (call.kind, call.title.as_str(), call.ask),
            ("execute", "[sandboxed] ls", false)
        );
        // Arguments that don't parse and unknown tools are always asked for.
        let call = hook.tool_call("2".into(), "execute", "not json", true);
        assert!(call.ask);
        assert_eq!(call.title, "execute not json");
        assert!(
            hook.tool_call("3".into(), "nope", r#"{"code":"1"}"#, true)
                .ask
        );
    }

    /// A call to a nonexistent tool must not end the run: the model gets feedback and retries.
    #[tokio::test]
    async fn unknown_tool_call_is_retried() {
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("t1", "bash", serde_json::json!({"command": "ls"})),
                MockStreamEvent::final_response_with_total_tokens(1),
            ],
            vec![
                MockStreamEvent::text("done"),
                MockStreamEvent::final_response_with_total_tokens(1),
            ],
        ]);
        let recorded = model.clone();
        let mut session = Session::new(model.into(), "", Arc::new(Silent));
        session
            .prompt("hi".into())
            .await
            .expect("run must continue");
        assert_eq!(recorded.request_count(), 2);
    }

    /// The frontend sees the call start, its approval request and the result; a denied
    /// call is reported as denied and does not run.
    #[tokio::test]
    async fn tool_calls_are_reported_to_the_frontend() {
        for approve in [true, false] {
            let args = serde_json::json!({"command": "echo hi", "disable_sandbox": true});
            let model = MockCompletionModel::from_stream_turns([
                vec![
                    MockStreamEvent::tool_call("t1", "execute", args),
                    MockStreamEvent::final_response_with_total_tokens(1),
                ],
                vec![
                    MockStreamEvent::text("done"),
                    MockStreamEvent::final_response_with_total_tokens(1),
                ],
            ]);
            let frontend = Arc::new(Recorder {
                events: Mutex::new(Vec::new()),
                approve,
            });
            let mut session = Session::new(model.into(), "", frontend.clone());
            session.prompt("hi".into()).await.unwrap();
            let events = frontend.events.lock().unwrap();
            let names: Vec<_> = events
                .iter()
                .map(|e| match e {
                    Event::Note(n) => n.lines().next().unwrap().to_string(),
                    Event::ToolStarted(c) => format!("started {}", c.id),
                    Event::ToolDenied(c) => format!("denied {}", c.id),
                    Event::ToolFinished { id, failed, .. } => format!("finished {id} {failed}"),
                    Event::Text(t) => t.clone(),
                })
                .collect();
            let ask = "ask execute (not sandboxed): echo hi".to_string();
            if approve {
                let expected = [
                    ask,
                    "started t1".into(),
                    "finished t1 false".into(),
                    "done".into(),
                ];
                assert_eq!(names, expected);
            } else {
                assert_eq!(names, [ask, "denied t1".into(), "done".into()]);
            }
        }
    }

    #[test]
    fn unknown_tool_feedback_lists_available_tools() {
        let tools = ["execute".into(), "agent".into()];
        let text = unknown_tool_feedback("bash", &tools, false);
        assert!(
            text.contains("`bash`") && text.contains("execute, agent"),
            "{text}"
        );
        assert!(!text.contains("use_skill"), "{text}");
    }

    #[test]
    fn unknown_tool_feedback_points_skills_to_use_skill() {
        let text = unknown_tool_feedback("creating-skills", &["use_skill".into()], true);
        assert!(
            text.contains("calling the `use_skill` tool with name `creating-skills`"),
            "{text}"
        );
    }
}
