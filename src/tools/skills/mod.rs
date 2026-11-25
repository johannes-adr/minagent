//! Agent Skills (https://agentskills.io/specification): discovery, validation and the
//! system prompt section.
//!
//! Skills live in [`DIR`] (`skills/<name>/SKILL.md`, relative to the working directory).
//! Only name and description go into the system prompt (progressive disclosure); the
//! agent loads the full instructions on demand with the `use_skill` tool.
//! Built-in skills are embedded in the binary and (re)installed into [`DIR`] on startup.

pub mod use_skill;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Skills directory, relative to the working directory.
pub const DIR: &str = "skills";

/// Embeds `builtin_skills/<path>` as `(path, contents)`.
macro_rules! builtin {
    ($path:literal) => {
        (
            $path,
            include_str!(concat!("../../../builtin_skills/", $path)),
        )
    };
}

/// Files of the embedded skills: (path below [`DIR`], contents).
const BUILTIN: &[(&str, &str)] = &[
    builtin!("creating-skills/SKILL.md"),
    builtin!("paperless/SKILL.md"),
    builtin!("paperless/scripts/paperless.js"),
    builtin!("paperless/references/search.md"),
    builtin!("paperless/references/editing.md"),
    builtin!("paperless/references/consume.md"),
    builtin!("paperless/references/backfill.md"),
    builtin!("paperless/references/recipes.md"),
    builtin!("paperless/references/automation.md"),
    builtin!("paperless/references/rest-api.md"),
];

/// Paths (below [`DIR`]) of the files of built-in skills. They are rewritten on startup, so
/// edits to them would be lost.
pub fn builtin_paths() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(path, _)| *path)
}

/// Built-in skills of older versions; their directories are removed on startup.
const REMOVED_BUILTIN: &[&str] = &["using-skills"];

/// Which skills an agent may use: all, all but some, or only some. A skill that is not
/// allowed is not listed in the system prompt, `use_skill` doesn't know it and scripts can't
/// import or read it. (It is still a file: with `execute` the agent can read it anyway.)
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SkillFilter {
    #[default]
    All,
    /// All skills except these; skills added later are allowed.
    Disabled(Arc<BTreeSet<String>>),
    /// Only these; skills added later are not.
    Only(Arc<BTreeSet<String>>),
}

impl SkillFilter {
    pub fn disabled(names: impl IntoIterator<Item = String>) -> Self {
        Self::Disabled(Arc::new(names.into_iter().collect()))
    }

    pub fn only(names: impl IntoIterator<Item = String>) -> Self {
        Self::Only(Arc::new(names.into_iter().collect()))
    }

    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Disabled(names) => !names.contains(name),
            Self::Only(names) => names.contains(name),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Path of the skill's `SKILL.md`.
    pub location: PathBuf,
}

#[derive(Default)]
pub struct Skills {
    pub skills: Vec<Skill>,
    /// Skills that were skipped because they are invalid.
    pub warnings: Vec<String>,
}

impl Skills {
    /// Only the skills `filter` allows.
    pub fn filtered(mut self, filter: &SkillFilter) -> Self {
        self.skills.retain(|s| filter.allows(&s.name));
        self
    }

    /// Installs the built-in skills into `root`, then discovers all skills there.
    pub fn load(root: &Path) -> io::Result<Skills> {
        install_builtin(root)?;
        Ok(discover(root))
    }

    /// The system prompt section: how to load skills and the list of available skills.
    pub fn prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n\n# Skills\n\nSkills are task-specific instructions. If a task clearly matches a \
             skill's description, call `use_skill` with its name before doing anything else, then \
             follow the returned instructions. Load each skill at most once per conversation, and \
             only skills listed here. When the instructions name a document for a subtopic, load \
             it with `use_skill` and `file` once the task needs it.\n\n<available_skills>\n",
        );
        for skill in &self.skills {
            let _ = write!(
                out,
                "<skill>\n<name>{}</name>\n<description>{}</description>\n</skill>\n",
                escape(&skill.name),
                escape(&skill.description),
            );
        }
        out.push_str("</available_skills>\n");
        out
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Writes the embedded skills to `root` and removes retired built-in skills.
fn install_builtin(root: &Path) -> io::Result<()> {
    for name in REMOVED_BUILTIN {
        let stale = root.join(name);
        if stale.exists() {
            fs::remove_dir_all(stale)?;
        }
    }
    for (path, content) in BUILTIN {
        let target = root.join(path);
        if fs::read_to_string(&target).is_ok_and(|existing| existing == *content) {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, content)?;
    }
    Ok(())
}

/// Finds all valid skills in `root` (`<root>/<name>/SKILL.md`), sorted by name.
pub fn discover(root: &Path) -> Skills {
    let mut result = Skills::default();
    let Ok(entries) = fs::read_dir(root) else {
        return result;
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs {
        let location = dir.join("SKILL.md");
        let Ok(text) = fs::read_to_string(&location) else {
            continue;
        };
        let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        match parse(&text, dir_name) {
            Ok((name, description)) => result.skills.push(Skill {
                name,
                description,
                location,
            }),
            Err(e) => result
                .warnings
                .push(format!("ignoring {}: {e}", location.display())),
        }
    }
    result
}

/// Splits a `SKILL.md` into its frontmatter and the body after the closing `---` line.
fn split_frontmatter(text: &str) -> Result<(&str, &str), &'static str> {
    let rest = text
        .strip_prefix("---")
        .and_then(|r| r.strip_prefix("\r\n").or_else(|| r.strip_prefix('\n')))
        .ok_or("missing frontmatter")?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err("unterminated frontmatter")
}

