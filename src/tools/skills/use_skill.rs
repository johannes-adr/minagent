//! The `use_skill` tool: loads the full instructions of a skill, or one of its files
//! (e.g. a reference document on a subtopic) when the instructions name it.

use crate::harness::{Approval, Context, ToolPolicy};
use crate::tools::skills::{self, Skill, SkillFilter};
use rig_core::tool::{Tool, ToolContext};
use std::convert::Infallible;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// At most this many files are listed when a requested file does not exist.
const MAX_LISTED_FILES: usize = 50;

const DESCRIPTION: &str = "Load the full instructions of a skill listed under `available_skills` in the system prompt. \
Call it BEFORE starting a task that matches a skill's description, then follow the \
instructions it returns. Paths in them (`scripts/...`, `references/...`, `assets/...`) \
are relative to the returned skill directory. When the instructions name a document for a \
subtopic (e.g. `references/search.md`), load it only once the task needs it: call this tool \
again with the same `name` and `file` set to that path. Run the skill's scripts (shell \
scripts) with `execute`, e.g. `sh skills/<name>/scripts/x.sh [args]`. Import the skill's \
JavaScript modules in `execute_javascript`: `import { f } from \"skill:<name>/scripts/x.js\"`. \
A skill is not a tool: load it with this tool.";

#[derive(rig_core::serde::Deserialize, rig_core::schemars::JsonSchema)]
#[serde(crate = "rig_core::serde")]
#[schemars(crate = "rig_core::schemars")]
pub(crate) struct UseSkillParameters {
    /// Name of the skill to load, exactly as listed under available_skills (e.g. `creating-skills`).
    pub name: String,
    /// Optional: a file of the skill to load instead of its instructions, as the instructions name it (e.g. `references/search.md`). Omit it to load the instructions.
    #[serde(default)]
    pub file: Option<String>,
}

/// Loads skills. It cannot be a `#[rig_tool]` function because it needs to know which
/// skills the agent may use.
pub(crate) struct UseSkill {
    allowed: SkillFilter,
}

impl UseSkill {
    pub(crate) fn new(allowed: SkillFilter) -> Self {
        Self { allowed }
    }
}

impl Tool for UseSkill {
    const NAME: &'static str = "use_skill";
    type Args = UseSkillParameters;
    type Output = String;
    type Error = Infallible;

    fn description(&self) -> String {
        DESCRIPTION.to_string()
    }

    fn parameters(&self) -> rig_core::serde_json::Value {
        rig_core::schemars::schema_for!(UseSkillParameters).to_value()
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, Infallible> {
        Ok(load(
            Path::new(skills::DIR),
            &args.name,
            args.file.as_deref(),
            &self.allowed,
        ))
    }
}

/// Loading a skill only reads files, so it needs no approval.
impl ToolPolicy for UseSkill {
    const KIND: &'static str = "read";

    /// How to load skills and the list of available skills: the ones this agent may use.
    fn instance_prompt(&self, _context: &Context) -> String {
        skills::discover(Path::new(skills::DIR))
            .filtered(&self.allowed)
            .prompt()
    }

