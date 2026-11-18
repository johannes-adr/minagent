//! The `use_skill` tool: loads the full instructions of a skill.

use crate::harness::{Approval, ToolPolicy};
use crate::tools::skills;
use rig_agent::rig_tool;
use rig_core::tool::ToolExecutionError;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Load the full instructions of a skill listed under `available_skills` in the system prompt.
/// Call it BEFORE starting a task that matches a skill's description, then follow the
/// instructions it returns. Paths in them (`scripts/...`, `references/...`, `assets/...`)
/// are relative to the returned skill directory. Run the skill's scripts with `execute`,
/// e.g. `<exe> --js skills/<name>/scripts/x.js [args]`, where `<exe>` is the agent
/// executable from the system prompt. A skill is not a tool: load it with this tool.
#[rig_tool(params(
    name = "Name of the skill to load, exactly as listed under available_skills (e.g. `creating-skills`)."
))]
pub(crate) async fn use_skill(name: String) -> Result<String, ToolExecutionError> {
    Ok(load(Path::new(skills::DIR), &name))
}

/// Loading a skill only reads files, so it needs no approval.
impl ToolPolicy for UseSkill {
    const KIND: &'static str = "read";

    fn approval(args: &UseSkillParameters, _sandboxed: bool) -> Approval {
        Approval::auto(format!("[skill] {}", args.name))
    }
}

/// The instructions of skill `name` in `root`, or a message listing the available skills.
fn load(root: &Path, name: &str) -> String {
    let found = skills::discover(root);
    let Some(skill) = found.skills.iter().find(|s| s.name == name) else {
        let mut out = format!("Unknown skill `{name}`. Available skills:");
        for skill in &found.skills {
            let _ = write!(out, "\n- {}: {}", skill.name, skill.description);
        }
        return out;
    };
    match fs::read_to_string(&skill.location) {
        Ok(text) => format!(
            "# Skill: {name}\nSkill directory: {}/{name}/\n\n{}",
            skills::DIR,
            skills::instructions(&text)
        ),
        Err(e) => format!("Could not read {}: {e}", skill.location.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agent_use_skill_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("demo")).unwrap();
        fs::write(
            dir.join("demo/SKILL.md"),
            "---\nname: demo\ndescription: A demo.\n---\n# Demo steps\nDo it.\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn loading_a_skill_needs_no_approval() {
        for sandboxed in [true, false] {
            let args = UseSkillParameters {
                name: "demo".into(),
            };
            assert_eq!(
                UseSkill::approval(&args, sandboxed),
                Approval::auto("[skill] demo")
            );
        }
    }

    #[test]
    fn returns_instructions_without_frontmatter() {
        let dir = workspace("ok");
        let out = load(&dir, "demo");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            out,
            "# Skill: demo\nSkill directory: skills/demo/\n\n# Demo steps\nDo it."
        );
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
        let out = load(&dir, "demo");
        let _ = fs::remove_dir_all(&dir);
        assert!(out.ends_with(body.trim()), "{}", out.len());
        assert!(!out.contains("omitted"));
    }

    #[test]
    fn unknown_or_invalid_name_lists_available_skills() {
        let dir = workspace("unknown");
        for name in ["nope", "../demo", ""] {
            let out = load(&dir, name);
            assert!(out.starts_with(&format!("Unknown skill `{name}`")), "{out}");
            assert!(out.contains("\n- demo: A demo."), "{out}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
