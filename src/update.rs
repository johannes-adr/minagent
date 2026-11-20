//! Self-update from the GitHub releases (`.github/workflows/release.yml`): finds the latest
//! version, downloads the binary for this platform, checks its SHA-256 sum, replaces the
//! running executable and starts it again with the same arguments, environment and
//! working directory.
//!
//! The sum comes from the same release as the file, so it protects against a broken download,
//! not against someone who controls the release.

use rig_core::serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// The version this binary was built as (the release workflow writes the tag into `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// What `Cargo.toml` says in the repository: a build from source, not a release.
const DEV_VERSION: &str = "0.1.0";
const REPO: &str = "johannes-adr/minagent";
/// A release archive is a few megabytes; this only stops an endless download.
const MAX_DOWNLOAD: usize = 200 * 1024 * 1024;
/// How long a successful check is reused (GitHub allows 60 anonymous requests per hour).
const FRESH: Duration = Duration::from_secs(3600);
const FRESH_ERROR: Duration = Duration::from_secs(300);

/// `(major, minor, patch)` of `1.2.3` or `v1.2.3`; anything else is `None`.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || {
        let part = parts.next()?;
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// Whether `latest` is a higher version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// Whether this is a build from source, which never looks for updates.
pub fn is_dev_build() -> bool {
    VERSION == DEV_VERSION
}

/// The platform part of the release asset's name, if the workflow builds one for this system.
fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

/// Why this process cannot replace itself, or `None` when it can.
pub fn blocker(isolated: bool) -> Option<&'static str> {
    if isolated {
        Some("this agent runs in a container: pull the new image instead")
    } else if !cfg!(unix) || platform().is_none() {
        Some("there is no release binary for this system")
    } else {
        None
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("agent/{VERSION}"))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())
}

/// The newest release's version, e.g. `1.2.0`.
async fn fetch_latest() -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let release: Value = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("could not look for a new version: {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not read the release: {e}"))?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let (major, minor, patch) =
        parse_version(tag).ok_or_else(|| format!("the latest release has an odd tag: {tag:?}"))?;
    Ok(format!("{major}.{minor}.{patch}"))
}

/// The latest version, asked for at most once per hour.
#[derive(Default)]
pub struct Latest {
    cached: Mutex<Option<(Instant, Result<String, String>)>>,
}

