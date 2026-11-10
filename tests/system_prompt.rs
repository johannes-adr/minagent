use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agent_prompt_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn system_prompt(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agent"))
        .current_dir(dir)
        .arg("--system-prompt")
        .output()
        .unwrap()
}

/// The default template installs itself and lists the skills.
#[test]
fn default_prompt_lists_skills() {
    let dir = temp_dir("default");
    let out = system_prompt(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let installed = dir.join("system_prompt/system.js").exists();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
    assert!(installed);
    assert!(stdout.starts_with("You are a helpful agent."), "{stdout}");
    assert!(stdout.contains("<name>using-skills</name>"), "{stdout}");
    assert!(stdout.contains("--js skills/using-skills/scripts/use_skill.js <skill-name>"), "{stdout}");
    assert!(stdout.contains("NOT tools"), "{stdout}");
}

/// Files added to the directory are part of the prompt without being registered anywhere:
/// `.js` (async default export) and `.md`, in file name order; `_` files are helpers.
#[test]
fn added_components_are_discovered() {
    let dir = temp_dir("custom");
    assert!(system_prompt(&dir).status.success()); // installs the default directory
    let prompt = dir.join("system_prompt");
    std::fs::write(
        prompt.join("20-rules-description-list.js"),
        r#"import { bullet } from "./_helpers.js";
import fs from "node:fs";
export default async () =>
  "Rules:\n" + fs.readdirSync("rules").sort().map((f) => bullet(f.replace(/\.md$/, ""))).join("\n");"#,
    )
    .unwrap();
    std::fs::write(prompt.join("_helpers.js"), "export const bullet = (s) => `- ${s}`;").unwrap();
    std::fs::write(prompt.join("30-tone.md"), "Be brief.\n").unwrap();
    std::fs::write(prompt.join("40-empty.js"), "export default () => '';").unwrap();
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    std::fs::write(dir.join("rules/a.md"), "").unwrap();
    std::fs::write(dir.join("rules/b.md"), "").unwrap();

    let out = system_prompt(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
    let rules = stdout.find("Rules:\n- a\n- b\n\nBe brief.\n\n").expect(&stdout);
    assert!(stdout.starts_with("You are a helpful agent."), "{stdout}");
    assert!(rules < stdout.find("# Skills").expect(&stdout), "{stdout}"); // 20-... sorts before skills.js
    assert!(!stdout.contains("bullet"), "{stdout}");
}

/// Deleting a default file removes it from the prompt for good (it is not reinstalled).
#[test]
fn deleted_component_stays_deleted() {
    let dir = temp_dir("deleted");
    assert!(system_prompt(&dir).status.success());
    std::fs::remove_file(dir.join("system_prompt/skills.js")).unwrap();
    let out = system_prompt(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
    assert!(!stdout.contains("Skills"), "{stdout}");
}

/// An error in the template is reported instead of silently producing a prompt.
#[test]
fn failing_component_is_reported() {
    let dir = temp_dir("failing");
    assert!(system_prompt(&dir).status.success());
    std::fs::write(
        dir.join("system_prompt/50-broken.js"),
        "export default () => { throw new Error('boom-marker'); };",
    )
    .unwrap();
    let out = system_prompt(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success());
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("boom-marker"), "{text}");
}

/// An invalid skill is left out of the prompt and reported on stderr.
#[test]
fn invalid_skill_is_warned_about() {
    let dir = temp_dir("badskill");
    let bad = dir.join("skills/bad-one");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("SKILL.md"), "---\nname: other\ndescription: d\n---\n").unwrap();
    let out = system_prompt(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("<name>using-skills</name>") && !stdout.contains("bad-one"), "{stdout}");
    assert!(stderr.contains("does not match directory `bad-one`"), "{stderr}");
}
