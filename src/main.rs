mod config;
mod frontend;
mod harness;
mod tools;

use frontend::{acp, tui};
use harness::Session;
use std::path::Path;
use std::sync::Arc;
use tools::skills;

const PREAMBLE_START: &str =
    "You are a helpful agent. Use the `execute` tool to run shell commands. ";
const PREAMBLE_LOCAL: &str =
    "It runs directly on this machine, so the user must approve every call. ";
const PREAMBLE_ISOLATED: &str = "It runs in an isolated container; network access may be restricted to allowed hosts by a proxy. ";
const PREAMBLE_END: &str = "Use `use_skill` to load a skill's instructions and `agent` to delegate self-contained tasks (large searches, reading many files) to a subagent that only reports back its result.";

/// The system prompt; the environment hint depends on whether the harness is `isolated`.
fn base_preamble(isolated: bool) -> String {
    let environment = if isolated {
        PREAMBLE_ISOLATED
    } else {
        PREAMBLE_LOCAL
    };
    format!("{PREAMBLE_START}{environment}{PREAMBLE_END}")
}

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

    let model = match config::Config::load()?.model() {
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
    let skills = skills::Skills::load(Path::new(skills::DIR))?;
    for warning in &skills.warnings {
        eprintln!("skills: {warning}");
    }
    let preamble = format!("{}{}", base_preamble(harness::isolated()), skills.prompt());

    if acp {
        acp::serve(|frontend| Session::new(model.clone(), &preamble, frontend)).await?;
    } else {
        tui::run(Session::new(model, &preamble, Arc::new(tui::Tui))).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_describes_the_environment() {
        let local = base_preamble(false);
        assert!(local.contains("must approve every call"), "{local}");
        let isolated = base_preamble(true);
        assert!(isolated.contains("isolated container"), "{isolated}");
    }
}
