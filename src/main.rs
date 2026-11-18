mod config;
mod frontend;
mod harness;
mod sandbox;
mod tools;

use frontend::{acp, tui};
use harness::Session;
use std::path::Path;
use std::sync::Arc;
use tools::skills;

const PREAMBLE: &str = "You are a helpful agent. Use the `execute` tool to run shell commands and `execute_javascript` to run JavaScript. Both run in a sandbox (writes only in the working directory and temp, no internet, no access to .env files); set `disable_sandbox` (execute) or `allow_network` (execute_javascript) only if a call fails with \"Operation not permitted\" or needs the network. Use `use_skill` to load a skill's instructions and `agent` to delegate self-contained tasks (large searches, reading many files) to a subagent that only reports back its result.";

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
        return tools::execute_javascript::runtime::run_js(file, rest).await;
    }
    // `--env-example` (re)generates `.env.example` from the config.
    if let [_, flag] = args.as_slice()
        && flag == "--env-example"
    {
        std::fs::write(".env.example", config::env_example())?;
        println!("wrote .env.example");
        return Ok(());
    }
    // `--acp` serves the Agent Client Protocol on stdin/stdout instead of the terminal UI.
    let acp = matches!(args.as_slice(), [_, flag] if flag == "--acp");

    // First start: create `.env.example` so the user knows what to configure.
    if !Path::new(".env.example").exists() {
        std::fs::write(".env.example", config::env_example())?;
        let note = "created .env.example; copy it to .env, fill it in and start again";
        // In ACP mode stdout belongs to the protocol.
        if acp {
            eprintln!("{note}");
        } else {
            println!("{note}");
        }
        return Ok(());
    }

    let model = config::Config::load()?.model()?;
    let skills = skills::Skills::load(Path::new(skills::DIR))?;
    for warning in &skills.warnings {
        eprintln!("skills: {warning}");
    }
    let preamble = format!("{}{}", PREAMBLE, skills.prompt(&relative_exe()?));

    if acp {
        acp::serve(|frontend| Session::new(model.clone(), &preamble, frontend)).await?;
    } else {
        tui::run(Session::new(model, &preamble, Arc::new(tui::Tui))).await?;
    }
    Ok(())
}
