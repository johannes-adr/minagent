//! The web UI's file browser: a small file system API on two areas, the skills (`skills/`)
//! and the agent's working directory (`work/`). Nothing outside them is reachable: paths
//! are relative, may not contain `..` and may not go through symlinks.
//!
//! The files of built-in skills are rewritten on every start, so they are read-only here.

use crate::tools::{self, skills};
use rig_core::serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// Files larger than this are not opened in the editor.
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Debug, PartialEq)]
pub enum FileError {
    /// The request itself is wrong: a bad path, a name that is taken, a directory where a
    /// file is needed.
    Invalid(String),
    NotFound,
    /// Built-in skill files can't be changed.
    Protected,
    /// Not UTF-8 text, or too large for the editor.
    NotText(String),
    Io(String),
}

impl FileError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound => 404,
            Self::Protected => 403,
            Self::NotText(_) => 415,
            Self::Io(_) => 500,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Invalid(m) | Self::NotText(m) | Self::Io(m) => m.clone(),
            Self::NotFound => "not found".into(),
            Self::Protected => "built-in skill files are replaced on every start; copy the skill under another name to change it".into(),
        }
    }
}

impl From<io::Error> for FileError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Io(e.to_string()),
        }
    }
}

type Result<T> = std::result::Result<T, FileError>;

/// A directory the browser may work in.
pub struct Area {
    dir: PathBuf,
    /// Files (relative to `dir`, `/`-separated) that can't be changed.
    protected: Vec<String>,
}

impl Area {
    /// `skills` or `work`, relative to the harness's directory.
    pub fn named(name: &str) -> Option<Self> {
        let cwd = std::env::current_dir().unwrap_or_default();
        match name {
            "skills" => Some(Self {
                dir: cwd.join(skills::DIR),
                protected: skills::builtin_paths().map(str::to_owned).collect(),
            }),
            "work" => Some(Self {
                dir: cwd.join(tools::WORKDIR),
                protected: Vec::new(),
            }),
            _ => None,
        }
    }

