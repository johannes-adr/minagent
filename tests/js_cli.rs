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
