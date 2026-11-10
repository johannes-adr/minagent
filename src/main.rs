mod config;
mod js;
mod prompt;
mod sandbox;
mod skills;
mod tui;

use futures::StreamExt;
use rig_agent::agent::{
    AgentHook, DispatchAction, DispatchEvent, HookContext, MultiTurnStreamItem, StreamingResult,
};
use rig_agent::run::policy::{InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason};
use rig_agent::run::response::PromptResponse;
use rig_agent::streaming::{Item, StreamEvent};
use rig_agent::{AgentBuilder, rig_tool};
use rig_core::effect::EffectKind;
use rig_core::serde_json;
use rig_core::tool::ToolExecutionError;
use std::path::Path;
use std::process::Output;
use std::time::Duration;
use tui::{Status, UiEvent};

/// Maximum run time of a single `execute` / `execute_javascript` call.
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

fn timed_out(timeout: Duration) -> ToolExecutionError {
    ToolExecutionError::from_error(std::io::Error::other(format!(
        "timed out after {}s; the process was killed",
        timeout.as_secs()
    )))
}

fn format_output(out: &Output) -> String {
    format!(
        "exit: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Execute a shell command and return its exit code, stdout and stderr.
#[rig_tool(params(
    command = "The shell command to run",
    disable_sandbox = "Set to true only when a command failed with \"Operation not permitted\" or needs network access (e.g. installs, git push); the user must then approve it."
))]
async fn execute(
    command: String,
    disable_sandbox: Option<bool>,
) -> Result<String, ToolExecutionError> {
    let mut cmd = if sandbox::supported() && disable_sandbox != Some(true) {
        sandbox::command("sh", ["-c", &command], false).map_err(ToolExecutionError::from_error)?
    } else {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &command]);
        cmd
    };
    // Dropping the future on timeout kills the child.
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(TOOL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| timed_out(TOOL_TIMEOUT))?
        .map_err(ToolExecutionError::from_error)?;
    let out = format_output(&out);
    tui::emit(UiEvent::ToolOutput(out.clone()));
    Ok(out)
}

/// Run JavaScript (ES module) with fetch, node:fs etc. and return its exit code, stdout and stderr.
/// Sandboxed: writes only in the working directory and temp, network only to localhost
/// unless `allow_network` is set.
#[rig_tool(params(
    code = "JavaScript ES module to run (top-level await, fetch, node:fs, node:path, timers, URL, crypto, ...). Print results with console.log.",
    allow_network = "Set to true only when the script needs internet access; the user must then approve it."
))]
async fn execute_javascript(
    code: String,
    allow_network: Option<bool>,
) -> Result<String, ToolExecutionError> {
    let exe = std::env::current_exe().map_err(ToolExecutionError::from_error)?;
    let out = run_js_code(&exe, &code, allow_network == Some(true), TOOL_TIMEOUT).await?;
    tui::emit(UiEvent::ToolOutput(out.clone()));
    Ok(out)
}

/// Runs `code` via `<exe> --js -`, sandboxed where supported. The code goes through
/// stdin, not a temp file, so sandboxed processes can't swap it after approval.
/// The process is killed after `timeout`.
async fn run_js_code(
    exe: &Path,
    code: &str,
    allow_network: bool,
    timeout: Duration,
) -> Result<String, ToolExecutionError> {
    tokio::time::timeout(timeout, run_js_process(exe, code, allow_network))
        .await
        .map_err(|_| timed_out(timeout))?
}

async fn run_js_process(
    exe: &Path,
    code: &str,
    allow_network: bool,
) -> Result<String, ToolExecutionError> {
    let out = js::run_sandboxed(exe, code, allow_network)
        .await
        .map_err(ToolExecutionError::from_error)?;
    Ok(format_output(&out))
}

/// Asks the user (approval popup) before every tool call; anything but `y` skips it.
/// Sandboxed calls without extra rights are approved automatically.
struct Approve;

enum Approval {
    /// Run without asking; the text is shown in the chat.
    Auto(String),
    /// Ask the user with this prompt.
    Ask(String),
}

fn decide(name: &str, args: &str, sandboxed: bool) -> Approval {
    let raw = || Approval::Ask(format!("{name} {args}\napprove? [y/N] "));
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return raw();
    };
    match (name, v["command"].as_str(), v["code"].as_str()) {
        ("execute", Some(command), _)
            if sandboxed && v["disable_sandbox"].as_bool() != Some(true) =>
        {
            Approval::Auto(format!("[sandboxed] {command}"))
        }
        ("execute_javascript", _, Some(code)) => {
            let network = v["allow_network"].as_bool() == Some(true);
            if sandboxed && !network {
                let first = code.lines().next().unwrap_or_default();
                let more = if code.lines().nth(1).is_some() {
                    " ..."
                } else {
                    ""
                };
                Approval::Auto(format!("[sandboxed] execute_javascript: {first}{more}"))
            } else {
                let note = match (network, sandboxed) {
                    (true, true) => "with internet access",
                    (true, false) => "with internet access, not sandboxed",
                    (false, _) => "not sandboxed",
                };
                Approval::Ask(format!(
                    "execute_javascript ({note}):\n{code}\napprove? [y/N] "
                ))
            }
        }
        _ => raw(),
    }
}

