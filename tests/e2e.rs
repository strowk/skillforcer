use std::io::Write;
use std::process::{Command, Stdio};

/// Spawns the built binary as `hook --harness codex`, pipes `input` on
/// stdin, and returns captured stdout. Mirrors the spawn mechanism used by
/// `hook_denies_uncovered_comment_write` above (same binary lookup via
/// `CARGO_BIN_EXE_skillforcer`, same current_dir/stdin/stdout wiring).
fn run_hook_binary(input: &str, cwd: &std::path::Path) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_skillforcer"))
        .arg("hook")
        .arg("--harness")
        .arg("codex")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

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
fn codex_hook_deny_then_allow_after_skill_read() {
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
    std::fs::write(proj.path().join("r.jsonl"), "").unwrap();
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");

    // The hook binary discovers a real, user-level state directory (there's
    // no env override), and state persists there across test runs. Use a
    // session id unique to this process invocation so a prior run's
    // recorded skill load can never leak into this run's first assertion.
    let session_id = format!(
        "e2e-cdx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let pre = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"{}","transcript_path":"{}","cwd":"{}","tool_name":"apply_patch","tool_input":{{"input":"*** Begin Patch\n*** Update File: src/a.rs\n+// hi\n*** End Patch"}}}}"#,
        session_id,
        esc(&proj.path().join("r.jsonl")),
        esc(proj.path()),
    );
    // 1. deny before any load
    let out = run_hook_binary(&pre, proj.path());
    assert!(
        out.contains("\"permissionDecision\":\"deny\""),
        "expected deny JSON, got: {out}"
    );

    // 2. record an implicit skill read
    let post = format!(
        r#"{{"hook_event_name":"PostToolUse","session_id":"{}","transcript_path":"{}","cwd":"{}","tool_name":"Bash","tool_input":{{"command":["bash","-lc","cat .agents/skills/tech-writing/SKILL.md"]}}}}"#,
        session_id,
        esc(&proj.path().join("r.jsonl")),
        esc(proj.path()),
    );
    let post_out = run_hook_binary(&post, proj.path());
    assert!(post_out.is_empty(), "expected no output, got: {post_out}");

    // 3. retried write passes (empty stdout = allow)
    let out = run_hook_binary(&pre, proj.path());
    assert!(!out.contains("deny"), "expected allow, got: {out}");
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
