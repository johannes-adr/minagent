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

/// A skill's script is run with `--js <file> [args]`; the args land in `globalThis.ARGS`.
#[test]
fn js_flag_passes_args_to_skill_script() {
    let dir = std::env::temp_dir().join(format!("agent_skill_test_{}", std::process::id()));
    let script = dir.join("skills/demo/scripts");
    std::fs::create_dir_all(&script).unwrap();
    std::fs::write(
        script.join("greet.js"),
        "console.log(globalThis.ARGS.join(','));",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent"))
        .current_dir(&dir)
        .args(["--js", "skills/demo/scripts/greet.js", "a", "b c"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "a,b c");
}
