//! macOS Seatbelt (`sandbox-exec`) sandbox, ported from bob's `sandbox.ts`.
//!
//! Reads are allowed everywhere except credential stores; writes are limited to the
//! workspace, temp and package caches. Git hooks/config stay read-only so a sandboxed
//! command cannot plant code that later runs outside the sandbox. (Bob also protects
//! its own `.bob` directory; this project has no such directory, so that rule is dropped.)

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Which network access a sandboxed process gets.
#[derive(Clone, Copy)]
pub enum Network {
    /// Loopback only (shell commands).
    Localhost,
    /// Loopback plus outbound internet, including DNS and TLS (the JS child, so `fetch` works).
    Internet,
}

const PROFILE_HEAD: &str = r#"(version 1)
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
"#;

const NETWORK_LOCALHOST: &str = r#"(allow network-bind network-inbound (local ip "localhost:*"))
(allow network-outbound (remote ip "localhost:*"))
"#;

const NETWORK_INTERNET: &str = r#"(allow network-bind network-inbound (local ip "localhost:*"))
(allow network-outbound)
"#;

const PROFILE_TAIL: &str = r#"(allow file-read*)
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

fn profile(network: Network) -> String {
    let net = match network {
        Network::Localhost => NETWORK_LOCALHOST,
        Network::Internet => NETWORK_INTERNET,
    };
    format!("{PROFILE_HEAD}{net}{PROFILE_TAIL}")
}

pub fn supported() -> bool {
    cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).exists()
}

/// Wraps `program args...` in `sandbox-exec`, scoped to the current directory as workspace.
pub fn command<S: AsRef<OsStr>>(
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = S>,
    network: Network,
) -> io::Result<Command> {
    command_in(&std::env::current_dir()?, program, args, network)
}

fn command_in<S: AsRef<OsStr>>(
    workspace: &Path,
    program: impl AsRef<OsStr>,
    args: impl IntoIterator<Item = S>,
    network: Network,
) -> io::Result<Command> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is not set"))?;
    let param = |key: &str, path: &Path| -> io::Result<String> {
        Ok(format!("{key}={}", std::fs::canonicalize(path)?.display()))
    };
    let mut cmd = Command::new(SANDBOX_EXEC);
    cmd.arg("-p")
        .arg(profile(network))
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
        let out = command_in(&ws.0, "sh", ["-c", script], Network::Localhost)
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
}
