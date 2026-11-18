//! The `execute_javascript` tool: runs JavaScript in a child process, sandboxed where supported.
//! The child is this executable in `--js` mode, which runs the code in [`runtime`].

pub mod runtime;

use crate::harness::{Approval, ToolPolicy};
use crate::tools::process::{self, exit_line, format_output};
use rig_agent::rig_tool;
use rig_core::tool::ToolExecutionError;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Maximum run time of a single `execute_javascript` call.
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

/// Sandboxed code without internet access runs without asking; the rest needs approval.
impl ToolPolicy for ExecuteJavascript {
    const KIND: &'static str = "execute";

    fn approval(args: &ExecuteJavascriptParameters, sandboxed: bool) -> Approval {
        let network = args.allow_network == Some(true);
        if sandboxed && !network {
            let mut lines = args.code.lines();
            let first = lines.next().unwrap_or_default();
            let more = if lines.next().is_some() { " ..." } else { "" };
            return Approval::auto(format!("[sandboxed] execute_javascript: {first}{more}"));
        }
        let note = match (network, sandboxed) {
            (true, true) => "with internet access",
            (true, false) => "with internet access, not sandboxed",
            (false, _) => "not sandboxed",
        };
        Approval::ask(format!("execute_javascript ({note}):\n{}", args.code))
    }
}

fn timed_out(timeout: Duration) -> ToolExecutionError {
    ToolExecutionError::from_error(std::io::Error::other(format!(
        "timed out after {}s; the process was killed",
        timeout.as_secs()
    )))
}

/// Run JavaScript (ES module) with fetch, node:fs etc. and return its exit code, stdout and stderr.
/// Sandboxed: writes only in the working directory and temp, network only to localhost
/// unless `allow_network` is set.
#[rig_tool(params(
    code = "JavaScript ES module to run (top-level await, fetch, node:fs, node:path, timers, URL, crypto, ...). Print results with console.log.",
    allow_network = "Set to true only when the script needs internet access; the user must then approve it."
))]
pub(crate) async fn execute_javascript(
    code: String,
    allow_network: Option<bool>,
) -> Result<String, ToolExecutionError> {
    let exe = std::env::current_exe().map_err(ToolExecutionError::from_error)?;
    run_js_code(&exe, &code, allow_network == Some(true), TOOL_TIMEOUT).await
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
    let mut cmd = process::command(exe, ["--js", "-"], true, allow_network)
        .map_err(ToolExecutionError::from_error)?;
    // Dropping the child (on timeout) kills it.
    let mut child = cmd
        .kill_on_drop(true)
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
    Ok(format_output(
        &exit_line(&out.status),
        &out.stdout,
        &out.stderr,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox;
    use std::path::PathBuf;

    fn approval(args: &str, sandboxed: bool) -> Approval {
        let args = rig_core::serde_json::from_str(args).unwrap();
        ExecuteJavascript::approval(&args, sandboxed)
    }

    #[test]
    fn network_and_unsandboxed_code_needs_approval() {
        assert!(!approval(r#"{"code":"1"}"#, true).ask);
        assert!(!approval(r#"{"code":"1","allow_network":false}"#, true).ask);
        assert!(approval(r#"{"code":"1"}"#, false).ask);
        assert!(approval(r#"{"code":"1","allow_network":true}"#, true).ask);
        assert_eq!(
            approval(r#"{"code":"a\nb"}"#, true).title,
            "[sandboxed] execute_javascript: a ..."
        );
    }

    #[test]
    fn approval_shows_code_unescaped() {
        let args = r#"{"code":"console.log(\"hi\");\nawait fetch('x')","allow_network":true}"#;
        let approval = approval(args, true);
        assert_eq!(
            approval.title,
            "execute_javascript (with internet access):\nconsole.log(\"hi\");\nawait fetch('x')"
        );
    }

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
        let error = result
            .expect_err("an endless loop must time out")
            .to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn js_tool_memory_is_limited() {
        let code = "const a = []; while (true) a.push(new Array(1e6).fill(1));";
        let out = js(code, false).await;
        assert!(!out.contains("exit code: 0"), "{out}");
        assert!(out.to_lowercase().contains("memory"), "{out}");
    }

    #[tokio::test]
    async fn js_tool_returns_console_output() {
        let out = js("console.log('hello ' + (1 + 1));", false).await;
        assert!(out.contains("exit code: 0"), "{out}");
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
}
