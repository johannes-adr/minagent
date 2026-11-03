mod config;
mod js;
mod sandbox;

use futures::StreamExt;
use rig_agent::agent::{
    AgentHook, DispatchAction, DispatchEvent, HookContext, MultiTurnStreamItem, StreamingResult,
};
use rig_agent::run::response::PromptResponse;
use rig_agent::streaming::{Item, StreamEvent};
use rig_agent::{AgentBuilder, rig_tool};
use rig_core::effect::EffectKind;
use rig_core::serde_json;
use rig_core::tool::ToolExecutionError;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Output, Stdio};
use tokio::io::AsyncWriteExt;

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
    let out = cmd.output().await.map_err(ToolExecutionError::from_error)?;
    Ok(format_output(&out))
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
    let out = run_js_code(&exe, &code, allow_network == Some(true)).await?;
    println!("{out}");
    Ok(out)
}

/// Runs `code` via `<exe> --js -`, sandboxed where supported. The code goes through
/// stdin, not a temp file, so sandboxed processes can't swap it after approval.
async fn run_js_code(
    exe: &Path,
    code: &str,
    allow_network: bool,
) -> Result<String, ToolExecutionError> {
    let args = ["--js", "-"];
    let mut cmd = if sandbox::supported() {
        sandbox::command(exe, args, allow_network).map_err(ToolExecutionError::from_error)?
    } else {
        let mut cmd = tokio::process::Command::new(exe);
        cmd.args(args);
        cmd
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ToolExecutionError::from_error)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(code.as_bytes())
        .await
        .map_err(ToolExecutionError::from_error)?;
    drop(stdin);
    let out = child
        .wait_with_output()
        .await
        .map_err(ToolExecutionError::from_error)?;
    Ok(format_output(&out))
}

/// Asks the user before every tool call; anything but `y` skips it.
/// Sandboxed calls without extra rights are approved automatically.
struct Approve;

enum Approval {
    /// Run without asking; the text is printed.
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

impl AgentHook for Approve {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::ToolCall { name, args } = event.kind else {
            return DispatchAction::proceed();
        };
        let prompt = match decide(name, args, sandbox::supported()) {
            Approval::Auto(text) => {
                println!("{text}");
                return DispatchAction::proceed();
            }
            Approval::Ask(prompt) => prompt,
        };
        match ask(&prompt) {
            Ok(answer) if answer == "y" => DispatchAction::proceed(),
            _ => DispatchAction::skip("the user denied this tool call"),
        }
    }
}

/// Prints the streamed answer text (no reasoning) and returns the final response.
async fn print_stream(
    stream: &mut StreamingResult,
) -> Result<PromptResponse, Box<dyn std::error::Error>> {
    while let Some(item) = stream.next().await {
        match item? {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text, ..
            })) => {
                print!("{text}");
                io::stdout().flush()?;
            }
            MultiTurnStreamItem::FinalResponse(response) => return Ok(response),
            _ => {}
        }
    }
    Err("the stream ended without a final response".into())
}

fn ask(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    // `--js <file|->` runs JS directly; sandboxing is up to the caller (`execute`, `execute_javascript`).
    if let [_, flag, file] = args.as_slice()
        && flag == "--js"
    {
        return js::run_js(file).await;
    }
    // `--env-example` (re)generates `.env.example` from the config.
    if let [_, flag] = args.as_slice()
        && flag == "--env-example"
    {
        std::fs::write(".env.example", config::env_example())?;
        println!("wrote .env.example");
        return Ok(());
    }

    // First start: create `.env.example` so the user knows what to configure.
    if !Path::new(".env.example").exists() {
        std::fs::write(".env.example", config::env_example())?;
        println!("created .env.example; copy it to .env, fill it in and start again");
        return Ok(());
    }

    let model = config::Config::load()?.model()?;
    let agent = AgentBuilder::new(model)
        .preamble("You are a helpful agent. Use the `execute` tool to run shell commands and `execute_javascript` to run JavaScript. Both run in a sandbox (writes only in the working directory and temp, no internet, no access to .env files); set `disable_sandbox` (execute) or `allow_network` (execute_javascript) only if a call fails with \"Operation not permitted\" or needs the network.")
        .tool(Execute)
        .tool(ExecuteJavascript) // add more tools here
        .add_hook(Approve)
        .default_max_turns(20)
        .build();

    let mut history = Vec::new();
    loop {
        let input = ask("> ")?;
        if input.is_empty() {
            break;
        }
        let mut stream = agent.prompt(input).history(history.iter().cloned()).stream();
        let response = print_stream(&mut stream).await?;
        history.extend(response.messages);
        println!("\n");
    }
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
        run_js_code(&agent_exe(), code, allow_network)
            .await
            .unwrap()
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