    /// An area on `dir` with some protected files, for tests.
    #[cfg(test)]
    fn on(dir: PathBuf, protected: &[&str]) -> Self {
        Self {
            dir,
            protected: protected.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    /// The clean `/`-separated form of `rel` and its absolute path. Fails on `..`, on
    /// symlinks and on anything that would leave the area.
    fn resolve(&self, rel: &str) -> Result<(String, PathBuf)> {
        let mut segments = Vec::new();
        for segment in rel.split('/') {
            match segment {
                "" | "." => {}
                ".." => return Err(FileError::Invalid("`..` is not allowed in paths".into())),
                s if s.contains(['\\', '\0']) => {
                    return Err(FileError::Invalid(format!("bad name {s:?}")));
                }
                s => segments.push(s),
            }
        }
        fs::create_dir_all(&self.dir)?;
        let root = self.dir.canonicalize()?;
        let mut path = root.clone();
        path.extend(&segments);
        // The deepest part that exists must really be inside the area (it may be a symlink).
        let mut probe = path.as_path();
        while probe.canonicalize().is_err() {
            probe = probe.parent().ok_or(FileError::NotFound)?;
        }
        if !probe.canonicalize()?.starts_with(&root) {
            return Err(FileError::Invalid("the path leaves the directory".into()));
        }
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(FileError::Invalid("symbolic links are not supported".into()));
        }
        Ok((segments.join("/"), path))
    }

    /// Whether changing `rel` (a file, or a directory with everything in it) would touch a
    /// protected file.
    fn touches_protected(&self, rel: &str) -> bool {
        self.protected
            .iter()
            .any(|p| p == rel || (!rel.is_empty() && p.starts_with(&format!("{rel}/"))))
    }

    pub fn list(&self, rel: &str) -> Result<Vec<Value>> {
        let (rel, path) = self.resolve(rel)?;
        if !path.is_dir() {
            return Err(FileError::NotFound);
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(&path)?.flatten() {
            let meta = entry.metadata()?;
            if entry.file_type()?.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let full = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            entries.push(json!({
                "name": name,
                "dir": meta.is_dir(),
                "size": meta.len(),
                "modified": modified,
                "readonly": !meta.is_dir() && self.touches_protected(&full),
            }));
        }
        entries.sort_by(|a, b| {
            (b["dir"].as_bool(), a["name"].as_str()).cmp(&(a["dir"].as_bool(), b["name"].as_str()))
        });
        Ok(entries)
    }

    pub fn read(&self, rel: &str) -> Result<(String, bool)> {
        let (rel, path) = self.resolve(rel)?;
        let meta = fs::metadata(&path)?;
        if !meta.is_file() {
            return Err(FileError::Invalid("not a file".into()));
        }
        if meta.len() > MAX_FILE {
            return Err(FileError::NotText(format!(
                "{} KiB is too large to edit here (limit {} KiB)",
                meta.len() / 1024,
                MAX_FILE / 1024
            )));
        }
        let text = String::from_utf8(fs::read(&path)?)
            .map_err(|_| FileError::NotText("not a text file".into()))?;
        Ok((text, self.touches_protected(&rel)))
    }

    pub fn write(&self, rel: &str, content: &str) -> Result<()> {
        let (rel, path) = self.resolve(rel)?;
        if rel.is_empty() || path.is_dir() {
            return Err(FileError::Invalid("not a file".into()));
        }
        if self.touches_protected(&rel) {
            return Err(FileError::Protected);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Via a temporary file, so that a failure leaves the old content intact.
        let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
        fs::write(&tmp, content)?;
        Ok(fs::rename(tmp, path)?)
    }

    pub fn mkdir(&self, rel: &str) -> Result<()> {
        let (rel, path) = self.resolve(rel)?;
        if rel.is_empty() || path.exists() {
            return Err(FileError::Invalid("already exists".into()));
        }
        Ok(fs::create_dir_all(path)?)
    }

    pub fn remove(&self, rel: &str) -> Result<()> {
        let (rel, path) = self.resolve(rel)?;
        if rel.is_empty() {
            return Err(FileError::Invalid("the directory itself can't be deleted".into()));
        }
        if self.touches_protected(&rel) {
            return Err(FileError::Protected);
        }
        if path.is_dir() {
            Ok(fs::remove_dir_all(path)?)
        } else {
            Ok(fs::remove_file(path)?)
        }
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from_rel, from_path) = self.resolve(from)?;
        let (to_rel, to_path) = self.resolve(to)?;
        if from_rel.is_empty() || to_rel.is_empty() {
            return Err(FileError::Invalid("the directory itself can't be moved".into()));
        }
        if self.touches_protected(&from_rel) || self.touches_protected(&to_rel) {
            return Err(FileError::Protected);
        }
        if !from_path.exists() {
            return Err(FileError::NotFound);
        }
        if to_path.exists() {
            return Err(FileError::Invalid("the target already exists".into()));
        }
        if to_rel.starts_with(&format!("{from_rel}/")) {
            return Err(FileError::Invalid("can't move a directory into itself".into()));
        }
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(fs::rename(from_path, to_path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(name: &str, protected: &[&str]) -> Area {
        let dir = std::env::temp_dir().join(format!("agent_files_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Area::on(dir, protected)
    }

    #[test]
    fn files_are_written_listed_read_renamed_and_deleted() {
        let area = area("crud", &[]);
        area.write("a/b/note.md", "hello").unwrap(); // parents are created
        area.mkdir("empty").unwrap();
        let names: Vec<String> = area.list("").unwrap().iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["a", "empty"]); // directories first, sorted
        let inner = area.list("a/b").unwrap();
        assert_eq!((inner[0]["name"].as_str(), inner[0]["size"].as_u64()), (Some("note.md"), Some(5)));
        assert_eq!(area.read("a/b/note.md").unwrap(), ("hello".to_owned(), false));
        area.write("a/b/note.md", "changed").unwrap();
        area.rename("a/b/note.md", "moved/n.md").unwrap();
        assert_eq!(area.read("moved/n.md").unwrap().0, "changed");
        assert_eq!(area.read("a/b/note.md"), Err(FileError::NotFound));
        area.remove("a").unwrap();
        area.remove("moved/n.md").unwrap();
        assert!(area.list("a").is_err());
        let _ = fs::remove_dir_all(&area.dir);
    }

    #[test]
    fn paths_cannot_leave_the_area() {
        let area = area("escape", &[]);
        area.write("ok.txt", "x").unwrap();
        for bad in ["../x", "a/../../x", "/etc/passwd/..", "a\\b", "x\0y"] {
            assert!(matches!(area.write(bad, "x"), Err(FileError::Invalid(_))), "{bad:?}");
            assert!(area.read(bad).is_err(), "{bad:?}");
        }
        // An absolute path is taken as relative to the area, not to the file system.
        area.write("/abs.txt", "x").unwrap();
        assert!(area.dir.join("abs.txt").exists());
        assert!(area.remove("").is_err() && area.remove("/").is_err());
        assert!(area.rename("ok.txt", "../out.txt").is_err());
        assert!(area.rename("", "x").is_err());
        let _ = fs::remove_dir_all(&area.dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let area = area("symlink", &[]);
        let outside = std::env::temp_dir().join(format!("agent_files_outside_{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        area.write("seed.txt", "x").unwrap();
        std::os::unix::fs::symlink(&outside, area.dir.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("nothing"), area.dir.join("dangling")).unwrap();
        assert!(area.read("link/secret.txt").is_err());
        assert!(area.write("link/new.txt", "x").is_err());
        assert!(area.write("dangling", "x").is_err());
        assert!(!outside.join("new.txt").exists() && !outside.join("nothing").exists());
        // Symlinks don't even show up in listings.
        let names: Vec<_> = area.list("").unwrap().iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["seed.txt"]);
        let _ = fs::remove_dir_all(&area.dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn protected_files_stay_as_they_are() {
        let area = area("protected", &["builtin/SKILL.md"]);
        fs::create_dir_all(area.dir.join("builtin")).unwrap();
        fs::write(area.dir.join("builtin/SKILL.md"), "original").unwrap();
        assert_eq!(area.write("builtin/SKILL.md", "x"), Err(FileError::Protected));
        assert_eq!(area.remove("builtin/SKILL.md"), Err(FileError::Protected));
        assert_eq!(area.remove("builtin"), Err(FileError::Protected)); // it contains one
        assert_eq!(area.rename("builtin", "other"), Err(FileError::Protected));
        assert_eq!(area.rename("builtin/SKILL.md", "other.md"), Err(FileError::Protected));
        assert_eq!(area.read("builtin/SKILL.md"), Ok(("original".to_owned(), true)));
        // Other files next to it can be added and changed.
        area.write("builtin/extra.md", "mine").unwrap();
        area.remove("builtin/extra.md").unwrap();
        let listed = area.list("builtin").unwrap();
        assert_eq!(listed[0]["readonly"], true);
        let _ = fs::remove_dir_all(&area.dir);
    }

    #[test]
    fn binary_and_large_files_are_not_opened() {
        let area = area("binary", &[]);
        fs::create_dir_all(&area.dir).unwrap();
        fs::write(area.dir.join("a.bin"), [0xff, 0xfe, 0x00]).unwrap();
        fs::write(area.dir.join("big.txt"), vec![b'a'; MAX_FILE as usize + 1]).unwrap();
        assert!(matches!(area.read("a.bin"), Err(FileError::NotText(_))));
        assert!(matches!(area.read("big.txt"), Err(FileError::NotText(_))));
        assert!(matches!(area.read("."), Err(FileError::Invalid(_)))); // a directory
        let _ = fs::remove_dir_all(&area.dir);
    }

    #[test]
    fn rename_refuses_overwrites_and_loops() {
        let area = area("rename", &[]);
        area.write("a/x.txt", "1").unwrap();
        area.write("b.txt", "2").unwrap();
        assert!(area.rename("b.txt", "a/x.txt").is_err());
        assert!(area.rename("a", "a/inner").is_err());
        assert_eq!(area.read("a/x.txt").unwrap().0, "1");
        let _ = fs::remove_dir_all(&area.dir);
    }
}
