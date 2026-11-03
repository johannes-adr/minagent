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
