//! The harness: which tools an agent gets, how their calls are approved and reported,
//! and how a conversation runs. Frontends (TUI, ACP) only use [`Session`] and implement
//! [`Frontend`].
//!
//! Adding a tool: write it in `src/tools/`, implement [`ToolPolicy`] for it and add it
//! in [`tools`].

use crate::frontend::{ContextUsage, Event, Frontend, ToolCall};
use crate::tools::agent::Subagent;
use crate::tools::ask_user::AskUser;
use crate::tools::execute::Execute;
#[cfg(feature = "javascript")]
use crate::tools::execute_javascript::ExecuteJavascript;
use crate::tools::skills::{self, SkillFilter, use_skill::UseSkill};
#[cfg(feature = "image")]
use crate::tools::view_image::ViewImage;
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
use rig_core::message::{Message, ToolResultContent};
use rig_core::operation::Completion;
use rig_core::serde_json::{self, Value};
use rig_core::tool::{Tool, ToolOutput};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

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

/// The names of all tools.
pub fn tool_names() -> Vec<&'static str> {
    vec![
        Execute::NAME,
        UseSkill::NAME,
        Subagent::NAME,
        AskUser::NAME,
        #[cfg(feature = "javascript")]
        ExecuteJavascript::NAME,
        #[cfg(feature = "image")]
        ViewImage::NAME,
    ]
}

/// Fails on a name that is not a tool.
pub fn validate_tools(names: &[String]) -> Result<(), String> {
    let known = tool_names();
    match names.iter().find(|n| !known.contains(&n.as_str())) {
        Some(unknown) => Err(format!(
            "unknown tool {unknown:?}; use {}",
            known.join(", ")
        )),
        None => Ok(()),
    }
}

/// The tools an agent may have (all of them, or only those named) and, for a run that
/// needs it, its own `execute_javascript` settings instead of the configured ones.
#[derive(Clone, Default)]
pub struct Toolset {
    names: Option<Arc<Vec<String>>>,
    /// The skills these agents may use.
    skills: SkillFilter,
    #[cfg(feature = "javascript")]
    js: Option<Arc<crate::tools::execute_javascript::Settings>>,
}

// Not derived: the JavaScript settings hold secrets.
impl std::fmt::Debug for Toolset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Toolset")
            .field(&self.names)
            .field(&self.skills)
            .finish()
    }
}

impl PartialEq for Toolset {
    fn eq(&self, other: &Self) -> bool {
        self.names == other.names && self.skills == other.skills
    }
}

impl Toolset {
    /// `None` enables all tools.
    pub fn only(names: Option<Vec<String>>) -> Self {
        Self {
            names: names.map(Arc::new),
            ..Self::default()
        }
    }

    /// Lets the agents use only the skills `filter` allows.
    pub fn with_skills(mut self, filter: SkillFilter) -> Self {
        self.skills = filter;
        self
    }

    pub fn skills(&self) -> &SkillFilter {
        &self.skills
    }

    /// Runs `execute_javascript` under `settings` and refuses hosts they don't allow.
    #[cfg(feature = "javascript")]
    pub fn with_js(mut self, settings: crate::tools::execute_javascript::Settings) -> Self {
        self.js = Some(Arc::new(settings));
        self
    }

    pub fn allows(&self, name: &str) -> bool {
        self.names
            .as_ref()
            .is_none_or(|names| names.iter().any(|n| n == name))
    }

    /// The names of the enabled tools.
    pub fn names(&self) -> Vec<String> {
        tool_names()
            .into_iter()
            .filter(|name| self.allows(name))
            .map(str::to_owned)
            .collect()
    }
}

/// What the harness tells its tools about the environment it runs in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Context {
    /// Whether the harness runs inside an isolated container (`AGENT_ISOLATED=1` in the
    /// process environment, set by the `Dockerfile`).
    pub isolated: bool,
}

impl Context {
    pub fn from_env() -> Self {
        Self {
            isolated: std::env::var_os("AGENT_ISOLATED").is_some_and(|v| v == "1"),
        }
    }
}

