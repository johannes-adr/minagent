use std::process::Command;

/// Without a valid config, startup creates `.env.example` (if missing) and exits with an error.
#[test]
fn missing_config_creates_env_example_and_fails() {
    let dir = std::env::temp_dir().join(format!("agent_startup_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent"))
        .current_dir(&dir)
        .env_remove("LLM_BACKEND")
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    let example = std::fs::read_to_string(dir.join(".env.example"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("OPENAI_API_KEY is not set"));
    assert!(example.unwrap().contains("LLM_BACKEND="));
}
