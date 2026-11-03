mod js;
mod sandbox;

use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig_agent::{AgentBuilder, rig_tool};
use rig_core::effect::EffectKind;
use rig_core::providers::openai::{self, OpenAI};
use rig_core::serde_json;
use rig_core::tool::ToolExecutionError;
use std::io::{self, Write};

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
        sandbox::command("sh", ["-c", &command], sandbox::Network::Localhost)
            .map_err(ToolExecutionError::from_error)?
    } else {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &command]);
        cmd
    };
    let out = cmd.output().await.map_err(ToolExecutionError::from_error)?;
    Ok(format!(
        "exit: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Asks the user before every tool call; anything but `y` skips it.
/// Sandboxed `execute` calls are approved automatically.
struct Approve;

/// Returns the command of an `execute` call that will run sandboxed.
fn sandboxed_command(name: &str, args: &str) -> Option<String> {
    if name != "execute" || !sandbox::supported() {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(args).ok()?;
    if args["disable_sandbox"].as_bool() == Some(true) {
        return None;
    }
    args["command"].as_str().map(str::to_owned)
}

impl AgentHook for Approve {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::ToolCall { name, args } = event.kind else {
            return DispatchAction::proceed();
        };
        if let Some(command) = sandboxed_command(name, args) {
            println!("[sandboxed] {command}");
            return DispatchAction::proceed();
        }
        match ask(&format!("{name} {args}\napprove? [y/N] ")) {
            Ok(answer) if answer == "y" => DispatchAction::proceed(),
            _ => DispatchAction::skip("the user denied this tool call"),
        }
    }
}

fn ask(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// Runs a JS file in a sandboxed copy of this binary (`--js-child`), or directly if unsupported.
async fn run_js_sandboxed(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !sandbox::supported() {
        return js::run_js(file).await;
    }
    let status = sandbox::command(
        std::env::current_exe()?,
        ["--js-child", file],
        sandbox::Network::Internet,
    )?
    .status()
    .await?;
    std::process::exit(status.code().unwrap_or(1));
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, file] = args.as_slice()
        && flag == "--js"
    {
        return run_js_sandboxed(file).await;
    }
    if let [_, flag, file] = args.as_slice()
        && flag == "--js-child"
    {
        return js::run_js(file).await;
    }

    let agent = AgentBuilder::new(OpenAI::from_env()?.completion(openai::GPT_5_5))
        .preamble("You are a helpful agent. Use the `execute` tool to run shell commands. Commands run in a sandbox (writes only in the working directory and temp, no internet); set `disable_sandbox` only if a command fails with \"Operation not permitted\" or needs the network.")
        .tool(Execute) // add more tools here
        .add_hook(Approve)
        .default_max_turns(20)
        .build();

    let mut history = Vec::new();
    loop {
        let input = ask("> ")?;
        if input.is_empty() {
            break;
        }
        println!("{}\n", agent.chat(input, &mut history).await?);
    }
    Ok(())
}