/// What a tool needs besides [`Tool`] to be offered to the model.
pub trait ToolPolicy: Tool + 'static {
    /// ACP tool kind (`execute`, `read`, `edit`, `search`, `fetch`, `think`, `other`, ...).
    const KIND: &'static str = "other";
    /// How a call with `args` is shown and whether the user must approve it.
    fn approval(args: &Self::Args, context: &Context) -> Approval;
    /// What the system prompt says about this tool; added only while the tool is enabled.
    /// Empty adds nothing.
    fn prompt(_context: &Context) -> String {
        String::new()
    }
    /// Like [`ToolPolicy::prompt`], for a tool that depends on how this instance was set up
    /// (which skills it may load).
    fn instance_prompt(&self, context: &Context) -> String {
        Self::prompt(context)
    }
}

#[derive(Clone, Copy)]
struct Policy {
    kind: &'static str,
    /// `None` when the arguments don't parse.
    decide: fn(&str, &Context) -> Option<Approval>,
}

fn decide<T: ToolPolicy>(args: &str, context: &Context) -> Option<Approval> {
    let args = serde_json::from_str::<T::Args>(args).ok()?;
    Some(T::approval(&args, context))
}

/// An agent under construction together with the policies of its tools.
struct Tools {
    builder: AgentBuilder,
    enabled: Toolset,
    context: Context,
    /// The system prompt: the base preamble plus the [`ToolPolicy::prompt`] of each tool.
    preamble: String,
    policies: HashMap<&'static str, Policy>,
}

impl Tools {
    fn add<T: ToolPolicy>(mut self, tool: T) -> Self {
        if !self.enabled.allows(T::NAME) {
            return self;
        }
        let policy = Policy {
            kind: T::KIND,
            decide: decide::<T>,
        };
        self.policies.insert(T::NAME, policy);
        let prompt = tool.instance_prompt(&self.context);
        if !prompt.trim().is_empty() {
            self.preamble = format!("{}\n\n{}", self.preamble, prompt.trim());
        }
        self.builder = self.builder.tool(tool);
        self
    }

    fn build(self, frontend: Arc<dyn Frontend>, max_turns: usize) -> Agent {
        let hook = Approve {
            frontend,
            context: self.context,
            policies: self.policies,
        };
        self.builder
            .preamble(&self.preamble)
            .add_hook(hook)
            .default_max_turns(max_turns)
            .build()
    }
}

/// The tools every agent (main agent and subagents) has. Add new tools here.
fn tools(
    model: &DynModel<Completion>,
    preamble: &str,
    enabled: &Toolset,
    #[cfg_attr(not(feature = "javascript"), allow(unused_variables))] frontend: &Arc<dyn Frontend>,
) -> Tools {
    let tools = Tools {
        builder: AgentBuilder::new(model.clone()),
        enabled: enabled.clone(),
        context: Context::from_env(),
        preamble: preamble.to_owned(),
        policies: HashMap::new(),
    }
    .add(Execute)
    .add(UseSkill::new(enabled.skills.clone()));
    #[cfg(feature = "javascript")]
    let tools = tools.add(ExecuteJavascript::new(frontend.clone(), enabled.js.clone(), enabled.skills.clone()));
    #[cfg(feature = "image")]
    let tools = tools.add(ViewImage);
    tools
}

/// The main agent: all tools plus `agent` to start subagents and `ask_user`.
pub fn main_agent(
    model: DynModel<Completion>,
    preamble: &str,
    frontend: Arc<dyn Frontend>,
    enabled: &Toolset,
) -> Agent {
    tools(&model, preamble, enabled, &frontend)
        .add(Subagent::new(
            model.clone(),
            preamble,
            frontend.clone(),
            enabled.clone(),
        ))
        .add(AskUser::new(frontend.clone()))
        .build(frontend, MAX_TURNS)
}

/// A subagent: the same tools without `agent`, so subagents can't nest.
pub fn subagent(
    model: DynModel<Completion>,
    preamble: &str,
    frontend: Arc<dyn Frontend>,
    enabled: &Toolset,
) -> Agent {
    tools(&model, preamble, enabled, &frontend).build(frontend, SUBAGENT_MAX_TURNS)
}