    fn approval(args: &UseSkillParameters, _context: &Context) -> Approval {
        match non_empty(args.file.as_deref()) {
            Some(file) => Approval::auto(format!("[skill] {}/{file}", args.name)),
            None => Approval::auto(format!("[skill] {}", args.name)),
        }
    }
}

fn non_empty(file: Option<&str>) -> Option<&str> {
    file.map(str::trim).filter(|f| !f.is_empty())
}

/// The instructions of skill `name` in `root` (or its `file`), or a message listing the
/// available skills.
fn load(root: &Path, name: &str, file: Option<&str>, allowed: &SkillFilter) -> String {
    let found = skills::discover(root).filtered(allowed);
    let Some(skill) = found.skills.iter().find(|s| s.name == name) else {
        let mut out = format!("Unknown skill `{name}`. Available skills:");
        for skill in &found.skills {
            let _ = write!(out, "\n- {}: {}", skill.name, skill.description);
        }
        return out;
    };
    if let Some(file) = non_empty(file) {
        return load_file(skill, file);
    }
    match fs::read_to_string(&skill.location) {
        Ok(text) => format!(
            "# Skill: {name}\nSkill directory: {}/{name}/\n\n{}",
            skills::DIR,
            skills::instructions(&text)
        ),
        Err(e) => format!("Could not read {}: {e}", skill.location.display()),
    }
}

/// The contents of `file` (relative to the skill's directory), or a message listing the
/// skill's files. A bare name like `search` also finds `references/search.md`.
fn load_file(skill: &Skill, file: &str) -> String {
    let name = &skill.name;
    let Some(dir) = skill.location.parent() else {
        return format!("Skill `{name}` has no directory");
    };
    let mut candidates = vec![file.to_owned()];
    if !file.contains('/') {
        candidates.push(format!("references/{file}"));
        if !file.ends_with(".md") {
            candidates.push(format!("references/{file}.md"));
        }
    }
    for candidate in &candidates {
        let Some(path) = resolve(dir, candidate) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        return match fs::read_to_string(&path) {
            Ok(text) => format!(
                "# Skill: {name}, file {candidate}\nSkill directory: {}/{name}/\n\n{}",
                skills::DIR,
                text.trim()
            ),
            Err(e) => format!("Could not read `{candidate}` of skill `{name}`: {e}"),
        };
    }
    let mut out = format!("Skill `{name}` has no file `{file}`. Its files:");
    let files = list_files(dir);
    for path in files.iter().take(MAX_LISTED_FILES) {
        let _ = write!(out, "\n- {path}");
    }
    if files.len() > MAX_LISTED_FILES {
        let _ = write!(out, "\n- ... ({} more)", files.len() - MAX_LISTED_FILES);
    }
    out
}

/// `relative` inside `dir`, or `None` if it would leave `dir` (absolute path, `..`,
/// symlink pointing outside) or names a hidden file such as `.env`.
fn resolve(dir: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    let plain = relative.components().all(|c| match c {
        Component::Normal(part) => !part.to_string_lossy().starts_with('.'),
        Component::CurDir => true,
        _ => false,
    });
    if !plain {
        return None;
    }
    let dir = dir.canonicalize().ok()?;
    let path = dir.join(relative).canonicalize().ok()?;
    path.starts_with(&dir).then_some(path)
}

/// The non-hidden files below `dir` as sorted relative paths with `/` separators.
fn list_files(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth > 0 {
                walk(&entry.path(), &format!("{prefix}{name}/"), depth - 1, out);
            } else if kind.is_file() {
                out.push(format!("{prefix}{name}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", 3, &mut out);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(root: &Path, name: &str, file: Option<&str>) -> String {
        super::load(root, name, file, &SkillFilter::All)
    }

    #[test]
    fn a_skill_that_is_not_allowed_is_unknown() {
        let dir = workspace("filtered");
        let off = SkillFilter::disabled(["demo".to_owned()]);
        let only_other = SkillFilter::only(["other".to_owned()]);
        for filter in [off, only_other] {
            for file in [None, Some("search")] {
                let out = super::load(&dir, "demo", file, &filter);
                assert!(out.starts_with("Unknown skill `demo`"), "{out}");
                assert!(!out.contains("A demo."), "{out}");
            }
        }
        let on = SkillFilter::only(["demo".to_owned()]);
        assert!(super::load(&dir, "demo", None, &on).contains("Do it."));
        let _ = fs::remove_dir_all(&dir);
    }

    fn workspace(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agent_use_skill_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("demo/references")).unwrap();
        fs::write(
            dir.join("demo/SKILL.md"),
            "---\nname: demo\ndescription: A demo.\n---\n# Demo steps\nDo it.\n",
        )
        .unwrap();
        fs::write(
            dir.join("demo/references/search.md"),
            "# Search\nQuery syntax.\n",
        )
        .unwrap();
        fs::write(dir.join("demo/.env"), "SECRET=1").unwrap();
        fs::write(dir.join("secret.txt"), "outside").unwrap();
        dir
    }

    #[test]
    fn loading_a_skill_needs_no_approval() {
        for isolated in [true, false] {
            for (file, title) in [
                (None, "[skill] demo"),
                (Some(" "), "[skill] demo"),
                (Some("references/a.md"), "[skill] demo/references/a.md"),
            ] {
                let args = UseSkillParameters {
                    name: "demo".into(),
                    file: file.map(String::from),
                };
                assert_eq!(
                    UseSkill::approval(&args, &Context { isolated }),
                    Approval::auto(title)
                );
            }
        }
    }

    #[test]
    fn returns_instructions_without_frontmatter() {
        let dir = workspace("ok");
        let out = load(&dir, "demo", None);
        let empty = load(&dir, "demo", Some(""));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            out,
            "# Skill: demo\nSkill directory: skills/demo/\n\n# Demo steps\nDo it."
        );
        assert_eq!(empty, out);
    }

    #[test]
    fn long_instructions_are_not_truncated() {
        let dir = workspace("long");
        let body = "line\n".repeat(20_000);
        fs::write(
            dir.join("demo/SKILL.md"),
            format!("---\nname: demo\ndescription: d\n---\n{body}"),
        )
        .unwrap();
        let out = load(&dir, "demo", None);
        let _ = fs::remove_dir_all(&dir);
        assert!(out.ends_with(body.trim()), "{}", out.len());
        assert!(!out.contains("omitted"));
    }

    #[test]
    fn unknown_or_invalid_name_lists_available_skills() {
        let dir = workspace("unknown");
        for name in ["nope", "../demo", ""] {
            let out = load(&dir, name, None);
            assert!(out.starts_with(&format!("Unknown skill `{name}`")), "{out}");
            assert!(out.contains("\n- demo: A demo."), "{out}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_a_file_of_the_skill() {
        let dir = workspace("file");
        let expected = "# Skill: demo, file references/search.md\nSkill directory: skills/demo/\n\n# Search\nQuery syntax.";
        for file in ["references/search.md", "./references/search.md"] {
            assert!(load(&dir, "demo", Some(file)).ends_with("# Search\nQuery syntax."));
        }
        // A bare topic name finds the reference document.
        assert_eq!(load(&dir, "demo", Some("search")), expected);
        assert_eq!(load(&dir, "demo", Some("search.md")), expected);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_outside_the_skill_or_hidden_are_not_loaded() {
        let dir = workspace("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("secret.txt"), dir.join("demo/link.txt")).unwrap();
        for file in [
            "../secret.txt",
            "references/../../secret.txt",
            "/etc/passwd",
            ".env",
            "link.txt",
            "nope.md",
            "references",
        ] {
            let out = load(&dir, "demo", Some(file));
            assert!(
                out.starts_with(&format!("Skill `demo` has no file `{file}`. Its files:")),
                "{file}: {out}"
            );
            assert!(!out.contains("outside") && !out.contains("SECRET"), "{out}");
            assert!(out.contains("\n- SKILL.md\n- "), "{out}");
            assert!(out.contains("\n- references/search.md"), "{out}");
            assert!(
                !out.contains("- .env") && !out.contains("link.txt\n"),
                "{out}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every `references/...` file the built-in `paperless` skill names can be loaded.
    #[test]
    fn files_named_by_the_paperless_skill_load() {
        let dir =
            std::env::temp_dir().join(format!("agent_use_skill_builtin_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        skills::Skills::load(&dir).unwrap();
        let text = fs::read_to_string(dir.join("paperless/SKILL.md")).unwrap();
        let named: Vec<_> = text
            .split('`')
            .filter(|s| s.starts_with("references/") && s.ends_with(".md"))
            .collect();
        assert_eq!(named.len(), 7, "{named:?}");
        for file in named {
            let out = load(&dir, "paperless", Some(file));
            assert!(
                out.starts_with(&format!("# Skill: paperless, file {file}")),
                "{out}"
            );
        }
        let out = load(&dir, "paperless", Some("search"));
        let _ = fs::remove_dir_all(&dir);
        assert!(
            out.starts_with("# Skill: paperless, file references/search.md"),
            "{out}"
        );
    }
}