impl Latest {
    pub async fn get(&self) -> Result<String, String> {
        // Held while asking, so that several pages share one request.
        let mut cached = self.cached.lock().await;
        if let Some((at, result)) = cached.as_ref() {
            let fresh = if result.is_ok() { FRESH } else { FRESH_ERROR };
            if at.elapsed() < fresh {
                return result.clone();
            }
        }
        let result = fetch_latest().await;
        *cached = Some((Instant::now(), result.clone()));
        result
    }
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download of {url} failed: {e}"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("download failed: {e}"))? {
        if bytes.len() + chunk.len() > MAX_DOWNLOAD {
            return Err(format!("{url} is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `sum_file` (the output of `shasum -a 256`: the sum, then the name) holds the
/// SHA-256 sum of `data`.
fn checksum_matches(data: &[u8], sum_file: &str) -> bool {
    let expected = sum_file.split_whitespace().next().unwrap_or_default();
    expected.len() == 64 && expected.eq_ignore_ascii_case(&hex(&Sha256::digest(data)))
}

/// Unpacks `archive` in `work`, checks that the binary in it reports `version`, and moves it
/// over `exe` (same directory, so the move is atomic and a running process is not disturbed).
fn replace(archive: &[u8], exe: &Path, work: &Path, version: &str) -> Result<(), String> {
    let io = |what: &'static str| move |e: std::io::Error| format!("{what}: {e}");
    std::fs::create_dir(work).map_err(io("could not create a directory next to the program"))?;
    let tar = work.join("archive.tar.gz");
    std::fs::write(&tar, archive).map_err(io("could not write the download"))?;
    // Only the one member is extracted, whatever else the archive holds.
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tar)
        .arg("-C")
        .arg(work)
        .arg("agent")
        .status()
        .map_err(io("could not run tar"))?;
    if !status.success() {
        return Err("could not unpack the download".into());
    }
    let new = work.join("agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .map_err(io("could not make the download executable"))?;
    }
    let output = std::process::Command::new(&new)
        .arg("--version")
        .output()
        .map_err(io("the downloaded program does not start"))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    if reported.trim() != format!("agent {version}") {
        return Err(format!("the downloaded program says {:?}, not {version}", reported.trim()));
    }
    std::fs::rename(&new, exe)
        .map_err(|e| format!("could not replace {}: {e}", exe.display()))
}

/// Downloads release `version`, checks it and replaces the running executable. Returns the
/// executable's path, which [`restart`] needs (the running one may be reported as deleted).
pub async fn install(version: &str) -> Result<PathBuf, String> {
    let (major, minor, patch) = parse_version(version).ok_or("not a version")?;
    let platform = platform().ok_or("there is no release binary for this system")?;
    let tag = format!("v{major}.{minor}.{patch}");
    let name = format!("agent-{tag}-{platform}.tar.gz");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let client = client()?;
    let archive = download(&client, &format!("{base}/{name}")).await?;
    let sum = download(&client, &format!("{base}/{name}.sha256")).await?;
    if !checksum_matches(&archive, &String::from_utf8_lossy(&sum)) {
        return Err("the download does not match its checksum".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("the program has no directory")?;
    let work = dir.join(format!(".agent-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let version = format!("{major}.{minor}.{patch}");
    let result = {
        let (exe, work) = (exe.clone(), work.clone());
        tokio::task::spawn_blocking(move || replace(&archive, &exe, &work, &version))
            .await
            .map_err(|e| e.to_string())?
    };
    let _ = std::fs::remove_dir_all(&work);
    result.map(|()| exe)
}

/// Starts `exe` in place of this process with the same arguments; the environment and the
/// working directory stay. Only returns, with the reason, when that failed.
pub fn restart(exe: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .exec();
        format!("could not start the new version: {error}")
    }
    #[cfg(not(unix))]
    {
        let _ = exe;
        "restarting is not supported here".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_by_number() {
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(is_newer("v2.0.0", "1.99.99"));
        assert!(!is_newer("1.1.1", "1.1.1"));
        assert!(!is_newer("1.1.0", "1.1.1"));
        assert!(!is_newer("garbage", "1.1.1"));
        assert!(!is_newer("1.2", "1.1.1"));
        assert!(!is_newer("1.2.3-beta", "1.1.1"));
        assert!(!is_newer("1.2.3.4", "1.1.1"));
    }

    #[test]
    fn a_checksum_file_is_read_like_shasum_writes_it() {
        let data = b"hello";
        let sum = hex(&Sha256::digest(data));
        assert!(checksum_matches(data, &format!("{sum}  agent-v1.tar.gz\n")));
        assert!(checksum_matches(data, &sum.to_uppercase()));
        assert!(!checksum_matches(b"other", &format!("{sum}  agent-v1.tar.gz\n")));
        assert!(!checksum_matches(data, ""));
        assert!(!checksum_matches(data, "abc"));
    }

    #[test]
    fn a_container_never_replaces_itself() {
        assert!(blocker(true).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_download_replaces_the_program() {
        let dir = std::env::temp_dir().join(format!("agent_update_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pack = dir.join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        let script = |version: &str| format!("#!/bin/sh\necho \"agent {version}\"\n");
        std::fs::write(pack.join("agent"), script("9.9.9")).unwrap();
        let archive = dir.join("a.tar.gz");
        let made = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(&pack)
            .arg("agent")
            .status()
            .unwrap();
        assert!(made.success());
        let bytes = std::fs::read(&archive).unwrap();

        let exe = dir.join("agent");
        std::fs::write(&exe, script("1.0.0")).unwrap();
        // The wrong version is refused and leaves the program alone.
        let work = dir.join("work1");
        assert!(replace(&bytes, &exe, &work, "8.8.8").is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), script("1.0.0"));
        let work = dir.join("work2");
        replace(&bytes, &exe, &work, "9.9.9").unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), script("9.9.9"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
