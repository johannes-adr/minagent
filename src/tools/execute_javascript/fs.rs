//! `fs` of `execute_javascript`: file access confined to the working directory, a
//! subdirectory of the harness's directory (see [`crate::tools::WORKDIR`]). Paths written
//! `skill:<name>/<path>` are read from the skills instead and are read-only.

use super::modules::{self, PREFIX};
use std::io;
use std::path::{Component, Path, PathBuf};

const OUTSIDE: &str = "path must be relative to the working directory and stay inside it";

/// Resolves `path` against `root` and fails if the result would leave `root` (absolute
/// path, `..`, symlink) or is part of a `.env*` file name. Symlinks are resolved, so a
/// link inside `root` that points outside is rejected too.
pub(super) fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let relative = Path::new(path);
    if !relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!("{path:?}: {OUTSIDE}"));
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    // Resolve symlinks of the longest existing prefix; the rest does not exist yet.
    let mut existing = root.join(relative);
    let mut missing = Vec::new();
    let mut resolved = loop {
        match existing.canonicalize() {
            Ok(real) => break real,
            Err(_) if existing.symlink_metadata().is_ok() => {
                return Err(format!("{path:?}: broken symlink"));
            }
            Err(_) => {
                missing.push(existing.file_name().ok_or(OUTSIDE)?.to_owned());
                existing.pop();
            }
        }
    };
    resolved.extend(missing.iter().rev());
    let inside = resolved
        .strip_prefix(&root)
        .map_err(|_| format!("{path:?}: {OUTSIDE}"))?;
    if inside.components().any(|c| {
        c.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .starts_with(".env")
    }) {
        return Err(format!("{path:?}: files named .env* are not accessible"));
    }
    Ok(resolved)
}

/// Resolves `path` inside [`crate::tools::WORKDIR`], which is created if missing, or, for
/// `skill:<name>/<path>`, inside the skills root (never created).
fn resolve_here(path: &str) -> Result<PathBuf, String> {
    if path.starts_with(PREFIX) {
        return resolve(&modules::skills_root(), modules::below_root(path)?);
    }
    let root = crate::tools::workdir();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    resolve(&root, path)
}

fn describe(path: &str, error: io::Error) -> String {
    format!("{path:?}: {error}")
}

pub(super) fn read_text(path: &str) -> Result<String, String> {
    std::fs::read_to_string(resolve_here(path)?).map_err(|e| describe(path, e))
}

/// The bytes of file `path` (for `fetch` uploads); fails if it is larger than `limit`.
pub(super) fn read_bytes(path: &str, limit: u64) -> Result<Vec<u8>, String> {
    let file = resolve_here(path)?;
    let size = std::fs::metadata(&file)
        .map_err(|e| describe(path, e))?
        .len();
    if size > limit {
        return Err(format!("{path:?} is larger than {} MB", limit >> 20));
    }
    std::fs::read(file).map_err(|e| describe(path, e))
}

/// Resolves `path` for writing: inside the working directory, never a skill.
pub(super) fn writable(path: &str) -> Result<PathBuf, String> {
    if path.starts_with(PREFIX) {
        return Err(format!("{path:?}: skills are read-only"));
    }
    resolve_here(path)
}

/// Writes `bytes` to `file` (resolved with [`writable`] from `path`), creating missing
/// parent directories.
pub(super) fn write_resolved(file: &Path, path: &str, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| describe(path, e))?;
    }
    std::fs::write(file, bytes).map_err(|e| describe(path, e))
}

/// Writes `text` to `path`, creating missing parent directories.
pub(super) fn write_text(path: &str, text: &str) -> Result<(), String> {
    write_resolved(&writable(path)?, path, text.as_bytes())
}

/// The sorted entry names of directory `path`; directories end with `/`.
pub(super) fn list(path: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(resolve_here(path)?).map_err(|e| describe(path, e))? {
        let entry = entry.map_err(|e| describe(path, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.to_lowercase().starts_with(".env") {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    names.sort();
    Ok(names)
}

pub(super) fn exists(path: &str) -> Result<bool, String> {
    Ok(resolve_here(path)?.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent_js_fs_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "a").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=1").unwrap();
        dir
    }

    #[test]
    fn paths_inside_the_root_resolve() {
        let dir = workspace("inside");
        let root = dir.canonicalize().unwrap();
        for path in [
            "sub/a.txt",
            "./sub/a.txt",
            "new/dir/b.txt",
            "environment.txt",
        ] {
            assert!(resolve(&dir, path).unwrap().starts_with(&root), "{path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn escapes_are_rejected() {
        let dir = workspace("escape");
        let outside = std::env::temp_dir().join(format!("agent_js_outside_{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("gone"), dir.join("dangling")).unwrap();
        for path in [
            "../x",
            "sub/../../x",
            "/etc/passwd",
            "link/x",
            "link",
            "dangling",
        ] {
            assert!(resolve(&dir, path).is_err(), "{path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn env_files_are_rejected() {
        let dir = workspace("env");
        std::os::unix::fs::symlink(dir.join(".env"), dir.join("innocent")).unwrap();
        for path in [
            ".env",
            ".env.local",
            "sub/.ENV",
            ".envrc",
            "innocent",
            ".env/x",
        ] {
            assert!(resolve(&dir, path).is_err(), "{path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
