use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};
use rig_agent::{AgentBuilder, rig_tool};
use rig_core::effect::EffectKind;
use rig_core::providers::openai::{self, OpenAI};
use rig_core::tool::ToolExecutionError;
use std::io::{self, Write};

/// Execute a shell command and return its exit code, stdout and stderr.
#[rig_tool(params(command = "The shell command to run"))]
async fn execute(command: String) -> Result<String, ToolExecutionError> {
    let out = tokio::process::Command::new("sh")
        .args(["-c", &command])
        .output()
        .await
        .map_err(ToolExecutionError::from_error)?;
    Ok(format!(
        "exit: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Asks the user before every tool call; anything but `y` skips it.
struct Approve;

impl AgentHook for Approve {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::ToolCall { name, args } = event.kind else {
            return DispatchAction::proceed();
        };
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = AgentBuilder::new(OpenAI::from_env()?.completion(openai::GPT_5_5))
        .preamble("You are a helpful agent. Use the `execute` tool to run shell commands.")
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