/// Reports tool calls to the frontend and asks it for approval where the tool's
/// policy requires it.
struct Approve {
    frontend: Arc<dyn Frontend>,
    context: Context,
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

/// The text of a tool's output as the model sees it. Images are shown as a placeholder,
/// not as their base64 data.
fn output_text(output: &ToolOutput) -> String {
    if let Some(text) = output.as_text() {
        return text.to_owned();
    }
    if !output
        .as_content()
        .iter()
        .any(|part| matches!(part, ToolResultContent::Image(_)))
    {
        return serde_json::to_string(output).unwrap_or_default();
    }
    let parts: Vec<String> = output
        .as_content()
        .iter()
        .map(|part| match part {
            ToolResultContent::Text(text) => text.text.clone(),
            ToolResultContent::Image(_) => "[image]".to_owned(),
            ToolResultContent::Json { value } => value.to_string(),
        })
        .collect();
    parts.join("\n")
}

impl Approve {
    fn kind(&self, name: &str) -> &'static str {
        self.policies.get(name).map_or("other", |p| p.kind)
    }

    fn tool_call(&self, id: String, name: &str, args: &str) -> ToolCall {
        let approval = self
            .policies
            .get(name)
            .and_then(|policy| (policy.decide)(args, &self.context))
            .unwrap_or_else(|| Approval::ask(format!("{name} {args}")));
        ToolCall {
            id,
            name: name.to_owned(),
            kind: self.kind(name),
            title: approval.title,
            ask: approval.ask,
            args: serde_json::from_str(args).unwrap_or_else(|_| Value::String(args.to_owned())),
            agent: None,
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
        let call = self.tool_call(id, name, args);
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

/// Where the main agent's run reports its answer text and how full its context is.
pub struct Report<'a> {
    pub frontend: &'a dyn Frontend,
    /// The model's context window, if known.
    pub context_window: Option<u64>,
    /// Collects what the run did so far, so a stopped run can still be remembered.
    pub partial: Option<&'a Mutex<String>>,
}

/// Shortens `text` to at most `max` characters for the stopped-run digest.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}… [cut]", &text[..end]),
        None => text.to_string(),
    }
}

/// Drives `stream` to its final response, reporting to `report` if given.
pub async fn run_stream(
    stream: &mut StreamingResult,
    report: Option<Report<'_>>,
) -> Result<PromptResponse, String> {
    while let Some(item) = stream.next().await {
        match item.map_err(|e| e.to_string())? {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            })) => {
                if let Some(report) = &report {
                    if let Some(partial) = report.partial {
                        partial.lock().unwrap().push_str(&text);
                    }
                    report.frontend.emit(Event::Text(text));
                }
            }
            MultiTurnStreamItem::ToolCall { tool_call } => {
                if let Some(partial) = report.as_ref().and_then(|r| r.partial) {
                    let call = &tool_call.function;
                    partial.lock().unwrap().push_str(&format!(
                        "\n[tool call `{}`: {}]\n",
                        call.name,
                        clip(
                            &serde_json::to_string(&call.arguments).unwrap_or_default(),
                            1000
                        )
                    ));
                }
            }
            MultiTurnStreamItem::ToolResult { tool_result } => {
                if let Some(partial) = report.as_ref().and_then(|r| r.partial) {
                    let result = serde_json::to_string(&tool_result.content).unwrap_or_default();
                    partial
                        .lock()
                        .unwrap()
                        .push_str(&format!("[tool result: {}]\n", clip(&result, 2000)));
                }
            }
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Reasoning {
                text,
                ..
            })) => {
                if let Some(report) = &report {
                    report.frontend.emit(Event::Reasoning(text));
                }
            }
            MultiTurnStreamItem::CompletionCall(call) => {
                // The input and output of the latest call, not the sum over all calls: that
                // is what the next request sends again.
                if let Some(report) = &report
                    && let Some(tokens) = call.usage.context_tokens()
                {
                    report.frontend.emit(Event::Context(ContextUsage {
                        tokens,
                        window: report.context_window,
                    }));
                }
            }
            MultiTurnStreamItem::FinalResponse(response) => return Ok(response),
            _ => {}
        }
    }
    Err("the stream ended without a final response".into())
}

