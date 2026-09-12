use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn hook_denies_uncovered_comment_write() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join(".skillforcer.toml"),
        r#"[[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} first""#,
    )
    .unwrap();
    std::fs::write(proj.path().join("t.jsonl"), "").unwrap();

    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let hook = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
        esc(&proj.path().join("t.jsonl")),
        esc(proj.path()),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_skillforcer"))
        .arg("hook")
        .current_dir(proj.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(hook.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // render_and_print prints serde_json::Value via `println!("{v}")`, which
    // is compact JSON (no spaces around `:`).
    assert!(
        stdout.contains("\"permissionDecision\":\"deny\""),
        "stdout was: {stdout}"
    );
    assert!(stdout.contains("tech-writing"));
}

#[test]
fn list_presets_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_skillforcer"))
        .arg("list-presets")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("code-comments"));
}
