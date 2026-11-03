use std::process::Command;

/// Without `.env.example`, startup creates it and exits with 0 (before needing any config).
#[test]
fn creates_env_example_and_exits() {
    let dir = std::env::temp_dir().join(format!("agent_startup_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent"))
        .current_dir(&dir)
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    let example = std::fs::read_to_string(dir.join(".env.example"));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{out:?}");
    assert!(example.unwrap().contains("LLM_BACKEND="));
}
