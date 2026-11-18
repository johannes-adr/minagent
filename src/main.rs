mod config;
mod frontend;
mod harness;
mod tools;

use frontend::{acp, tui};
use harness::Session;
use std::path::Path;
use std::sync::Arc;
use tools::skills;

/// The system prompt before the tools add their own sentences (see `ToolPolicy::prompt`).
const PREAMBLE: &str = "You are a helpful agent.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
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

    let config = config::Config::load()?;
    if let Some(tools) = config.tools()
        && let Err(err) = harness::enable_only(tools)
    {
        // In ACP mode stdout belongs to the protocol.
        eprintln!("{err}");
        return Err(err.into());
    }
    #[cfg(feature = "javascript")]
    match config.javascript() {
        Ok(settings) => tools::execute_javascript::configure(settings),
        Err(err) => {
            // In ACP mode stdout belongs to the protocol.
            eprintln!("{err}");
            return Err(err.into());
        }
    }
    let model = match config.model() {
        Ok(model) => model,
        Err(err) => {
            // First start without config: create `.env.example` so the user knows what to configure.
            let mut note = err.clone();
            if !Path::new(".env.example").exists() {
                std::fs::write(".env.example", config::env_example())?;
                note += "\ncreated .env.example; copy it to .env, fill it in and start again";
            }
            // In ACP mode stdout belongs to the protocol.
            if acp {
                eprintln!("{note}");
            } else {
                println!("{note}");
            }
            return Err(err.into());
        }
    };
    // Installs the built-in skills; the `use_skill` tool lists them in the system prompt.
    let skills = skills::Skills::load(Path::new(skills::DIR))?;
    for warning in &skills.warnings {
        eprintln!("skills: {warning}");
    }

    if acp {
        acp::serve(|frontend| Session::new(model.clone(), PREAMBLE, frontend)).await?;
    } else {
        tui::run(Session::new(model, PREAMBLE, Arc::new(tui::Tui))).await?;
    }
    Ok(())
}
