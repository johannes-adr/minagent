//! The `execute` tool: runs shell commands, sandboxed where supported.

use crate::harness::{Approval, ToolPolicy};
use crate::tools::process::{self, exit_line, format_output};
use rig_agent::rig_tool;
use rig_core::tool::ToolExecutionError;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default and maximum `timeout_seconds` of `execute`.
const EXECUTE_DEFAULT_TIMEOUT_SECS: u64 = 120;
const EXECUTE_MAX_TIMEOUT_SECS: u64 = 600;

/// Sandboxed commands run without asking; everything else needs approval.
impl ToolPolicy for Execute {
    const KIND: &'static str = "execute";

    fn approval(args: &ExecuteParameters, sandboxed: bool) -> Approval {
        if sandboxed && args.disable_sandbox != Some(true) {
            Approval::auto(format!("[sandboxed] {}", args.command))
        } else {
            Approval::ask(format!("execute (not sandboxed): {}", args.command))
        }
    }
}

/// Run a shell command with `sh -c` and return its exit code, stdout and stderr.
///
/// - Every call starts a fresh shell in the workspace root: `cd`, variables and other shell
///   state do not carry over to the next call. Chain dependent steps with `&&` in one command
///   (e.g. `cd web && npm test`) or use paths relative to the workspace root.
/// - There is no stdin and no terminal: interactive programs (editors, pagers, prompts) wait
///   until the timeout. Use non-interactive flags such as `-y`, `--yes`, `git --no-pager`.
/// - The command is killed after `timeout_seconds` (default 120, max 600) and the output so
///   far is returned. Raise it for slow builds, installs or test suites.
/// - stdout and stderr are each cut to their first 10000 and last 20000 characters. Narrow
///   large outputs yourself (`grep -n`, `head`, `tail`, `sed -n '100,200p' file`).
/// - Start servers and watchers in the background with their output redirected, e.g.
///   `nohup npm run dev > dev.log 2>&1 &`, and read the log in a later call.
/// - Also use it to read, search and inspect files (`cat -n`, `grep -rn`, `find`, `ls`, `git`).
///   For multi-line file edits, `execute_javascript` with `node:fs` is more reliable than `sed`.
#[rig_tool(params(
    command = "The shell command to run (POSIX sh). Chain steps that depend on each other with `&&`.",
    timeout_seconds = "Kill the command after this many seconds (default 120, max 600). Raise it for slow builds, installs or test suites.",
    disable_sandbox = "Set to true only when a command failed with \"Operation not permitted\" or needs network access (e.g. installs, git push); the user must then approve it."
))]
pub(crate) async fn execute(
    command: String,
    timeout_seconds: Option<u64>,
    disable_sandbox: Option<bool>,
) -> Result<String, ToolExecutionError> {
    let sandboxed = disable_sandbox != Some(true);
    let cmd = process::command("sh", ["-c", &command], sandboxed, false)
        .map_err(ToolExecutionError::from_error)?;
    let secs = timeout_seconds
        .unwrap_or(EXECUTE_DEFAULT_TIMEOUT_SECS)
        .clamp(1, EXECUTE_MAX_TIMEOUT_SECS);
    run_command(cmd, Duration::from_secs(secs))
        .await
        .map_err(ToolExecutionError::from_error)
}

/// Reads `pipe` into a shared buffer, so the output read so far survives a timeout.
fn collect(
    pipe: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
) -> (Arc<Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = buf.clone();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 8192];
        while let Ok(n @ 1..) = pipe.read(&mut chunk).await {
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    (buf, task)
}

/// Runs `cmd` without stdin and returns its formatted output. After `timeout` the whole
/// process group is killed and the output so far is returned.
async fn run_command(
    mut cmd: tokio::process::Command,
    timeout: Duration,
) -> std::io::Result<String> {
    cmd.kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so a timeout also kills the command's child processes.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    let (stdout, stdout_task) = collect(child.stdout.take());
    let (stderr, stderr_task) = collect(child.stderr.take());
    let exit = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => exit_line(&status?),
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .status()
                    .await;
            }
            let _ = child.kill().await;
            format!(
                "timed out after {}s; the process was killed (raise timeout_seconds if it needs more time). Output so far:",
                timeout.as_secs()
            )
        }
    };
    // Background processes may keep the pipes open; don't wait for them for long.
    let aborts = [stdout_task.abort_handle(), stderr_task.abort_handle()];
    let readers = futures::future::join(stdout_task, stderr_task);
    let _ = tokio::time::timeout(Duration::from_millis(500), readers).await;
    aborts.iter().for_each(|a| a.abort());
    let stdout = std::mem::take(&mut *stdout.lock().unwrap());
    let stderr = std::mem::take(&mut *stderr.lock().unwrap());
    Ok(format_output(&exit, &stdout, &stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sh(command: &str, timeout: Duration) -> String {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", command]);
        run_command(cmd, timeout).await.unwrap()
    }

    #[tokio::test]
    async fn execute_reports_exit_code_and_streams() {
        let out = sh("echo out; echo err >&2; exit 3", Duration::from_secs(10)).await;
        assert_eq!(out, "exit code: 3\nstdout:\nout\nstderr:\nerr");
        let out = sh("true", Duration::from_secs(10)).await;
        assert_eq!(out, "exit code: 0\n(no output)");
    }

    /// On timeout the whole process group dies and the output so far is kept.
    #[tokio::test]
    async fn execute_timeout_keeps_partial_output() {
        let started = std::time::Instant::now();
        let out = sh(
            "echo started; sleep 30 & sleep 30",
            Duration::from_millis(500),
        )
        .await;
        assert!(out.starts_with("timed out after 0s"), "{out}");
        assert!(out.contains("started"), "{out}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// A background process that keeps the pipes open must not block the call.
    #[tokio::test]
    async fn execute_does_not_wait_for_background_processes() {
        let started = std::time::Instant::now();
        let out = sh("sleep 5 & echo done", Duration::from_secs(10)).await;
        assert!(
            out.starts_with("exit code: 0") && out.contains("done"),
            "{out}"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    fn approval(args: &str, sandboxed: bool) -> Approval {
        let args = rig_core::serde_json::from_str(args).unwrap();
        Execute::approval(&args, sandboxed)
    }

    #[test]
    fn only_sandboxed_commands_run_without_asking() {
        assert_eq!(
            approval(r#"{"command":"ls"}"#, true),
            Approval::auto("[sandboxed] ls")
        );
        assert!(approval(r#"{"command":"ls"}"#, false).ask);
        assert!(approval(r#"{"command":"ls","disable_sandbox":true}"#, true).ask);
    }
}
