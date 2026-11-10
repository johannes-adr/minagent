use std::process::Command;

/// `--js` runs a script file directly.
#[test]
fn js_flag_runs_script() {
    let file = std::env::temp_dir().join(format!("agent_cli_test_{}.mjs", std::process::id()));
    std::fs::write(&file, "console.log('cli ' + (2 + 3));").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent"))
        .arg("--js")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "cli 5");
}

/// The bundled `use_skill.js` prints a skill's instructions, passing the name as argument.
#[test]
fn use_skill_script_prints_skill() {
    let dir = std::env::temp_dir().join(format!("agent_skill_test_{}", std::process::id()));
    let skill = dir.join("skills/demo");
    let script = dir.join("skills/using-skills/scripts");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::create_dir_all(&script).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: A demo.\n---\n# Demo steps\nDo it.\n",
    )
    .unwrap();
    std::fs::write(
        script.join("use_skill.js"),
        include_str!("../builtin_skills/using-skills/scripts/use_skill.js"),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_agent"))
            .current_dir(&dir)
            .args(["--js", "skills/using-skills/scripts/use_skill.js"])
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["demo"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# Demo steps") && !stdout.contains("description:"), "{stdout}");
    assert!(String::from_utf8_lossy(&run(&[]).stdout).contains("- demo: A demo."));
    assert!(!run(&["nope"]).status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--prompt` needs a non-empty prompt; it is checked before any config is loaded.
#[test]
fn prompt_flag_rejects_missing_or_empty_prompt() {
    let run = |args: &[&str], stdin: &str| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent"))
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run(&["--prompt"], "");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: agent --prompt"));
    let out = run(&["--prompt", "-"], " \n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("the prompt is empty"));
}

/// `spawn_subagent.js` hands the prompt to `agent --prompt -` and passes on its failure.
#[test]
fn spawn_subagent_script_reports_failure() {
    let dir = std::env::temp_dir().join(format!("agent_subagent_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env.example"), "").unwrap();
    std::fs::write(dir.join("task.md"), "say 'hi' \"there\" $HOME `x`\n").unwrap();
    let script = dir.join("spawn_subagent.js");
    std::fs::write(
        &script,
        include_str!("../builtin_skills/subagents/scripts/spawn_subagent.js"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent"))
        .current_dir(&dir)
        .env("LLM_BACKEND", "nope")
        .args(["--js", "spawn_subagent.js", "task.md"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("subagent failed (exit 1)"), "{stderr}");
    assert!(stderr.contains("unknown LLM_BACKEND"), "{stderr}");
}
