mod config;
mod frontend;
mod harness;
mod sessions;
mod thinking;
mod tools;

use frontend::{acp, tui, web};
use harness::Session;
use std::path::Path;
use std::sync::Arc;
use tools::skills;

/// The system prompt before the tools add their own sentences (see `ToolPolicy::prompt`).
const PREAMBLE: &str = "You are a helpful agent.";

/// Project instructions (https://agents.md), read from the working directory.
const AGENTS_MD: &str = "AGENTS.md";

/// [`PREAMBLE`] followed by the contents of `AGENTS.md` in `dir`, if there is a non-empty one.
fn preamble_with_agents_md(dir: &Path) -> String {
    match std::fs::read_to_string(dir.join(AGENTS_MD)) {
        Ok(text) if !text.trim().is_empty() => format!(
            "{PREAMBLE}\n\n# Project instructions ({AGENTS_MD})\n\nFollow these instructions \
             from the project's {AGENTS_MD}:\n\n{}",
            text.trim()
        ),
        _ => PREAMBLE.to_owned(),
    }
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
    // `--web [addr]` serves the browser UI instead.
    let web_addr = match args.as_slice() {
        [_, flag] if flag == "--web" => Some(web::DEFAULT_ADDR),
        [_, flag, addr] if flag == "--web" => Some(addr.as_str()),
        _ => None,
    };

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
    let context_window = match config.context_window() {
        Ok(tokens) => tokens,
        Err(err) => {
            // In ACP mode stdout belongs to the protocol.
            eprintln!("{err}");
            return Err(err.into());
        }
    };
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

    let preamble = preamble_with_agents_md(Path::new("."));
    let new_session = |frontend| {
        Session::new(model.clone(), &preamble, frontend).with_context_window(context_window)
    };
    if acp {
        acp::serve(new_session).await?;
    } else if let Some(addr) = web_addr {
        // The slider offers the levels Ollama lists for the model; other backends tell none.
        let thinking = match config.ollama() {
            Some((url, key, model)) => {
                thinking::Thinking::discover(&url, key.as_deref(), &model).await
            }
            None => None,
        };
        let (frontend, inbox) = web::Web::new(sessions::Store::new(sessions::DIR), thinking)?;
        let session = new_session(frontend.clone());
        web::run(frontend, inbox, session, addr).await?;
    } else {
        tui::run(new_session(Arc::new(tui::Tui))).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_md_is_appended_to_the_preamble() {
        let dir = std::env::temp_dir().join(format!("agent_agents_md_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(preamble_with_agents_md(&dir), PREAMBLE); // no file

        std::fs::write(dir.join(AGENTS_MD), "  \n").unwrap();
        assert_eq!(preamble_with_agents_md(&dir), PREAMBLE); // blank file

        std::fs::write(dir.join(AGENTS_MD), "Run `cargo test`.\n").unwrap();
        let preamble = preamble_with_agents_md(&dir);
        assert!(preamble.starts_with(PREAMBLE), "{preamble}");
        assert!(preamble.ends_with("Run `cargo test`."), "{preamble}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