/// The most tokens one request to `model` may send, from rig's model catalog
/// (its input limit where that is below its context window).
fn catalog_context_window(model: &DynModel<Completion>) -> Option<u64> {
    model
        .spec()
        .and_then(|spec| spec.input_limit())
        .map(u64::from)
}

/// A conversation with the main agent.
pub struct Session {
    agent: Agent,
    model: DynModel<Completion>,
    preamble: String,
    /// The tools the configuration enables, and the ones this conversation uses instead.
    default_tools: Toolset,
    tools_override: Option<Toolset>,
    /// The skills this conversation may not use, instead of the configured ones.
    skills_override: Option<SkillFilter>,
    history: Vec<Message>,
    frontend: Arc<dyn Frontend>,
    /// The window set in the configuration; it wins over the model catalog's.
    window_override: Option<u64>,
    context_window: Option<u64>,
    /// The `think` value sent with every request, if one was chosen.
    think: Option<Value>,
    /// The prompt being run and what it has done so far.
    running: Option<(String, Arc<Mutex<String>>)>,
}

impl Session {
    pub fn new(model: DynModel<Completion>, preamble: &str, frontend: Arc<dyn Frontend>) -> Self {
        let enabled = Toolset::default();
        Self {
            context_window: catalog_context_window(&model),
            agent: main_agent(model.clone(), preamble, frontend.clone(), &enabled),
            model,
            preamble: preamble.to_owned(),
            default_tools: enabled,
            tools_override: None,
            skills_override: None,
            window_override: None,
            history: Vec::new(),
            frontend,
            think: None,
            running: None,
        }
    }

    /// Sends `think` (Ollama's thinking level) with the following prompts; `None` leaves
    /// it to the model's default.
    pub fn set_thinking(&mut self, think: Option<Value>) {
        self.think = think;
    }

    /// Uses `tokens` as the model's context window instead of the catalog's, if given.
    pub fn with_context_window(mut self, tokens: Option<u64>) -> Self {
        self.window_override = tokens;
        self.context_window = tokens.or(self.context_window);
        self
    }

    /// Restricts the agent to the tools in `names`; `None` enables all.
    pub fn with_tools(mut self, names: Option<Vec<String>>) -> Self {
        self.default_tools = Toolset::only(names);
        self.rebuild();
        self
    }

    /// The skills in `names` are switched off, unless a conversation chooses otherwise.
    pub fn with_disabled_skills(mut self, names: Vec<String>) -> Self {
        self.default_tools = self.default_tools.with_skills(SkillFilter::disabled(names));
        self.rebuild();
        self
    }

    /// The tools and skills this conversation has: its own choice, else the configured ones.
    pub fn enabled_tools(&self) -> Toolset {
        let tools = self
            .tools_override
            .clone()
            .unwrap_or_else(|| self.default_tools.clone());
        let skills = self
            .skills_override
            .clone()
            .unwrap_or_else(|| self.default_tools.skills().clone());
        tools.with_skills(skills)
    }

    /// Switches skills off for this conversation only; `None` goes back to the configured ones.
    pub fn set_skills(&mut self, disabled: Option<Vec<String>>) {
        self.skills_override = disabled.map(SkillFilter::disabled);
        self.rebuild();
    }

    /// Gives this conversation its own set of tools; `None` goes back to the configured ones.
    pub fn set_tools(&mut self, names: Option<Vec<String>>) {
        self.tools_override = names.map(|names| Toolset::only(Some(names)));
        self.rebuild();
    }

    /// Applies a changed configuration: the model, its context window and the configured
    /// tools. The conversation continues with the new agent.
    pub fn reconfigure(
        &mut self,
        model: DynModel<Completion>,
        window: Option<u64>,
        tools: Option<Vec<String>>,
        disabled_skills: Vec<String>,
    ) {
        self.context_window = window.or(catalog_context_window(&model));
        self.window_override = window;
        self.model = model;
        self.default_tools = Toolset::only(tools).with_skills(SkillFilter::disabled(disabled_skills));
        self.rebuild();
    }

