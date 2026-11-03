//! macOS Seatbelt (`sandbox-exec`) sandbox, ported from bob's `sandbox.ts`.
//!
//! Reads are allowed everywhere except credential stores; writes are limited to the
//! workspace, temp and package caches. Git hooks/config stay read-only so a sandboxed
//! command cannot plant code that later runs outside the sandbox. (Bob also protects
//! its own `.bob` directory; this project has no such directory, so that rule is dropped.)

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Network is limited to localhost.
const PROFILE: &str = r#"(version 1)
(deny default)
(allow process-exec process-fork process-info* signal)
(allow sysctl-read)
(allow user-preference-read)
(allow iokit-get-properties)
(allow ipc-posix-shm ipc-posix-sem)
(allow pseudo-tty)
(allow file-ioctl)
(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.logd")
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.FontObjectsServer")
  (global-name "com.apple.fonts")
  (global-name "com.apple.lsd.mapdb"))
(allow network-bind network-inbound (local ip "localhost:*"))
(allow network-outbound (remote ip "localhost:*"))
(allow file-read*)
(deny file-read*
  (subpath (string-append (param "HOME") "/.ssh"))
  (subpath (string-append (param "HOME") "/.aws"))
  (subpath (string-append (param "HOME") "/.gnupg"))
  (subpath (string-append (param "HOME") "/.kube"))
  (subpath (string-append (param "HOME") "/.docker"))
  (subpath (string-append (param "HOME") "/.config/gh"))
  (subpath (string-append (param "HOME") "/.netrc")))
(allow file-write*
  (subpath (param "WORKSPACE"))
  (subpath (param "TMP"))
  (subpath "/private/tmp")
  (subpath "/dev")
  (subpath (string-append (param "HOME") "/Library/Caches"))
  (subpath (string-append (param "HOME") "/Library/pnpm"))
  (subpath (string-append (param "HOME") "/.cache"))
  (subpath (string-append (param "HOME") "/.local/state"))
  (subpath (string-append (param "HOME") "/.local/share/pnpm"))
  (subpath (string-append (param "HOME") "/.npm"))
  (subpath (string-append (param "HOME") "/.yarn"))
  (subpath (string-append (param "HOME") "/.bun/install/cache"))
  (subpath (string-append (param "HOME") "/.gradle/caches"))
  (subpath (string-append (param "HOME") "/.m2/repository"))
  (subpath (string-append (param "HOME") "/go/pkg/mod"))
  (subpath (string-append (param "HOME") "/.cargo/registry")))
(deny file-write*
  (subpath (string-append (param "WORKSPACE") "/.git/hooks"))
  (literal (string-append (param "WORKSPACE") "/.git/config")))
(deny file-write-unlink file-write-create
  (literal (string-append (param "WORKSPACE") "/.git")))
"#;

pub fn supported() -> bool {
    cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).exists()
}

/// Wraps `program args...` in `sandbox-exec`, scoped to the current directory as workspace.
/// With `allow_network`, outbound internet access is allowed on top of the localhost-only profile.
pub fn command<S: AsRef<OsStr>>(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = S>,
    allow_network: bool,
) -> io::Result<Command> {
    command_in(&std::env::current_dir()?, program, args, allow_network)
}

fn command_in<S: AsRef<OsStr>>(
    workspace: &Path,
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = S>,
    allow_network: bool,
) -> io::Result<Command> {
    // Later SBPL rules win, so this overrides the localhost-only `network-outbound` rule.
    let profile = if allow_network {
        Cow::Owned(format!("{PROFILE}(allow network-outbound)\n"))
    } else {
        Cow::Borrowed(PROFILE)
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is not set"))?;
    let param = |key: &str, path: &Path| -> io::Result<String> {
        Ok(format!("{key}={}", std::fs::canonicalize(path)?.display()))
    };
    let mut cmd = Command::new(SANDBOX_EXEC);
    cmd.arg("-p")
        .arg(&*profile)
        .args(["-D", &param("HOME", &home)?])
        .args(["-D", &param("WORKSPACE", workspace)?])
        .args(["-D", &param("TMP", &std::env::temp_dir())?])
        .arg(program)
        .args(args);
    Ok(cmd)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Temp workspace under $HOME (not under TMP, which is writable); removed on drop.
    struct Workspace(PathBuf);

    impl Workspace {
        fn new(name: &str) -> Self {
            let home = std::env::var("HOME").unwrap();
            let dir =
                PathBuf::from(home).join(format!(".agent_sandbox_{name}_{}", std::process::id()));
            std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
            Workspace(dir)
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs `sh -c script` sandboxed in `ws`; returns (success, stderr).
    async fn run(ws: &Workspace, script: &str) -> (bool, String) {
        run_net(ws, script, false).await
    }

    async fn run_net(ws: &Workspace, script: &str, allow_network: bool) -> (bool, String) {
        let out = command_in(&ws.0, "sh", ["-c", script], allow_network)
            .unwrap()
            .current_dir(&ws.0)
            .output()
            .await
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[tokio::test]
    async fn allows_writes_in_workspace() {
        if !supported() {
            return;
        }
        let ws = Workspace::new("inside");
        let (ok, err) = run(&ws, "echo hi > inside.txt").await;
        assert!(ok, "{err}");
        assert!(ws.0.join("inside.txt").exists());
    }

    #[tokio::test]
    async fn blocks_writes_outside_workspace() {
        if !supported() {
            return;
        }
        let ws = Workspace::new("outside");
        let target = PathBuf::from(std::env::var("HOME").unwrap())
            .join(format!("agent_sandbox_escape_{}.txt", std::process::id()));
        let (ok, err) = run(&ws, &format!("echo hi > {}", target.display())).await;
        let created = target.exists();
        let _ = std::fs::remove_file(&target);
        assert!(!ok);
        assert!(err.contains("Operation not permitted"), "{err}");
        assert!(!created);
    }

    #[tokio::test]
    async fn blocks_writes_to_git_hooks() {
        if !supported() {
            return;
        }
        let ws = Workspace::new("hooks");
        let (ok, err) = run(&ws, "echo hi > .git/hooks/pre-commit").await;
        assert!(!ok);
        assert!(err.contains("Operation not permitted"), "{err}");
        assert!(!ws.0.join(".git/hooks/pre-commit").exists());
    }

    #[tokio::test]
    async fn blocks_reading_credentials() {
        if !supported() {
            return;
        }
        let ssh = PathBuf::from(std::env::var("HOME").unwrap()).join(".ssh");
        if !ssh.exists() {
            return;
        }
        let ws = Workspace::new("creds");
        let (ok, err) = run(&ws, &format!("ls {}", ssh.display())).await;
        assert!(!ok);
        assert!(err.contains("Operation not permitted"), "{err}");
    }

    /// Needs internet, so it is not run by default: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires internet access"]
    async fn network_only_with_allow_network() {
        if !supported() {
            return;
        }
        let ws = Workspace::new("net");
        let curl = "curl -sS -m 10 -o /dev/null https://example.com";
        let (ok, _) = run_net(&ws, curl, false).await;
        assert!(!ok, "internet must be blocked by default");
        let (ok, err) = run_net(&ws, curl, true).await;
        assert!(ok, "{err}");
    }
}
