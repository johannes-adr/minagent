//! Agent Skills (https://agentskills.io/specification): discovery, validation and the
//! system prompt section.
//!
//! Skills live in [`DIR`] (`skills/<name>/SKILL.md`, relative to the working directory).
//! Only name and description go into the system prompt (progressive disclosure); the
//! agent loads the full instructions on demand with the bundled `use_skill.js`.
//! Built-in skills are embedded in the binary and (re)installed into [`DIR`] on startup.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Skills directory, relative to the working directory.
pub const DIR: &str = "skills";

/// Files of the embedded skills: (path below [`DIR`], contents).
const BUILTIN: &[(&str, &str)] = &[
    (
        "using-skills/SKILL.md",
        include_str!("../builtin_skills/using-skills/SKILL.md"),
    ),
    (
        "using-skills/scripts/use_skill.js",
        include_str!("../builtin_skills/using-skills/scripts/use_skill.js"),
    ),
    (
        "creating-skills/SKILL.md",
        include_str!("../builtin_skills/creating-skills/SKILL.md"),
    ),
];

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
    /// Installs the built-in skills into `root`, then discovers all skills there.
    pub fn load(root: &Path) -> io::Result<Skills> {
        install_builtin(root)?;
        Ok(discover(root))
    }

    /// The system prompt section: how to run scripts and the list of available skills.
    pub fn prompt(&self, agent_exe: &Path) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let exe = agent_exe.display();
        let mut out = format!(
            "\n\n# Skills\n\nSkills are specialised instructions stored as files. They are NOT tools: \
             never call a skill by name as a tool. When a task matches a skill's description, you \
             MUST first load its instructions by running this with the `execute` tool, and then \
             follow what it prints (do not answer from the description alone):\n\n\
             `{exe} --js {DIR}/using-skills/scripts/use_skill.js <skill-name>`\n\n\
             Example: `{exe} --js {DIR}/using-skills/scripts/use_skill.js creating-skills`\n\n\
             Other JavaScript files are run with `{exe} --js <file> [args]`.\n\n<available_skills>\n"
        );
        for skill in &self.skills {
            let _ = write!(
                out,
                "<skill>\n<name>{}</name>\n<description>{}</description>\n<location>{}</location>\n</skill>\n",
                escape(&skill.name),
                escape(&skill.description),
                escape(&skill.location.display().to_string()),
            );
        }
        out.push_str("</available_skills>\n");
        out
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Writes the embedded skills to `root` (the skill text refers to the agent executable
/// as `AGENT`; the system prompt gives its path).
fn install_builtin(root: &Path) -> io::Result<()> {
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

/// Parses and validates the frontmatter of a `SKILL.md`; returns (name, description).
/// Only the top-level scalar fields are read (no YAML dependency needed for those).
fn parse(text: &str, dir_name: &str) -> Result<(String, String), String> {
    let rest = text
        .strip_prefix("---")
        .and_then(|r| r.strip_prefix("\r\n").or_else(|| r.strip_prefix('\n')))
        .ok_or("missing frontmatter")?;
    let end = rest
        .lines()
        .position(|l| l.trim_end() == "---")
        .ok_or("unterminated frontmatter")?;
    let (mut name, mut description) = (None, None);
    for line in rest.lines().take(end) {
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
        return Err(format!("name `{name}` does not match directory `{dir_name}`"));
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
    let ok_char = |c: char| c.is_lowercase() && c.is_alphanumeric() || c.is_ascii_digit() || c == '-';
    if name.is_empty() || name.chars().count() > 64 {
        Err("`name` must be 1-64 characters".into())
    } else if !name.chars().all(ok_char) {
        Err(format!("`name` `{name}` may only contain lowercase letters, digits and hyphens"))
    } else if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        Err(format!("`name` `{name}` must not start/end with or repeat hyphens"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn builtin_skill_is_valid_and_in_prompt() {
        let dir = std::env::temp_dir().join(format!("agent_skills_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let skills = Skills::load(&dir).unwrap();
        assert!(skills.warnings.is_empty(), "{:?}", skills.warnings);
        let names: Vec<_> = skills.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["creating-skills", "using-skills"]);
        assert!(dir.join("using-skills/scripts/use_skill.js").exists());
        let prompt = skills.prompt(Path::new("/bin/agent"));
        assert!(prompt.contains("<name>using-skills</name>"), "{prompt}");
        assert!(prompt.contains("/bin/agent --js skills/using-skills/scripts/use_skill.js <skill-name>"), "{prompt}");
        assert!(prompt.contains("NOT tools"), "{prompt}");
        let _ = fs::remove_dir_all(&dir);
    }
}