    /// A new conversation on the same model with `tools` and `frontend`, for a run that
    /// has nobody at the keyboard. `note` is added to the system prompt.
    pub fn fork(&self, frontend: Arc<dyn Frontend>, tools: Toolset, note: &str) -> Session {
        let preamble = format!("{}\n\n{note}", self.preamble);
        Session {
            agent: main_agent(self.model.clone(), &preamble, frontend.clone(), &tools),
            model: self.model.clone(),
            preamble,
            default_tools: tools,
            tools_override: None,
            skills_override: None,
            history: Vec::new(),
            frontend,
            window_override: self.window_override,
            context_window: self.context_window,
            think: None,
            running: None,
        }
    }

    /// Rebuilds the agent with the same settings, e.g. so that its system prompt lists
    /// skills that were added or changed.
    pub fn refresh(&mut self) {
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.agent = main_agent(
            self.model.clone(),
            &self.preamble,
            self.frontend.clone(),
            &self.enabled_tools(),
        );
    }

    /// The conversation so far, as the model sees it.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Continues another conversation from `history`.
    pub fn set_history(&mut self, history: Vec<Message>) {
        self.running = None;
        self.history = history;
    }

    /// Runs one prompt to the end; on success it becomes part of the history.
    /// Dropping the future cancels the prompt; call [`Session::commit_stopped`] after
    /// that (the next prompt does it too) so the history keeps the prompt's input and
    /// a digest of what the agent did before it was stopped.
    pub async fn prompt(&mut self, input: String) -> Result<(), String> {
        self.commit_stopped();
        let partial = Arc::new(Mutex::new(String::new()));
        self.running = Some((input.clone(), partial.clone()));
        let result = self.run_prompt(input, &partial).await;
        self.running = None;
        result
    }

    /// Remembers a prompt that was cancelled before it finished: the user's input, then
    /// what the agent had said and done as plain text (the results of the tool batch
    /// that was running are not known). Does nothing when no prompt was cancelled.
    pub fn commit_stopped(&mut self) {
        let Some((input, partial)) = self.running.take() else {
            return;
        };
        let done = partial.lock().unwrap().trim().to_string();
        let mut digest = String::from("[This answer was stopped by the user before it finished.");
        if done.is_empty() {
            digest.push_str(" Nothing had been done yet.]");
        } else {
            digest.push_str(" What I had said and done so far:]\n");
            digest.push_str(&done);
        }
        self.history.push(Message::user(input));
        self.history.push(Message::assistant(digest));
    }

