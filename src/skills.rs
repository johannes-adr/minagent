//! Built-in Agent Skills (https://agentskills.io/specification).
//!
//! Skills live in [`DIR`] (`skills/<name>/SKILL.md`, relative to the working directory).
//! The built-in ones are embedded in the binary and (re)installed on startup. Discovery
//! and the system prompt section are JavaScript (`system_prompt/skills.js`); the agent
//! loads a skill's full instructions on demand with the bundled `use_skill.js`.

use std::fs;
use std::io;
use std::path::Path;

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
    (
        "subagents/SKILL.md",
        include_str!("../builtin_skills/subagents/SKILL.md"),
    ),
    (
        "subagents/scripts/spawn_subagent.js",
        include_str!("../builtin_skills/subagents/scripts/spawn_subagent.js"),
    ),
    (
        "background-tasks/SKILL.md",
        include_str!("../builtin_skills/background-tasks/SKILL.md"),
    ),
    (
        "background-tasks/scripts/start_task.js",
        include_str!("../builtin_skills/background-tasks/scripts/start_task.js"),
    ),
    (
        "background-tasks/scripts/tasks.js",
        include_str!("../builtin_skills/background-tasks/scripts/tasks.js"),
    ),
    (
        "background-tasks/scripts/common.js",
        include_str!("../builtin_skills/background-tasks/scripts/common.js"),
    ),
];

/// Writes the embedded skills to `root` (the skill text refers to the agent executable
/// as `AGENT`; the system prompt gives its path).
pub fn install_builtin(root: &Path) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_builtin_skills() {
        let dir = std::env::temp_dir().join(format!("agent_skills_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        install_builtin(&dir).unwrap();
        for (path, content) in BUILTIN {
            assert_eq!(fs::read_to_string(dir.join(path)).unwrap(), *content);
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
