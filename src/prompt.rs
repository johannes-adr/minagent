//! The system prompt is produced by JavaScript.
//!
//! [`DIR`] (`system_prompt/`, relative to the working directory) holds `system.js`. It is
//! run like any script (`agent --js system_prompt/system.js <agent-exe>`, the executable
//! path as the model should write it is `ARGS[0]`) and prints the system prompt. What goes
//! into it is up to the script: the default one prints a base text followed by every other
//! `.js` / `.md` file in the directory. The default files are installed once, when the
//! directory doesn't exist, and never touched again, so they can be edited or deleted.
//! The script runs in the same sandbox as the `execute_javascript` tool (no network).

use crate::js;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// System prompt directory, relative to the working directory.
pub const DIR: &str = "system_prompt";

/// Maximum run time of the prompt script.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Files of the default prompt: (path below [`DIR`], contents).
const BUILTIN: &[(&str, &str)] = &[
    ("system.js", include_str!("../builtin_system_prompt/system.js")),
    ("skills.js", include_str!("../builtin_system_prompt/skills.js")),
];

/// Writes the default prompt files when `root` doesn't exist yet. An existing directory
/// is left alone, so files the user edited or deleted stay that way.
fn install_builtin(root: &Path) -> io::Result<()> {
    if root.exists() {
        return Ok(());
    }
    fs::create_dir_all(root)?;
    for (path, content) in BUILTIN {
        fs::write(root.join(path), content)?;
    }
    Ok(())
}

/// Installs the default prompt files and runs `system.js` to build the system prompt.
/// `exe` runs the script; `agent_exe` is the executable path as the model should write it.
pub async fn build(exe: &Path, agent_exe: &Path) -> Result<String, String> {
    install_builtin(Path::new(DIR)).map_err(|e| format!("cannot install {DIR}: {e}"))?;
    let script = format!("{DIR}/system.js");
    let agent_exe = agent_exe.display().to_string();
    let mut cmd = js::command(exe, &[&script, &agent_exe], false)
        .map_err(|e| format!("cannot run {script}: {e}"))?;
    let child = cmd
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {script}: {e}"))?;
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("{script} timed out after {}s", TIMEOUT.as_secs()))?
        .map_err(|e| format!("cannot run {script}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{script} failed ({}):\n{}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    // Warnings of the script (e.g. invalid skills) go to the terminal.
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