    async fn run_prompt(&mut self, input: String, partial: &Mutex<String>) -> Result<(), String> {
        let mut request = self.agent.prompt(input);
        if let Some(think) = &self.think {
            let mut params = serde_json::Map::new();
            params.insert("think".into(), think.clone());
            request = request.merge_additional_params(params);
        }
        let mut stream = request
            .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
            .history(self.history.iter().cloned())
            .stream();
        let report = Report {
            frontend: &*self.frontend,
            context_window: self.context_window,
            partial: Some(partial),
        };
        let response = run_stream(&mut stream, Some(report)).await?;
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
        let tools = tools(
            &model.into(),
            "",
            &Toolset::default(),
            &(Arc::new(Silent) as Arc<dyn Frontend>),
        );
        Approve {
            frontend: Arc::new(Silent),
            context: Context { isolated: true },
            policies: tools.policies,
        }
    }
    #[test]
    fn tools_add_their_sentences_to_the_system_prompt() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let preamble = tools(
            &model.into(),
            "BASE",
            &Toolset::default(),
            &(Arc::new(Silent) as Arc<dyn Frontend>),
        )
        .preamble;
        assert!(preamble.starts_with("BASE\n\n"), "{preamble}");
        assert!(preamble.contains("Use the `execute` tool"), "{preamble}");
    }

    #[test]
    fn a_conversation_can_have_its_own_tools() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let mut session = Session::new(model.into(), "", Arc::new(Silent))
            .with_tools(Some(vec!["execute".into(), "agent".into()]));
        assert_eq!(session.enabled_tools().names(), ["execute", "agent"]);
        session.set_tools(Some(vec!["use_skill".into()]));
        assert_eq!(session.enabled_tools().names(), ["use_skill"]);
        session.set_tools(None); // back to the configured ones
        assert_eq!(session.enabled_tools().names(), ["execute", "agent"]);
        assert!(validate_tools(&["nope".into()]).is_err());
    }

    #[test]
    fn disabled_tools_leave_the_system_prompt() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let enabled = Toolset::only(Some(vec!["use_skill".into()]));
        let tools = tools(
            &model.into(),
            "BASE",
            &enabled,
            &(Arc::new(Silent) as Arc<dyn Frontend>),
        );
        assert!(!tools.preamble.contains("Use the `execute` tool"), "{}", tools.preamble);
        assert!(tools.policies.contains_key("use_skill"));
        assert!(!tools.policies.contains_key("execute"));
    }

    #[test]
    fn skills_are_chosen_per_conversation_and_by_default() {
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let mut session = Session::new(model.into(), "", Arc::new(Silent))
            .with_disabled_skills(vec!["a".into()]);
        let allows = |s: &Session, name: &str| s.enabled_tools().skills().allows(name);
        assert!(!allows(&session, "a") && allows(&session, "b"));
        session.set_skills(Some(vec!["b".into()])); // its own choice replaces the default
        assert!(allows(&session, "a") && !allows(&session, "b") && allows(&session, "new"));
        session.set_skills(None);
        assert!(!allows(&session, "a") && allows(&session, "b"));
        // The tools stay as they are, whichever way skills are changed.
        session.set_tools(Some(vec!["execute".into()]));
        assert_eq!(session.enabled_tools().names(), ["execute"]);
        assert!(!allows(&session, "a"));
    }

    #[test]
    fn disabled_skills_leave_the_system_prompt() {
        use std::fs;
        let name = format!("zz-prompt-test-{}", std::process::id());
        let dir = std::path::Path::new(skills::DIR).join(&name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: Only for the test.\n---\n")).unwrap();
        let model = MockCompletionModel::from_stream_turns(Vec::<Vec<MockStreamEvent>>::new());
        let frontend = &(Arc::new(Silent) as Arc<dyn Frontend>);
        let on = tools(&model.clone().into(), "B", &Toolset::default(), frontend).preamble;
        let off = Toolset::default().with_skills(SkillFilter::disabled([name.clone()]));
        let off = tools(&model.into(), "B", &off, frontend).preamble;
        let _ = fs::remove_dir_all(&dir);
        assert!(on.contains(&name), "{on}");
        assert!(!off.contains(&name), "{off}");
    }

    #[test]
    fn calls_get_their_tools_policy() {
        let hook = approve_hook();
        let call = hook.tool_call("1".into(), "execute", r#"{"command":"ls"}"#);
        assert_eq!(
            (call.kind, call.title.as_str(), call.ask),
            ("execute", "[isolated] ls", false)
        );
        // Arguments that don't parse and unknown tools are always asked for.
        let call = hook.tool_call("2".into(), "execute", "not json");
        assert!(call.ask);
        assert_eq!(call.title, "execute not json");
        assert!(hook.tool_call("3".into(), "nope", r#"{"code":"1"}"#).ask);
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
            let args = serde_json::json!({"command": "echo hi"});
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
                .filter_map(|e| match e {
                    Event::Note(n) => Some(n.lines().next().unwrap().to_string()),
                    Event::ToolStarted(c) => Some(format!("started {}", c.id)),
                    Event::ToolDenied(c) => Some(format!("denied {}", c.id)),
                    Event::ToolFinished { id, failed, .. } => {
                        Some(format!("finished {id} {failed}"))
                    }
                    Event::Text(t) => Some(t.clone()),
                    Event::Reasoning(_) => None,
                    Event::Context(_)
                    | Event::SubagentStarted { .. }
                    | Event::SubagentFinished { .. }
                    | Event::Sub { .. } => None,
                })
                .collect();
            let ask = "ask execute: echo hi".to_string();
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
    /// After each model call the frontend learns how full the context is: that call's
    /// input and output, not a sum over the calls.
    #[tokio::test]
    async fn context_usage_is_reported_after_each_model_call() {
        use rig_core::completion::Usage;
        let usage = |input, output| {
            MockStreamEvent::final_response(Usage::new().input_tokens(input).output_tokens(output))
        };
        let args = serde_json::json!({"command": "echo hi"});
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call("t1", "execute", args),
                usage(1_000, 50),
            ],
            vec![MockStreamEvent::text("done"), usage(1_100, 200)],
        ]);
        let frontend = Arc::new(Recorder {
            events: Mutex::new(Vec::new()),
            approve: false,
        });
        let mut session =
            Session::new(model.into(), "", frontend.clone()).with_context_window(Some(4_000));
        session.prompt("hi".into()).await.unwrap();
        let reported: Vec<_> = frontend
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Context(usage) => Some(*usage),
                _ => None,
            })
            .collect();
        let at = |tokens| ContextUsage {
            tokens,
            window: Some(4_000),
        };
        assert_eq!(reported, [at(1_050), at(1_300)]);
    }

    /// A stopped prompt must not be forgotten: the next request still sees the input and
    /// what the agent had said.
    #[tokio::test]
    async fn stopped_prompt_stays_in_the_history() {
        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("partial answer"),
            MockStreamEvent::final_response_with_total_tokens(1),
        ]]);
        let mut session = Session::new(model.into(), "", Arc::new(Silent));
        // Cancel after the model has produced its text but before the run settles.
        let partial = Arc::new(Mutex::new("partial answer".to_string()));
        session.running = Some(("do the thing".into(), partial));
        session.commit_stopped();
        assert_eq!(session.history().len(), 2);
        assert!(format!("{:?}", session.history()[1]).contains("partial answer"));
        // Committing twice adds nothing.
        session.commit_stopped();
        assert_eq!(session.history().len(), 2);
    }

    /// The window comes from rig's model catalog; models it lists without one (most
    /// Ollama models) have none until `AGENT_CONTEXT_WINDOW` sets it.
    #[test]
    fn context_window_comes_from_the_model_catalog() {
        use rig_core::providers::anthropic::{self, AnthropicConfig};
        use rig_core::providers::ollama::{self, OllamaConfig};
        use rig_core::providers::openai::{self, OpenAIConfig};
        let claude = AnthropicConfig::new("key")
            .client()
            .completion(anthropic::CLAUDE_SONNET_5_5)
            .into();
        assert_eq!(catalog_context_window(&claude), Some(1_000_000));
        // GPT-5.5 reads less than its window.
        let gpt = OpenAIConfig::new("key")
            .client()
            .completion(openai::GPT_5_5)
            .into();
        assert_eq!(catalog_context_window(&gpt), Some(922_000));
        let qwen = OllamaConfig::new()
            .client()
            .completion(ollama::QWEN3)
            .into();
        assert_eq!(catalog_context_window(&qwen), None);
        let unknown = OpenAIConfig::new("key")
            .client()
            .completion("my-finetune")
            .into();
        assert_eq!(catalog_context_window(&unknown), None);
        let session = Session::new(qwen, "", Arc::new(Silent));
        assert_eq!(session.context_window, None);
        let session = session.with_context_window(Some(32_768));
        assert_eq!(session.context_window, Some(32_768));
        let session = Session::new(claude, "", Arc::new(Silent)).with_context_window(None);
        assert_eq!(session.context_window, Some(1_000_000));
    }

    /// Image data must not be dumped into the frontends.
    #[test]
    fn image_output_is_shown_as_a_placeholder() {
        let output = ToolOutput::content(vec![
            ToolResultContent::text("a.png (png, 9 bytes)"),
            ToolResultContent::image_base64("AAAA", None, None),
        ])
        .unwrap();
        assert_eq!(output_text(&output), "a.png (png, 9 bytes)\n[image]");
        assert_eq!(output_text(&ToolOutput::text("hi")), "hi");
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