/// How often the model may retry after calling a tool that doesn't exist, per prompt.
const INVALID_TOOL_CALL_RETRIES: usize = 3;

fn unknown_tool_feedback(tool: &str, available: &[String]) -> String {
    format!(
        "There is no tool named `{tool}`. Available tools: {}. Retry with one of these \
         (use `execute` for shell commands, `execute_javascript` for JavaScript). \
         If `{tool}` is a skill, it is not a tool: load it by running the use_skill.js command \
         from the Skills section with the `execute` tool.",
        available.join(", ")
    )
}

impl AgentHook for Approve {
    async fn on_invalid_tool_call(
        &self,
        _: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        match event.reason {
            InvalidToolCallReason::UnknownTool => {
                tui::emit(UiEvent::Info(format!("[unknown tool] {}", event.tool_name)));
                Some(InvalidToolCallAction::retry(unknown_tool_feedback(
                    &event.tool_name,
                    &event.available_tools,
                )))
            }
            _ => None,
        }
    }

    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::ToolCall { name, args } = event.kind else {
            return DispatchAction::proceed();
        };
        let prompt = match decide(name, args, sandbox::supported()) {
            Approval::Auto(text) => {
                tui::emit(UiEvent::Info(text));
                tui::emit(UiEvent::Status(Status::Tool));
                return DispatchAction::proceed();
            }
            Approval::Ask(prompt) => prompt,
        };
        if tui::request_approval(prompt).await {
            tui::emit(UiEvent::Info(format!("[approved] {name}")));
            tui::emit(UiEvent::Status(Status::Tool));
            DispatchAction::proceed()
        } else {
            tui::emit(UiEvent::Info(format!("[denied] {name}")));
            DispatchAction::skip("the user denied this tool call")
        }
    }
}

/// Forwards the streamed answer text (no reasoning) to the UI and returns the final response.
async fn forward_stream(stream: &mut StreamingResult) -> Result<PromptResponse, String> {
    while let Some(item) = stream.next().await {
        match item.map_err(|e| e.to_string())? {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            })) => tui::emit(UiEvent::Text(text)),
            MultiTurnStreamItem::ToolResult { .. } => tui::emit(UiEvent::Status(Status::Thinking)),
            MultiTurnStreamItem::FinalResponse(response) => return Ok(response),
            _ => {}
        }
    }
    Err("the stream ended without a final response".into())
}