/// The instructions of a `SKILL.md`: its body without the frontmatter, trimmed.
pub fn instructions(text: &str) -> &str {
    split_frontmatter(text)
        .map_or(text, |(_, body)| body)
        .trim()
}

/// Parses and validates the frontmatter of a `SKILL.md`; returns (name, description).
/// Only the top-level scalar fields are read (no YAML dependency needed for those).
fn parse(text: &str, dir_name: &str) -> Result<(String, String), String> {
    let (frontmatter, _) = split_frontmatter(text)?;
    let (mut name, mut description) = (None, None);
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if line.starts_with(char::is_whitespace) {
            continue; // nested value (e.g. under `metadata:`)
        }
        let value = unquote(value.trim());
        match key.trim() {
            "name" => name = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    let name = name.ok_or("missing `name`")?;
    let description = description.ok_or("missing `description`")?;
    validate_name(&name)?;
    if name != dir_name {
        return Err(format!(
            "name `{name}` does not match directory `{dir_name}`"
        ));
    }
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("`description` must be 1-1024 characters".into());
    }
    Ok((name, description))
}

fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    v.to_string()
}

fn validate_name(name: &str) -> Result<(), String> {
    let ok_char =
        |c: char| c.is_lowercase() && c.is_alphanumeric() || c.is_ascii_digit() || c == '-';
    if name.is_empty() || name.chars().count() > 64 {
        Err("`name` must be 1-64 characters".into())
    } else if !name.chars().all(ok_char) {
        Err(format!(
            "`name` `{name}` may only contain lowercase letters, digits and hyphens"
        ))
    } else if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        Err(format!(
            "`name` `{name}` must not start/end with or repeat hyphens"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_allow_the_right_skills() {
        let off = |names: &[&str]| SkillFilter::disabled(names.iter().map(|n| n.to_string()));
        let on = |names: &[&str]| SkillFilter::only(names.iter().map(|n| n.to_string()));
        assert!(SkillFilter::All.allows("a"));
        assert!(!off(&["a"]).allows("a") && off(&["a"]).allows("b"));
        assert!(on(&["a"]).allows("a") && !on(&["a"]).allows("b") && !on(&[]).allows("a"));
    }

    #[test]
    fn parses_valid_frontmatter() {
        let text = "---\nname: pdf-processing\ndescription: \"Does PDFs. Use for PDFs.\"\nmetadata:\n  name: other\n---\n# Body";
        assert_eq!(
            parse(text, "pdf-processing").unwrap(),
            ("pdf-processing".into(), "Does PDFs. Use for PDFs.".into())
        );
    }

    #[test]
    fn rejects_invalid_skills() {
        let ok = "---\nname: a-b\ndescription: d\n---\n";
        assert!(parse(ok, "a-b").is_ok());
        assert!(parse(ok, "other").is_err()); // dir mismatch
        assert!(parse("no frontmatter", "a").is_err());
        assert!(parse("---\nname: a\n---\n", "a").is_err()); // no description
        for bad in ["-a", "a-", "a--b", "A", "a b", ""] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        let long = "x".repeat(65);
        assert!(validate_name(&long).is_err());
    }

    #[test]
    fn instructions_strip_frontmatter() {
        let text = "---\r\nname: a\r\ndescription: d\r\n---\r\n\r\n# Body\r\ntext\r\n";
        assert_eq!(instructions(text), "# Body\r\ntext");
        assert_eq!(instructions("---\nname: a\n---\nx\n"), "x");
        assert_eq!(instructions("no frontmatter\n"), "no frontmatter");
    }

    #[test]
    fn builtin_skill_is_valid_and_in_prompt() {
        let dir = std::env::temp_dir().join(format!("agent_skills_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let skills = Skills::load(&dir).unwrap();
        assert!(skills.warnings.is_empty(), "{:?}", skills.warnings);
        let names: Vec<_> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["creating-skills", "paperless"]);
        for (path, _) in BUILTIN {
            assert!(dir.join(path).is_file(), "{path}");
        }
        let prompt = skills.prompt();
        assert!(prompt.contains("<name>creating-skills</name>"), "{prompt}");
        assert!(prompt.contains("call `use_skill`"), "{prompt}");
        assert!(!prompt.contains("<location>"), "{prompt}");
        assert!(!prompt.contains("--js"), "{prompt}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Workspaces of older versions still have the retired `using-skills` directory.
    #[test]
    fn retired_builtin_skill_is_removed_on_load() {
        let dir = std::env::temp_dir().join(format!("agent_skills_stale_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let stale = dir.join("using-skills/scripts");
        fs::create_dir_all(&stale).unwrap();
        fs::write(
            dir.join("using-skills/SKILL.md"),
            "---\nname: using-skills\ndescription: old\n---\n",
        )
        .unwrap();
        fs::write(stale.join("use_skill.js"), "").unwrap();
        let skills = Skills::load(&dir).unwrap();
        assert!(!dir.join("using-skills").exists());
        let names: Vec<_> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["creating-skills", "paperless"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