/// Path of this executable relative to the working directory when it lies below it
/// (e.g. `target/debug/agent`), so the model has less to write; otherwise absolute.
fn relative_exe() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let relative = std::env::current_dir()
        .ok()
        .and_then(|cwd| exe.strip_prefix(cwd).ok().map(Path::to_path_buf));
    Ok(relative.unwrap_or(exe))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    // `--js <file|-> [args...]` runs JS directly (args land in `globalThis.ARGS`);
    // sandboxing is up to the caller (`execute`, `execute_javascript`).
    if let [_, flag, file, rest @ ..] = args.as_slice()
        && flag == "--js"
    {
        return js::run_js(file, rest).await;
    }
    // `--env-example` (re)generates `.env.example` from the config.
    if let [_, flag] = args.as_slice()
        && flag == "--env-example"
    {
        std::fs::write(".env.example", config::env_example())?;
        println!("wrote .env.example");
        return Ok(());
    }

    // `--system-prompt` prints the system prompt built by `system_prompt/system.js`.
    if let [_, flag] = args.as_slice()
        && flag == "--system-prompt"
    {
        skills::install_builtin(Path::new(skills::DIR))?;
        println!("{}", prompt::build(&std::env::current_exe()?, &relative_exe()?).await?);
        return Ok(());
    }

    // First start: create `.env.example` so the user knows what to configure.
    if !Path::new(".env.example").exists() {
        std::fs::write(".env.example", config::env_example())?;
        println!("created .env.example; copy it to .env, fill it in and start again");
        return Ok(());
    }

    let model = config::Config::load()?.model()?;
    skills::install_builtin(Path::new(skills::DIR))?;
    let preamble = prompt::build(&std::env::current_exe()?, &relative_exe()?).await?;
    let agent = AgentBuilder::new(model)
        .preamble(&preamble)
        .tool(Execute)
        .tool(ExecuteJavascript) // add more tools here
        .add_hook(Approve)
        .default_max_turns(20)
        .build();

    // The agent runs in its own task: prompts come in from the UI, events go back via `tui::emit`.
    let (prompts, mut inbox) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut history = Vec::new();
        while let Some(input) = inbox.recv().await {
            let mut stream = agent
                .prompt(input)
                .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
                .history(history.iter().cloned())
                .stream();
            match forward_stream(&mut stream).await {
                Ok(response) => history.extend(response.messages),
                Err(error) => tui::emit(UiEvent::Error(error)),
            }
            tui::emit(UiEvent::Status(Status::Idle));
        }
    });
    tui::run(prompts).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The agent binary next to the test binary (`target/<profile>/agent`); it is built
    /// together with the integration test in `tests/js_cli.rs`.
    fn agent_exe() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let exe = exe.parent().unwrap().parent().unwrap().join("agent");
        assert!(exe.exists(), "build the binary first: cargo build");
        exe
    }

    async fn js(code: &str, allow_network: bool) -> String {
        run_js_code(&agent_exe(), code, allow_network, Duration::from_secs(30))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn js_tool_times_out() {
        let started = std::time::Instant::now();
        let result = run_js_code(
            &agent_exe(),
            "while (true) {}",
            false,
            Duration::from_millis(500),
        )
        .await;
        let error = result.expect_err("an endless loop must time out").to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn js_tool_memory_is_limited() {
        let code = "const a = []; while (true) a.push(new Array(1e6).fill(1));";
        let out = js(code, false).await;
        assert!(!out.contains("exit: exit status: 0"), "{out}");
        assert!(out.to_lowercase().contains("memory"), "{out}");
    }

    #[tokio::test]
    async fn js_tool_returns_console_output() {
        let out = js("console.log('hello ' + (1 + 1));", false).await;
        assert!(out.contains("exit: exit status: 0"), "{out}");
        assert!(out.contains("hello 2"), "{out}");
    }

    #[tokio::test]
    async fn js_tool_cannot_write_outside_workspace() {
        if !sandbox::supported() {
            return;
        }
        let target = PathBuf::from(std::env::var("HOME").unwrap()).join("agent_js_escape_test.txt");
        let code = format!(
            "import fs from 'node:fs'; fs.writeFileSync({:?}, 'x');",
            target.display().to_string()
        );
        let out = js(&code, false).await;
        let created = target.exists();
        let _ = std::fs::remove_file(&target);
        assert!(!created, "{out}");
        assert!(out.contains("Operation not permitted"), "{out}");
    }

    /// Needs internet, so it is not run by default: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires internet access"]
    async fn js_tool_fetch_only_with_allow_network() {
        if !sandbox::supported() {
            return;
        }
        let code = "const r = await fetch('https://example.com'); console.log('status', r.status);";
        let out = js(code, false).await;
        assert!(!out.contains("status 200"), "{out}");
        let out = js(code, true).await;
        assert!(out.contains("status 200"), "{out}");
    }

    fn auto(name: &str, args: &str, sandboxed: bool) -> bool {
        matches!(decide(name, args, sandboxed), Approval::Auto(_))
    }

    #[test]
    fn approval_decisions() {
        assert!(auto("execute", r#"{"command":"ls"}"#, true));
        assert!(!auto("execute", r#"{"command":"ls"}"#, false));
        assert!(!auto(
            "execute",
            r#"{"command":"ls","disable_sandbox":true}"#,
            true
        ));
        assert!(auto("execute_javascript", r#"{"code":"1"}"#, true));
        assert!(auto(
            "execute_javascript",
            r#"{"code":"1","allow_network":false}"#,
            true
        ));
        assert!(!auto("execute_javascript", r#"{"code":"1"}"#, false));
        assert!(!auto(
            "execute_javascript",
            r#"{"code":"1","allow_network":true}"#,
            true
        ));
        assert!(!auto("execute", "not json", true));
        assert!(!auto("execute_javascript", "{", true));
        assert!(!auto("unknown_tool", r#"{"code":"1"}"#, true));
    }

    /// A call to a nonexistent tool must not end the run: the model gets feedback and retries.
    #[tokio::test]
    async fn unknown_tool_call_is_retried() {
        use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
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
        let agent = AgentBuilder::new(model)
            .tool(Execute)
            .add_hook(Approve)
            .default_max_turns(20)
            .build();
        let mut stream = agent
            .prompt("hi")
            .max_invalid_tool_call_retries(INVALID_TOOL_CALL_RETRIES)
            .stream();
        let response = forward_stream(&mut stream).await.expect("run must continue");
        assert_eq!(response.output(), "done");
        assert_eq!(recorded.request_count(), 2);
    }

    #[test]
    fn unknown_tool_feedback_lists_available_tools() {
        let text = unknown_tool_feedback("bash", &["execute".into(), "execute_javascript".into()]);
        assert!(text.contains("`bash`") && text.contains("execute, execute_javascript"), "{text}");
    }

    #[test]
    fn network_prompt_shows_code_unescaped() {
        let args = r#"{"code":"console.log(\"hi\");\nawait fetch('x')","allow_network":true}"#;
        let Approval::Ask(prompt) = decide("execute_javascript", args, true) else {
            panic!("must ask");
        };
        assert!(prompt.contains("with internet access"), "{prompt}");
        assert!(
            prompt.contains("console.log(\"hi\");\nawait fetch('x')"),
            "{prompt}"
        );
        assert!(prompt.ends_with("approve? [y/N] "));
    }
}
