# skillforcer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust CLI that acts as Claude Code hooks and denies configured writes unless a governing skill was loaded recently enough.

**Architecture:** One binary served by a `lib` + thin `bin`. A Claude *adapter* parses hook JSON into an agent-agnostic model (`WriteEvent`, `SkillLoad`, `Decision`) and renders decisions back. A rule engine matches writes (path glob AND content regex, with mergeable presets); a freshness evaluator checks whether the required skill(s) loaded within the configured window(s); the session transcript is the authoritative source of skill-load history, with a per-session state file as a cursor-backed cache. A self-installer wires the hooks into `.claude/settings.json`.

**Tech Stack:** Rust (edition 2024), `argh` (CLI), `serde`/`serde_json`, `toml`, `regex`, `globset`, `directories`, `jiff` (timestamps), `anyhow` (errors). Dev: `tempfile`.

**Spec:** `docs/superpowers/specs/2026-09-12-skillforcer-design.md`

## Global Constraints

- Rust edition = `2024`; crate name `skillforcer` (already set in `Cargo.toml`).
- Split into `src/lib.rs` (all logic, testable) + `src/main.rs` (thin `argh` dispatch). Tests live in `#[cfg(test)]` modules or `tests/`.
- **Fail open by default:** any internal error, unreadable transcript, malformed input, or missing config → *allow*, never panic out of a hook. Only `fail_open = false` flips this.
- Deny is emitted as JSON on stdout: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"<msg>"}}`. Allow emits nothing.
- Skill-set config keys are explicit: `any_skill` (fresh if any one loaded) / `all_skills` (all must be fresh). Never a bare `skills` key.
- Freshness modes: `session`, `minutes`, `turns`, `tokens`. When several are declared, combinator is AND unless `combine_freshness` says otherwise.
- Verified tool_input shapes: `Write{file_path, content}`, `Edit{file_path, new_string, old_string, replace_all}`, `MultiEdit{file_path, edits:[{new_string,...}]}`, `NotebookEdit{notebook_path, new_source}`, `Skill{skill}`.
- Never write state into the repo; state lives under the OS state/cache dir (`directories`). Config is TOML: `./.skillforcer.toml` layered over `~/.config/skillforcer/config.toml`.
- Commit after every task. Run `cargo test` and `cargo clippy --all-targets -- -D warnings` before each commit; both must pass.

---

## File Structure

- `Cargo.toml` — deps + `[lib]`/`[[bin]]`.
- `src/lib.rs` — module declarations + re-exports.
- `src/main.rs` — `argh` top-level, reads stdin for `hook`, dispatches.
- `src/model.rs` — `WriteEvent`, `SkillLoad`, `Decision`, `Cursor`, `Now`.
- `src/cli.rs` — `argh` structs for all subcommands.
- `src/adapter/mod.rs` — `pub mod claude;`
- `src/adapter/claude.rs` — parse hook stdin, extract write/skill from tool_input, render decision JSON.
- `src/config.rs` — raw TOML structs, load + layer + validate; `Requires`/`SkillSet`/`Windows`/`Combine`.
- `src/presets.rs` — embedded preset library + lookup.
- `src/rules.rs` — compile + match rules (glob × regex), resolve `extends`.
- `src/freshness.rs` — evaluate `Requires` against `SkillLoad`s + `Now`.
- `src/transcript.rs` — incremental cursor-based JSONL scan (skill loads, turns, tokens).
- `src/state.rs` — per-session state store (load/save/prune).
- `src/install.rs` — settings.json read/modify/write, config scaffold.
- `src/commands.rs` — `run_hook`, `run_install`, `run_uninstall`, `run_check`, `run_status`, `run_list_presets`.
- `tests/fixtures/` — recorded hook payloads + a sample transcript.

---

### Task 1: Crate skeleton + CLI dispatch

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs`, `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `cli::Cli` (argh top-level) with subcommands `Hook`, `Install`, `Uninstall`, `Check`, `Status`, `ListPresets`; `cli::dispatch(cli: Cli) -> anyhow::Result<i32>` returning a process exit code. Each subcommand handler is a stub returning `Ok(0)` after printing `"not implemented"` to stderr, to be filled by later tasks.

- [ ] **Step 1: Set up `Cargo.toml`**

```toml
[package]
name = "skillforcer"
version = "0.1.0"
edition = "2024"

[lib]
name = "skillforcer"
path = "src/lib.rs"

[[bin]]
name = "skillforcer"
path = "src/main.rs"

[dependencies]
argh = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
regex = "1"
globset = "0.4"
directories = "5"
jiff = "0.1"
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing test** (in `src/cli.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_list_presets_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["list-presets"]).unwrap();
        assert!(matches!(cli.sub, Sub::ListPresets(_)));
    }
    #[test]
    fn parses_hook_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["hook"]).unwrap();
        assert!(matches!(cli.sub, Sub::Hook(_)));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib cli`
Expected: FAIL — `Cli`/`Sub` not defined.

- [ ] **Step 4: Implement `src/cli.rs`**

```rust
use argh::FromArgs;

/// Force skills to load before governed writes.
#[derive(FromArgs, Debug)]
pub struct Cli {
    #[argh(subcommand)]
    pub sub: Sub,
}

#[derive(FromArgs, Debug)]
#[argh(subcommand)]
pub enum Sub {
    Hook(HookCmd),
    Install(InstallCmd),
    Uninstall(UninstallCmd),
    Check(CheckCmd),
    Status(StatusCmd),
    ListPresets(ListPresetsCmd),
}

/// Run as a Claude Code hook (reads hook JSON on stdin).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "hook")]
pub struct HookCmd {}

/// Install skillforcer hooks into Claude settings.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "install")]
pub struct InstallCmd {
    /// target .claude/settings.local.json instead of settings.json
    #[argh(switch)]
    pub local: bool,
    /// target ~/.claude/settings.json
    #[argh(switch)]
    pub user: bool,
    /// print changes without writing
    #[argh(switch)]
    pub dry_run: bool,
}

/// Remove skillforcer hooks from Claude settings.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "uninstall")]
pub struct UninstallCmd {
    /// target .claude/settings.local.json
    #[argh(switch)]
    pub local: bool,
    /// target ~/.claude/settings.json
    #[argh(switch)]
    pub user: bool,
}

/// Evaluate rules against a file and report which fire.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "check")]
pub struct CheckCmd {
    /// file path to evaluate
    #[argh(positional)]
    pub file: String,
    /// read content from stdin instead of the file
    #[argh(switch)]
    pub stdin: bool,
}

/// Show detected skill-load state for a session.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "status")]
pub struct StatusCmd {
    /// session id
    #[argh(option)]
    pub session: Option<String>,
}

/// List bundled presets.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "list-presets")]
pub struct ListPresetsCmd {}

pub fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match cli.sub {
        Sub::Hook(_)
        | Sub::Install(_)
        | Sub::Uninstall(_)
        | Sub::Check(_)
        | Sub::Status(_)
        | Sub::ListPresets(_) => {
            eprintln!("not implemented");
            Ok(0)
        }
    }
}
```

- [ ] **Step 5: Implement `src/lib.rs`**

```rust
pub mod cli;
```

- [ ] **Step 6: Implement `src/main.rs`**

```rust
fn main() {
    let cli: skillforcer::cli::Cli = argh::from_env();
    match skillforcer::cli::dispatch(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("skillforcer: {e:#}");
            std::process::exit(0); // fail open: never block on our own error
        }
    }
}
```

- [ ] **Step 7: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS. `cargo run -- --help` lists subcommands.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/
git commit -m "feat: crate skeleton and CLI dispatch"
```

---

### Task 2: Core model + decision rendering

**Files:**
- Create: `src/model.rs`, `src/adapter/mod.rs`, `src/adapter/claude.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - `model::WriteEvent { path: PathBuf, content: String }`
  - `model::SkillLoad { skill: String, at: jiff::Timestamp, turn: u64, tokens: u64 }`
  - `model::Decision { Allow, Deny { reason: String } }`
  - `model::Cursor { offset: u64, turn: u64, tokens: u64 }` (Default = zeros)
  - `model::Now { time: jiff::Timestamp, turn: u64, tokens: u64 }`
  - `adapter::claude::render_decision(&Decision) -> Option<serde_json::Value>` — `None` for Allow, `Some(json)` for Deny.

- [ ] **Step 1: Write the failing test** (in `src/adapter/claude.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Decision;

    #[test]
    fn allow_renders_nothing() {
        assert_eq!(render_decision(&Decision::Allow), None);
    }

    #[test]
    fn deny_renders_permission_json() {
        let v = render_decision(&Decision::Deny { reason: "load it".into() }).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "load it");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib adapter`
Expected: FAIL — module/functions undefined.

- [ ] **Step 3: Implement `src/model.rs`**

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEvent {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SkillLoad {
    pub skill: String,
    pub at: jiff::Timestamp,
    pub turn: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub offset: u64,
    pub turn: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub time: jiff::Timestamp,
    pub turn: u64,
    pub tokens: u64,
}
```

- [ ] **Step 4: Implement `src/adapter/mod.rs` and `src/adapter/claude.rs`**

`src/adapter/mod.rs`:
```rust
pub mod claude;
```

`src/adapter/claude.rs` (render only for now):
```rust
use crate::model::Decision;
use serde_json::json;

pub fn render_decision(d: &Decision) -> Option<serde_json::Value> {
    match d {
        Decision::Allow => None,
        Decision::Deny { reason } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })),
    }
}
```

- [ ] **Step 5: Update `src/lib.rs`**

```rust
pub mod adapter;
pub mod cli;
pub mod model;
```

- [ ] **Step 6: Run tests + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/
git commit -m "feat: core model and deny/allow rendering"
```

---

### Task 3: Hook input parsing + tool extraction

**Files:**
- Modify: `src/adapter/claude.rs`
- Create: `tests/fixtures/pre_write.json`, `tests/fixtures/pre_edit.json`, `tests/fixtures/pre_notebook.json`, `tests/fixtures/post_skill.json`

**Interfaces:**
- Consumes: `model::WriteEvent`.
- Produces:
  - `adapter::claude::HookInput { PreToolUse(PreToolUse), PostToolUse(PostToolUse), Other }`
  - `adapter::claude::PreToolUse { session_id: String, transcript_path: PathBuf, cwd: PathBuf, tool_name: String, tool_input: serde_json::Value }` (same fields for `PostToolUse`).
  - `adapter::claude::parse_hook_input(&str) -> anyhow::Result<HookInput>`
  - `adapter::claude::write_event_from_tool(tool_name: &str, tool_input: &serde_json::Value) -> Option<WriteEvent>`
  - `adapter::claude::skill_from_tool(tool_name: &str, tool_input: &serde_json::Value) -> Option<String>`

- [ ] **Step 1: Create fixtures**

`tests/fixtures/pre_write.json`:
```json
{"hook_event_name":"PreToolUse","session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","tool_name":"Write","tool_input":{"file_path":"src/lib.rs","content":"// a comment\nfn x() {}"}}
```
`tests/fixtures/pre_edit.json`:
```json
{"hook_event_name":"PreToolUse","session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","tool_name":"Edit","tool_input":{"file_path":"src/lib.rs","old_string":"a","new_string":"// new comment","replace_all":false}}
```
`tests/fixtures/pre_notebook.json`:
```json
{"hook_event_name":"PreToolUse","session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","tool_name":"NotebookEdit","tool_input":{"notebook_path":"nb.ipynb","new_source":"# heading","cell_type":"markdown","edit_mode":"replace"}}
```
`tests/fixtures/post_skill.json`:
```json
{"hook_event_name":"PostToolUse","session_id":"s1","transcript_path":"/tmp/t.jsonl","cwd":"/proj","tool_name":"Skill","tool_input":{"skill":"tech-writing:technical-writing"}}
```

- [ ] **Step 2: Write the failing test** (in `src/adapter/claude.rs`)

```rust
#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parses_pre_write_and_extracts_content() {
        let raw = include_str!("../../tests/fixtures/pre_write.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        assert_eq!(p.tool_name, "Write");
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.path.to_str().unwrap(), "src/lib.rs");
        assert!(ev.content.contains("// a comment"));
    }

    #[test]
    fn edit_uses_new_string() {
        let raw = include_str!("../../tests/fixtures/pre_edit.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.content, "// new comment");
    }

    #[test]
    fn notebook_uses_notebook_path_and_new_source() {
        let raw = include_str!("../../tests/fixtures/pre_notebook.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.path.to_str().unwrap(), "nb.ipynb");
        assert_eq!(ev.content, "# heading");
    }

    #[test]
    fn unknown_tool_yields_none() {
        assert!(write_event_from_tool("Bash", &serde_json::json!({"command":"ls"})).is_none());
    }

    #[test]
    fn extracts_skill_from_post() {
        let raw = include_str!("../../tests/fixtures/post_skill.json");
        let HookInput::PostToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        assert_eq!(skill_from_tool(&p.tool_name, &p.tool_input).unwrap(), "tech-writing:technical-writing");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib adapter`
Expected: FAIL — `parse_hook_input`/`HookInput` undefined.

- [ ] **Step 4: Implement in `src/adapter/claude.rs`**

```rust
use crate::model::WriteEvent;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct RawHook {
    hook_event_name: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    transcript_path: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PreToolUse {
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

pub type PostToolUse = PreToolUse;

#[derive(Debug, Clone)]
pub enum HookInput {
    PreToolUse(PreToolUse),
    PostToolUse(PostToolUse),
    Other,
}

pub fn parse_hook_input(raw: &str) -> anyhow::Result<HookInput> {
    let h: RawHook = serde_json::from_str(raw)?;
    let common = PreToolUse {
        session_id: h.session_id,
        transcript_path: PathBuf::from(h.transcript_path),
        cwd: PathBuf::from(h.cwd),
        tool_name: h.tool_name,
        tool_input: h.tool_input,
    };
    Ok(match h.hook_event_name.as_str() {
        "PreToolUse" => HookInput::PreToolUse(common),
        "PostToolUse" => HookInput::PostToolUse(common),
        _ => HookInput::Other,
    })
}

pub fn skill_from_tool(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    if tool_name != "Skill" {
        return None;
    }
    input.get("skill")?.as_str().map(str::to_string)
}

pub fn write_event_from_tool(tool_name: &str, input: &serde_json::Value) -> Option<WriteEvent> {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let (path, content) = match tool_name {
        "Write" => (s("file_path")?, s("content")?),
        "Edit" => (s("file_path")?, s("new_string")?),
        "MultiEdit" => {
            let edits = input.get("edits")?.as_array()?;
            let joined = edits
                .iter()
                .filter_map(|e| e.get("new_string").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (s("file_path")?, joined)
        }
        "NotebookEdit" => (s("notebook_path")?, s("new_source")?),
        _ => return None,
    };
    Some(WriteEvent { path: PathBuf::from(path), content })
}
```

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/ tests/fixtures/
git commit -m "feat: parse hook input and extract write/skill events"
```

---

### Task 4: Config model, loading, layering, validation

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Produces:
  - `config::Combine { All, Any }` (Deserialize from `"all"`/`"any"`; Default `All`).
  - `config::SkillSet { Any(Vec<String>), All(Vec<String>) }`
  - `config::Windows { session: bool, minutes: Option<u64>, turns: Option<u64>, tokens: Option<u64> }`
  - `config::Requires { skills: SkillSet, windows: Windows }`
  - `config::Defaults { fail_open: bool, combine_freshness: Combine }` (Default: `fail_open=true`).
  - `config::RuleDef { name: String, extends: Vec<String>, path: Vec<String>, content: Option<String>, requires: Requires, message: Option<String> }`
  - `config::Config { defaults: Defaults, rules: Vec<RuleDef> }`
  - `config::load(project_dir: &Path, global_path: Option<&Path>) -> anyhow::Result<Config>` — parses project `.skillforcer.toml` layered over the global file; validates; returns `Config`. Missing project file → `Config::default()` (no rules).
  - `config::parse_str(project_toml: &str, global_toml: Option<&str>) -> anyhow::Result<Config>` (the testable core `load` wraps).

- [ ] **Step 1: Write the failing test** (in `src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [defaults]
        fail_open = false
        combine_freshness = "all"

        [[rule]]
        name = "comments"
        path = ["src/**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], minutes = 15, turns = 40 }
        message = "load {skills}"
    "#;

    #[test]
    fn parses_rule_and_requires() {
        let cfg = parse_str(SAMPLE, None).unwrap();
        assert!(!cfg.defaults.fail_open);
        assert_eq!(cfg.rules.len(), 1);
        let r = &cfg.rules[0];
        assert!(matches!(&r.requires.skills, SkillSet::Any(v) if v == &["tech-writing"]));
        assert_eq!(r.requires.windows.minutes, Some(15));
        assert_eq!(r.requires.windows.turns, Some(40));
    }

    #[test]
    fn rejects_both_skill_keys() {
        let bad = r#"[[rule]]
            name = "x"
            requires = { any_skill = ["a"], all_skills = ["b"], session = true }"#;
        assert!(parse_str(bad, None).is_err());
    }

    #[test]
    fn rejects_no_freshness() {
        let bad = r#"[[rule]]
            name = "x"
            requires = { any_skill = ["a"] }"#;
        assert!(parse_str(bad, None).is_err());
    }

    #[test]
    fn project_overrides_global_defaults() {
        let global = r#"[defaults]
            fail_open = true"#;
        let project = r#"[defaults]
            fail_open = false"#;
        let cfg = parse_str(project, Some(global)).unwrap();
        assert!(!cfg.defaults.fail_open);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/config.rs`**

Use raw serde structs mirroring the TOML, then convert+validate into the public `Config`. Layering: parse global (if any) and project; project's `defaults` fields override global; rules are the project's (global rules appended first, project after — project wins on same `name`).

```rust
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSet {
    Any(Vec<String>),
    All(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Windows {
    pub session: bool,
    pub minutes: Option<u64>,
    pub turns: Option<u64>,
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requires {
    pub skills: SkillSet,
    pub windows: Windows,
}

#[derive(Debug, Clone)]
pub struct Defaults {
    pub fail_open: bool,
    pub combine_freshness: Combine,
}
impl Default for Defaults {
    fn default() -> Self {
        Self { fail_open: true, combine_freshness: Combine::All }
    }
}

#[derive(Debug, Clone)]
pub struct RuleDef {
    pub name: String,
    pub extends: Vec<String>,
    pub path: Vec<String>,
    pub content: Option<String>,
    pub requires: Requires,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub defaults: Defaults,
    pub rules: Vec<RuleDef>,
}

// ---- raw serde layer ----
#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    defaults: RawDefaults,
    #[serde(default, rename = "rule")]
    rules: Vec<RawRule>,
}
#[derive(Debug, Deserialize, Default)]
struct RawDefaults {
    fail_open: Option<bool>,
    combine_freshness: Option<Combine>,
}
#[derive(Debug, Deserialize)]
struct RawRule {
    name: String,
    #[serde(default)]
    extends: Vec<String>,
    #[serde(default)]
    path: Vec<String>,
    content: Option<String>,
    requires: RawRequires,
    message: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RawRequires {
    any_skill: Option<Vec<String>>,
    all_skills: Option<Vec<String>>,
    #[serde(default)]
    session: bool,
    minutes: Option<u64>,
    turns: Option<u64>,
    tokens: Option<u64>,
}

fn convert_rule(r: RawRule) -> Result<RuleDef> {
    let skills = match (r.requires.any_skill, r.requires.all_skills) {
        (Some(_), Some(_)) => bail!("rule '{}': set only one of any_skill/all_skills", r.name),
        (Some(a), None) => SkillSet::Any(a),
        (None, Some(a)) => SkillSet::All(a),
        (None, None) => bail!("rule '{}': one of any_skill/all_skills is required", r.name),
    };
    let windows = Windows {
        session: r.requires.session,
        minutes: r.requires.minutes,
        turns: r.requires.turns,
        tokens: r.requires.tokens,
    };
    if !windows.session && windows.minutes.is_none() && windows.turns.is_none() && windows.tokens.is_none() {
        bail!("rule '{}': at least one freshness window (session/minutes/turns/tokens) is required", r.name);
    }
    Ok(RuleDef {
        name: r.name,
        extends: r.extends,
        path: r.path,
        content: r.content,
        requires: Requires { skills, windows },
        message: r.message,
    })
}

pub fn parse_str(project_toml: &str, global_toml: Option<&str>) -> Result<Config> {
    let global: RawConfig = match global_toml {
        Some(t) => toml::from_str(t).context("parsing global config")?,
        None => RawConfig::default(),
    };
    let project: RawConfig = toml::from_str(project_toml).context("parsing project config")?;

    let defaults = Defaults {
        fail_open: project.defaults.fail_open.or(global.defaults.fail_open).unwrap_or(true),
        combine_freshness: project
            .defaults
            .combine_freshness
            .or(global.defaults.combine_freshness)
            .unwrap_or_default(),
    };

    let mut rules: Vec<RuleDef> = Vec::new();
    for raw in global.rules.into_iter().chain(project.rules.into_iter()) {
        let converted = convert_rule(raw)?;
        if let Some(existing) = rules.iter_mut().find(|x| x.name == converted.name) {
            *existing = converted; // project (later) wins on same name
        } else {
            rules.push(converted);
        }
    }
    Ok(Config { defaults, rules })
}

pub fn load(project_dir: &Path, global_path: Option<&Path>) -> Result<Config> {
    let proj_path = project_dir.join(".skillforcer.toml");
    let project_toml = match std::fs::read_to_string(&proj_path) {
        Ok(s) => s,
        Err(_) => return Ok(Config::default()),
    };
    let global_toml = global_path.and_then(|p| std::fs::read_to_string(p).ok());
    parse_str(&project_toml, global_toml.as_deref())
}
```

- [ ] **Step 4: Update `src/lib.rs`** — add `pub mod config;`

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: config model, layering, validation"
```

---

### Task 5: Preset library + extends resolution

**Files:**
- Create: `src/presets.rs`
- Modify: `src/lib.rs`, `src/config.rs`

**Interfaces:**
- Consumes: `config::RuleDef`.
- Produces:
  - `presets::Preset { name: &'static str, content: Option<&'static str>, path: &'static [&'static str] }`
  - `presets::builtin() -> &'static [Preset]`
  - `presets::get(name: &str) -> Option<&'static Preset>`
  - `config::resolve_extends(rule: &RuleDef) -> anyhow::Result<RuleDef>` — merges named presets: preset `content`/`path` fill in when the rule leaves them empty; rule `path` entries append to preset `path`; unknown preset name → error. (Rule's own `content` wins if set.)

- [ ] **Step 1: Write the failing test** (in `src/presets.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn code_comments_preset_exists_and_matches() {
        let p = get("code-comments").unwrap();
        let re = Regex::new(p.content.unwrap()).unwrap();
        assert!(re.is_match("// hello"));
        assert!(re.is_match("/* block */"));
        assert!(re.is_match("value = 1  # trailing"));
        assert!(!re.is_match("let x = 1;"));
    }

    #[test]
    fn markdown_headings_preset_exists() {
        let p = get("markdown-headings").unwrap();
        let re = Regex::new(p.content.unwrap()).unwrap();
        assert!(re.is_match("# Title"));
        assert!(!re.is_match("no heading here"));
    }
}
```

Add in `src/config.rs`:
```rust
#[cfg(test)]
mod extends_tests {
    use super::*;
    #[test]
    fn extends_fills_content_from_preset() {
        let rule = RuleDef {
            name: "c".into(),
            extends: vec!["code-comments".into()],
            path: vec!["src/**/*.rs".into()],
            content: None,
            requires: Requires { skills: SkillSet::Any(vec!["s".into()]), windows: Windows { session: true, ..Default::default() } },
            message: None,
        };
        let resolved = resolve_extends(&rule).unwrap();
        assert!(resolved.content.is_some());
        assert!(resolved.path.iter().any(|p| p == "src/**/*.rs"));
    }
    #[test]
    fn unknown_preset_errors() {
        let rule = RuleDef {
            name: "c".into(), extends: vec!["nope".into()], path: vec![], content: Some("x".into()),
            requires: Requires { skills: SkillSet::All(vec!["s".into()]), windows: Windows { session: true, ..Default::default() } },
            message: None,
        };
        assert!(resolve_extends(&rule).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib presets && cargo test --lib config`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/presets.rs`**

```rust
pub struct Preset {
    pub name: &'static str,
    pub content: Option<&'static str>,
    pub path: &'static [&'static str],
}

// Heuristic comment detector: line/block comment leaders and trailing `#`.
const CODE_COMMENTS: &str = r"(//|/\*|\*/|<!--|(^|\s)#|(^|\s);;)";
const MD_HEADINGS: &str = r"(?m)^#{1,6}\s";

static PRESETS: &[Preset] = &[
    Preset { name: "code-comments", content: Some(CODE_COMMENTS), path: &["**/*.rs", "**/*.ts", "**/*.js", "**/*.go", "**/*.py", "**/*.c", "**/*.h", "**/*.cpp", "**/*.java"] },
    Preset { name: "markdown-headings", content: Some(MD_HEADINGS), path: &["**/*.md"] },
];

pub fn builtin() -> &'static [Preset] {
    PRESETS
}

pub fn get(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}
```

- [ ] **Step 4: Implement `resolve_extends` in `src/config.rs`**

```rust
pub fn resolve_extends(rule: &RuleDef) -> anyhow::Result<RuleDef> {
    let mut out = rule.clone();
    let mut preset_paths: Vec<String> = Vec::new();
    for name in &rule.extends {
        let p = crate::presets::get(name)
            .ok_or_else(|| anyhow::anyhow!("rule '{}': unknown preset '{}'", rule.name, name))?;
        if out.content.is_none() {
            out.content = p.content.map(str::to_string);
        }
        preset_paths.extend(p.path.iter().map(|s| s.to_string()));
    }
    // preset paths first, then the rule's own (both apply as a set)
    let mut merged = preset_paths;
    merged.extend(rule.path.iter().cloned());
    out.path = merged;
    out.extends = Vec::new();
    Ok(out)
}
```

- [ ] **Step 5: Update `src/lib.rs`** — add `pub mod presets;`

- [ ] **Step 6: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: preset library and extends resolution"
```

---

### Task 6: Rule engine (compile + match)

**Files:**
- Create: `src/rules.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `config::RuleDef` (already `extends`-resolved), `model::WriteEvent`, `config::resolve_extends`.
- Produces:
  - `rules::CompiledRule { def: config::RuleDef, globset: Option<globset::GlobSet>, content: Option<regex::Regex> }`
  - `rules::compile(rule: &config::RuleDef) -> anyhow::Result<CompiledRule>` — calls `resolve_extends` internally, then builds globset (empty `path` → `None` = matches any path) and regex.
  - `rules::CompiledRule::matches(&self, ev: &model::WriteEvent) -> bool` — path AND content (a `None` predicate is treated as "matches").

- [ ] **Step 1: Write the failing test** (in `src/rules.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Requires, RuleDef, SkillSet, Windows};
    use crate::model::WriteEvent;
    use std::path::PathBuf;

    fn rule(path: Vec<&str>, content: Option<&str>) -> RuleDef {
        RuleDef {
            name: "t".into(),
            extends: vec![],
            path: path.into_iter().map(String::from).collect(),
            content: content.map(String::from),
            requires: Requires { skills: SkillSet::Any(vec!["s".into()]), windows: Windows { session: true, ..Default::default() } },
            message: None,
        }
    }
    fn ev(path: &str, content: &str) -> WriteEvent {
        WriteEvent { path: PathBuf::from(path), content: content.into() }
    }

    #[test]
    fn matches_path_and_content() {
        let c = compile(&rule(vec!["src/**/*.rs"], Some("//"))).unwrap();
        assert!(c.matches(&ev("src/a/b.rs", "// hi")));
        assert!(!c.matches(&ev("src/a/b.rs", "no comment")));
        assert!(!c.matches(&ev("docs/x.md", "// hi")));
    }

    #[test]
    fn empty_path_matches_any() {
        let c = compile(&rule(vec![], Some("TODO"))).unwrap();
        assert!(c.matches(&ev("anywhere.txt", "TODO fix")));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib rules`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/rules.rs`**

```rust
use crate::config::{resolve_extends, RuleDef};
use crate::model::WriteEvent;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

pub struct CompiledRule {
    pub def: RuleDef,
    pub globset: Option<GlobSet>,
    pub content: Option<Regex>,
}

pub fn compile(rule: &RuleDef) -> Result<CompiledRule> {
    let def = resolve_extends(rule)?;
    let globset = if def.path.is_empty() {
        None
    } else {
        let mut b = GlobSetBuilder::new();
        for p in &def.path {
            b.add(Glob::new(p).with_context(|| format!("rule '{}': bad glob '{}'", def.name, p))?);
        }
        Some(b.build()?)
    };
    let content = match &def.content {
        Some(re) => Some(Regex::new(re).with_context(|| format!("rule '{}': bad regex", def.name))?),
        None => None,
    };
    Ok(CompiledRule { def, globset, content })
}

impl CompiledRule {
    pub fn matches(&self, ev: &WriteEvent) -> bool {
        let path_ok = match &self.globset {
            Some(gs) => gs.is_match(&ev.path),
            None => true,
        };
        if !path_ok {
            return false;
        }
        match &self.content {
            Some(re) => re.is_match(&ev.content),
            None => true,
        }
    }
}
```

- [ ] **Step 4: Update `src/lib.rs`** — add `pub mod rules;`

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: rule compilation and matching"
```

---

### Task 7: Freshness evaluator

**Files:**
- Create: `src/freshness.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `config::{Requires, SkillSet, Windows, Combine}`, `model::{SkillLoad, Now}`.
- Produces:
  - `freshness::FreshnessResult { satisfied: bool, reason: String }`
  - `freshness::evaluate(req: &Requires, loads: &[SkillLoad], now: &Now, combine: Combine) -> FreshnessResult`

Semantics: for each required skill, find its most recent `SkillLoad`. A load satisfies a declared window if `session` (any load), `minutes` (`now.time - load.at <= minutes`), `turns` (`now.turn - load.turn <= turns`), `tokens` (`now.tokens - load.tokens <= tokens`). With `Combine::All`, every *declared* window must pass; with `Any`, at least one. `SkillSet::Any` → satisfied if any one skill's freshness passes; `All` → every listed skill must pass. `reason` is a human string, e.g. `"skill 'x' last loaded 41m ago (window 15m)"` or `"skill 'x' never loaded this session"`.

- [ ] **Step 1: Write the failing test** (in `src/freshness.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Combine, Requires, SkillSet, Windows};
    use crate::model::{Now, SkillLoad};

    fn ts(s: &str) -> jiff::Timestamp { s.parse().unwrap() }

    fn load(skill: &str, at: &str, turn: u64, tokens: u64) -> SkillLoad {
        SkillLoad { skill: skill.into(), at: ts(at), turn, tokens }
    }
    fn now(at: &str, turn: u64, tokens: u64) -> Now {
        Now { time: ts(at), turn, tokens }
    }

    #[test]
    fn session_satisfied_by_any_load() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { session: true, ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        let r = evaluate(&req, &loads, &now("2026-09-12T12:00:00Z", 500, 999999), Combine::All);
        assert!(r.satisfied);
    }

    #[test]
    fn minutes_window_expires() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { minutes: Some(15), ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        // 41 minutes later
        let r = evaluate(&req, &loads, &now("2026-09-12T10:41:00Z", 5, 200), Combine::All);
        assert!(!r.satisfied);
        assert!(r.reason.contains("x"));
    }

    #[test]
    fn all_skills_requires_each_fresh() {
        let req = Requires { skills: SkillSet::All(vec!["a".into(), "b".into()]), windows: Windows { session: true, ..Default::default() } };
        let loads = vec![load("a", "2026-09-12T10:00:00Z", 1, 100)];
        let r = evaluate(&req, &loads, &now("2026-09-12T10:05:00Z", 3, 150), Combine::All);
        assert!(!r.satisfied); // b never loaded
    }

    #[test]
    fn turns_window_and_combinator_all() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { minutes: Some(60), turns: Some(10), ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        // within minutes(60) but 40 turns later -> AND fails
        let r = evaluate(&req, &loads, &now("2026-09-12T10:30:00Z", 41, 200), Combine::All);
        assert!(!r.satisfied);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib freshness`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/freshness.rs`**

```rust
use crate::config::{Combine, Requires, SkillSet, Windows};
use crate::model::{Now, SkillLoad};

pub struct FreshnessResult {
    pub satisfied: bool,
    pub reason: String,
}

fn latest<'a>(loads: &'a [SkillLoad], skill: &str) -> Option<&'a SkillLoad> {
    loads.iter().filter(|l| l.skill == skill).max_by_key(|l| l.at)
}

// Returns (passed, reason) for a single skill against the windows.
fn skill_fresh(skill: &str, loads: &[SkillLoad], now: &Now, w: &Windows, combine: Combine) -> (bool, String) {
    let Some(load) = latest(loads, skill) else {
        return (false, format!("skill '{skill}' never loaded this session"));
    };

    let mut checks: Vec<(bool, String)> = Vec::new();
    if w.session {
        checks.push((true, String::new())); // a load exists
    }
    if let Some(m) = w.minutes {
        let elapsed_min = now.time.duration_since(load.at).as_secs().max(0) as u64 / 60;
        checks.push((elapsed_min <= m, format!("skill '{skill}' last loaded {elapsed_min}m ago (window {m}m)")));
    }
    if let Some(t) = w.turns {
        let dt = now.turn.saturating_sub(load.turn);
        checks.push((dt <= t, format!("skill '{skill}' loaded {dt} turns ago (window {t})")));
    }
    if let Some(tok) = w.tokens {
        let dtok = now.tokens.saturating_sub(load.tokens);
        checks.push((dtok <= tok, format!("skill '{skill}' loaded {dtok} tokens ago (window {tok})")));
    }

    let passed = match combine {
        Combine::All => checks.iter().all(|(ok, _)| *ok),
        Combine::Any => checks.iter().any(|(ok, _)| *ok),
    };
    let reason = checks
        .iter()
        .find(|(ok, _)| !*ok)
        .map(|(_, r)| r.clone())
        .unwrap_or_else(|| format!("skill '{skill}' fresh"));
    (passed, reason)
}

pub fn evaluate(req: &Requires, loads: &[SkillLoad], now: &Now, combine: Combine) -> FreshnessResult {
    match &req.skills {
        SkillSet::Any(skills) => {
            let mut last_reason = "no skills configured".to_string();
            for s in skills {
                let (ok, reason) = skill_fresh(s, loads, now, &req.windows, combine);
                if ok {
                    return FreshnessResult { satisfied: true, reason };
                }
                last_reason = reason;
            }
            FreshnessResult { satisfied: false, reason: last_reason }
        }
        SkillSet::All(skills) => {
            for s in skills {
                let (ok, reason) = skill_fresh(s, loads, now, &req.windows, combine);
                if !ok {
                    return FreshnessResult { satisfied: false, reason };
                }
            }
            FreshnessResult { satisfied: true, reason: "all skills fresh".into() }
        }
    }
}
```

Note: `jiff::Timestamp::duration_since` returns a `SignedDuration`; use `.as_secs()`. If the API differs, compute `(now.time - load.at)` via `now.time.since(load.at)`. Adjust to the installed `jiff` version; the test pins the behavior.

- [ ] **Step 4: Update `src/lib.rs`** — add `pub mod freshness;`

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: freshness evaluation across all modes"
```

---

### Task 8: Transcript scanner (incremental, cursor)

**Files:**
- Create: `src/transcript.rs`, `tests/fixtures/transcript.jsonl`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `model::{SkillLoad, Cursor}`.
- Produces:
  - `transcript::Scan { skill_loads: Vec<SkillLoad>, end: Cursor }`
  - `transcript::scan(path: &std::path::Path, from: model::Cursor) -> anyhow::Result<Scan>` — reads lines starting at `from.offset`; each JSON line advances `turn` when `type` is `"user"` or `"assistant"`; adds `message.usage.output_tokens` (+`input_tokens` if present, else just output) to running tokens; when an assistant message contains a `tool_use` with `name == "Skill"`, appends a `SkillLoad` with the line `timestamp`, current turn, current tokens. `end` reflects new offset/turn/tokens (base = `from`).

Token accounting: define `tokens` as cumulative `output_tokens` summed across assistant messages (simple, monotonic). Document this in a code comment.

- [ ] **Step 1: Create `tests/fixtures/transcript.jsonl`** (one JSON object per line)

```json
{"type":"user","timestamp":"2026-09-12T10:00:00Z","message":{"role":"user","content":"hi"}}
{"type":"assistant","timestamp":"2026-09-12T10:00:05Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"tech-writing"}}],"usage":{"output_tokens":50}}}
{"type":"user","timestamp":"2026-09-12T10:01:00Z","message":{"role":"user","content":"ok"}}
{"type":"assistant","timestamp":"2026-09-12T10:01:10Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"output_tokens":30}}}
```

- [ ] **Step 2: Write the failing test** (in `src/transcript.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cursor;
    use std::path::Path;

    #[test]
    fn finds_skill_load_with_turn_and_tokens() {
        let scan = scan(Path::new("tests/fixtures/transcript.jsonl"), Cursor::default()).unwrap();
        assert_eq!(scan.skill_loads.len(), 1);
        let s = &scan.skill_loads[0];
        assert_eq!(s.skill, "tech-writing");
        assert_eq!(s.turn, 2); // user(1) then assistant(2)
        assert_eq!(s.tokens, 50);
        assert_eq!(scan.end.turn, 4);
        assert_eq!(scan.end.tokens, 80);
    }

    #[test]
    fn incremental_from_cursor_returns_no_old_loads() {
        let first = scan(Path::new("tests/fixtures/transcript.jsonl"), Cursor::default()).unwrap();
        let second = scan(Path::new("tests/fixtures/transcript.jsonl"), first.end).unwrap();
        assert!(second.skill_loads.is_empty());
        assert_eq!(second.end.turn, 4);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib transcript`
Expected: FAIL — undefined.

- [ ] **Step 4: Implement `src/transcript.rs`**

```rust
use crate::model::{Cursor, SkillLoad};
use anyhow::Result;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

pub struct Scan {
    pub skill_loads: Vec<SkillLoad>,
    pub end: Cursor,
}

pub fn scan(path: &Path, from: Cursor) -> Result<Scan> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(from.offset))?;
    let mut reader = BufReader::new(file);

    let mut turn = from.turn;
    let mut tokens = from.tokens;
    let mut offset = from.offset;
    let mut loads = Vec::new();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue; // skip malformed lines (fail open)
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "user" || ty == "assistant" {
            turn += 1;
        }
        if let Some(out) = v.pointer("/message/usage/output_tokens").and_then(|t| t.as_u64()) {
            tokens += out;
        }
        if ty == "assistant" {
            if let Some(items) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for item in items {
                    let is_skill = item.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        && item.get("name").and_then(|t| t.as_str()) == Some("Skill");
                    if is_skill {
                        if let Some(skill) = item.pointer("/input/skill").and_then(|s| s.as_str()) {
                            let at = v
                                .get("timestamp")
                                .and_then(|t| t.as_str())
                                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                                .unwrap_or_else(jiff::Timestamp::now);
                            loads.push(SkillLoad { skill: skill.to_string(), at, turn, tokens });
                        }
                    }
                }
            }
        }
    }

    Ok(Scan { skill_loads: loads, end: Cursor { offset, turn, tokens } })
}
```

- [ ] **Step 5: Update `src/lib.rs`** — add `pub mod transcript;`

- [ ] **Step 6: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/ tests/fixtures/transcript.jsonl
git commit -m "feat: incremental transcript scanner"
```

---

### Task 9: Session state store

**Files:**
- Create: `src/state.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `model::{SkillLoad, Cursor}`.
- Produces:
  - `state::SessionState { session_id: String, loads: Vec<SkillLoad>, cursor: model::Cursor }` (serde).
  - `state::Store { .. }` with:
    - `Store::with_base(base: PathBuf) -> Store` (for tests)
    - `Store::discover() -> anyhow::Result<Store>` (uses `directories::ProjectDirs::from("", "", "skillforcer")` state/data dir)
    - `load(&self, session_id: &str) -> SessionState` (missing → fresh default with that id; never errors on absence)
    - `save(&self, st: &SessionState) -> anyhow::Result<()>`
    - `prune(&self, max_age: std::time::Duration)` (best-effort; ignores errors)

- [ ] **Step 1: Write the failing test** (in `src/state.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cursor, SkillLoad};

    #[test]
    fn round_trips_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::with_base(dir.path().to_path_buf());
        let mut st = store.load("sess-1");
        assert!(st.loads.is_empty());
        st.loads.push(SkillLoad { skill: "x".into(), at: "2026-09-12T10:00:00Z".parse().unwrap(), turn: 2, tokens: 50 });
        st.cursor = Cursor { offset: 123, turn: 4, tokens: 80 };
        store.save(&st).unwrap();

        let again = store.load("sess-1");
        assert_eq!(again.loads.len(), 1);
        assert_eq!(again.cursor.offset, 123);
    }

    #[test]
    fn missing_session_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::with_base(dir.path().to_path_buf());
        let st = store.load("nope");
        assert_eq!(st.session_id, "nope");
        assert_eq!(st.cursor, Cursor::default());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib state`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/state.rs`**

```rust
use crate::model::{Cursor, SkillLoad};
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionState {
    pub session_id: String,
    #[serde(default)]
    pub loads: Vec<SkillLoad>,
    #[serde(default)]
    pub cursor: Cursor,
}

pub struct Store {
    base: PathBuf,
}

impl Store {
    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn discover() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "skillforcer")
            .ok_or_else(|| anyhow::anyhow!("cannot determine state directory"))?;
        let base = dirs.state_dir().unwrap_or_else(|| dirs.data_dir()).join("sessions");
        Ok(Self { base })
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        let safe: String = session_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        self.base.join(format!("{safe}.json"))
    }

    pub fn load(&self, session_id: &str) -> SessionState {
        let path = self.path_for(session_id);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionState>(&s).ok())
            .unwrap_or(SessionState { session_id: session_id.to_string(), loads: Vec::new(), cursor: Cursor::default() })
    }

    pub fn save(&self, st: &SessionState) -> Result<()> {
        std::fs::create_dir_all(&self.base)?;
        let path = self.path_for(&st.session_id);
        let json = serde_json::to_string_pretty(st)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn prune(&self, max_age: std::time::Duration) {
        let Ok(rd) = std::fs::read_dir(&self.base) else { return };
        let now = std::time::SystemTime::now();
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if now.duration_since(modified).map(|d| d > max_age).unwrap_or(false) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Update `src/lib.rs`** — add `pub mod state;`

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: session state store"
```

---

### Task 10: Guard + recorder command (the hook engine)

**Files:**
- Create: `src/commands.rs`
- Modify: `src/lib.rs`, `src/cli.rs`

**Interfaces:**
- Consumes: everything above (`adapter::claude`, `config`, `rules`, `freshness`, `transcript`, `state`, `model`).
- Produces:
  - `commands::run_hook(raw_stdin: &str, store: &state::Store, project_dir: &Path, global_path: Option<&Path>) -> model::Decision` — pure decision (testable). It never returns `Err`; on any internal error it returns `Decision::Allow` when `fail_open`, honoring config's `fail_open` when the config is readable.
  - `commands::render_and_print(&model::Decision)` — prints deny JSON to stdout (nothing for allow).
  - `commands::message_for(rule: &config::RuleDef, ev: &model::WriteEvent, reason: &str) -> String` — templating for `{file}`, `{rule}`, `{skills}`, `{reason}`.
  - `cli::dispatch` `Sub::Hook` arm now: read stdin, discover store, resolve project dir from hook `cwd`, call `run_hook`, print, and on `PostToolUse` update state (record skill + advance cursor).

Behavior for `run_hook`:
1. `parse_hook_input`. On parse error → `Allow`.
2. `PostToolUse` + skill present → load state, scan transcript from `state.cursor`, append any new skill loads, record the just-loaded skill (from tool_input, timestamped `now`, at `end` turn/tokens), save state, return `Allow`.
3. `PreToolUse` + `write_event_from_tool` is `Some` → load config; if config load fails, `Allow`. Compile rules. For each matching rule: build `Now` (time now; turn/tokens from a fresh transcript scan merged into state), gather `loads` (state.loads plus new scan loads), evaluate freshness. First matching rule that is **unsatisfied** → `Deny{message_for(...)}` (respect `fail_open=false` meaning still deny — deny is the point; `fail_open` only governs *errors*). If matched rules are all satisfied or none match → `Allow`. On any error mid-way → `Allow` if `defaults.fail_open` else `Deny` with an error reason.
4. Any other event / non-write tool → `Allow`.

- [ ] **Step 1: Write the failing test** (in `src/commands.rs`)

Use a temp project dir with a `.skillforcer.toml` and a temp transcript. Build hook JSON strings inline.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Decision;
    use crate::state::Store;
    use std::io::Write;

    fn write_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    const CFG: &str = r#"
        [[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} before editing {file}"
    "#;

    #[test]
    fn denies_write_when_skill_never_loaded() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", ""); // empty transcript = no skill loads
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());

        let hook = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
            transcript.display().to_string().replace('\\', "\\\\"),
            proj.path().display().to_string().replace('\\', "\\\\"),
        );
        let d = run_hook(&hook, &store, proj.path(), None);
        match d {
            Decision::Deny { reason } => {
                assert!(reason.contains("tech-writing"));
                assert!(reason.contains("src/a.rs"));
            }
            _ => panic!("expected deny"),
        }
    }

    #[test]
    fn allows_write_after_skill_recorded_via_post_hook() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", "");
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());

        let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let post = format!(
            r#"{{"hook_event_name":"PostToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Skill","tool_input":{{"skill":"tech-writing"}}}}"#,
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&post, &store, proj.path(), None), Decision::Allow));

        let pre = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&pre, &store, proj.path(), None), Decision::Allow));
    }

    #[test]
    fn allows_non_matching_write() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", "");
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let pre = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"README.md","content":"no comment"}}}}"#,
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&pre, &store, proj.path(), None), Decision::Allow));
    }

    #[test]
    fn malformed_input_fails_open() {
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        assert!(matches!(run_hook("not json", &store, std::path::Path::new("."), None), Decision::Allow));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib commands`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/commands.rs`** (hook engine + helpers)

```rust
use crate::adapter::claude::{self, HookInput};
use crate::config::{self, Config, RuleDef, SkillSet};
use crate::freshness;
use crate::model::{Decision, Now, WriteEvent};
use crate::rules;
use crate::state::{SessionState, Store};
use crate::transcript;
use std::path::Path;

pub fn message_for(rule: &RuleDef, ev: &WriteEvent, reason: &str) -> String {
    let skills = match &rule.requires.skills {
        SkillSet::Any(s) | SkillSet::All(s) => s.join(", "),
    };
    let template = rule.message.clone().unwrap_or_else(|| {
        "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it with the Skill tool, then retry.".to_string()
    });
    template
        .replace("{file}", &ev.path.display().to_string())
        .replace("{rule}", &rule.name)
        .replace("{skills}", &skills)
        .replace("{reason}", reason)
}

fn refresh_state(st: &mut SessionState, transcript_path: &Path) {
    if let Ok(scan) = transcript::scan(transcript_path, st.cursor) {
        st.loads.extend(scan.skill_loads);
        st.cursor = scan.end;
    }
}

pub fn run_hook(raw: &str, store: &Store, project_dir: &Path, global_path: Option<&Path>) -> Decision {
    let input = match claude::parse_hook_input(raw) {
        Ok(i) => i,
        Err(_) => return Decision::Allow,
    };

    match input {
        HookInput::PostToolUse(p) => {
            if let Some(skill) = claude::skill_from_tool(&p.tool_name, &p.tool_input) {
                let mut st = store.load(&p.session_id);
                refresh_state(&mut st, &p.transcript_path);
                // record the just-loaded skill at the current cursor position
                st.loads.push(crate::model::SkillLoad {
                    skill,
                    at: jiff::Timestamp::now(),
                    turn: st.cursor.turn,
                    tokens: st.cursor.tokens,
                });
                let _ = store.save(&st);
                store.prune(std::time::Duration::from_secs(60 * 60 * 24 * 7));
            }
            Decision::Allow
        }
        HookInput::PreToolUse(p) => {
            let Some(ev) = claude::write_event_from_tool(&p.tool_name, &p.tool_input) else {
                return Decision::Allow;
            };
            // Resolve project dir from the hook's cwd when present.
            let proj = if p.cwd.as_os_str().is_empty() { project_dir.to_path_buf() } else { p.cwd.clone() };
            let cfg: Config = match config::load(&proj, global_path) {
                Ok(c) => c,
                Err(_) => return Decision::Allow,
            };
            let fail_open = cfg.defaults.fail_open;
            let combine = cfg.defaults.combine_freshness;

            let mut st = store.load(&p.session_id);
            refresh_state(&mut st, &p.transcript_path);
            let _ = store.save(&st);

            let now = Now { time: jiff::Timestamp::now(), turn: st.cursor.turn, tokens: st.cursor.tokens };

            for rule in &cfg.rules {
                let compiled = match rules::compile(rule) {
                    Ok(c) => c,
                    Err(_) => {
                        if fail_open { continue } else { return Decision::Deny { reason: format!("skillforcer: rule '{}' failed to compile", rule.name) } }
                    }
                };
                if compiled.matches(&ev) {
                    let res = freshness::evaluate(&compiled.def.requires, &st.loads, &now, combine);
                    if !res.satisfied {
                        return Decision::Deny { reason: message_for(&compiled.def, &ev, &res.reason) };
                    }
                }
            }
            Decision::Allow
        }
        HookInput::Other => Decision::Allow,
    }
}

pub fn render_and_print(d: &Decision) {
    if let Some(v) = claude::render_decision(d) {
        println!("{v}");
    }
}
```

- [ ] **Step 4: Wire `Sub::Hook` in `src/cli.rs`**

Replace the `Sub::Hook(_)` stub so it reads stdin and runs the engine:
```rust
Sub::Hook(_) => {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok();
    let store = crate::state::Store::discover().unwrap_or_else(|_| crate::state::Store::with_base(std::env::temp_dir().join("skillforcer")));
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let global = directories::ProjectDirs::from("", "", "skillforcer").map(|d| d.config_dir().join("config.toml"));
    let decision = crate::commands::run_hook(&raw, &store, &cwd, global.as_deref());
    crate::commands::render_and_print(&decision);
    Ok(0)
}
```
(Keep the other stubs unchanged in this task.)

- [ ] **Step 5: Update `src/lib.rs`** — add `pub mod commands;`

- [ ] **Step 6: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: hook guard and recorder engine"
```

---

### Task 11: `check`, `status`, `list-presets` commands

**Files:**
- Modify: `src/commands.rs`, `src/cli.rs`

**Interfaces:**
- Consumes: `config`, `rules`, `presets`, `state`, `model`.
- Produces:
  - `commands::run_check(project_dir: &Path, file: &Path, content: &str, global: Option<&Path>) -> anyhow::Result<Vec<String>>` — returns names of rules that match (path+content) the given content; also prints them.
  - `commands::run_list_presets() -> String` — newline list of `name` (+ default paths).
  - `commands::run_status(store: &Store, session_id: &str) -> String` — pretty summary of recorded loads + cursor.

- [ ] **Step 1: Write the failing test** (in `src/commands.rs`)

```rust
#[cfg(test)]
mod cmd_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn check_reports_matching_rule() {
        let proj = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(proj.path().join(".skillforcer.toml")).unwrap();
        f.write_all(br#"[[rule]]
            name = "comments"
            path = ["**/*.rs"]
            content = "//"
            requires = { any_skill = ["tech-writing"], session = true }"#).unwrap();
        let hits = run_check(proj.path(), std::path::Path::new("src/a.rs"), "// c", None).unwrap();
        assert_eq!(hits, vec!["comments".to_string()]);
    }

    #[test]
    fn list_presets_includes_code_comments() {
        assert!(run_list_presets().contains("code-comments"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib commands`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement the three functions in `src/commands.rs`**

```rust
use crate::model::WriteEvent;

pub fn run_check(project_dir: &Path, file: &Path, content: &str, global: Option<&Path>) -> anyhow::Result<Vec<String>> {
    let cfg = config::load(project_dir, global)?;
    let ev = WriteEvent { path: file.to_path_buf(), content: content.to_string() };
    let mut hits = Vec::new();
    for rule in &cfg.rules {
        if let Ok(compiled) = rules::compile(rule) {
            if compiled.matches(&ev) {
                hits.push(rule.name.clone());
            }
        }
    }
    for h in &hits {
        println!("match: {h}");
    }
    if hits.is_empty() {
        println!("no rules match {}", file.display());
    }
    Ok(hits)
}

pub fn run_list_presets() -> String {
    let mut out = String::new();
    for p in crate::presets::builtin() {
        out.push_str(&format!("{}  (paths: {})\n", p.name, p.path.join(", ")));
    }
    out
}

pub fn run_status(store: &Store, session_id: &str) -> String {
    let st = store.load(session_id);
    let mut out = format!("session {}: {} skill load(s); cursor turn={} tokens={} offset={}\n",
        st.session_id, st.loads.len(), st.cursor.turn, st.cursor.tokens, st.cursor.offset);
    for l in &st.loads {
        out.push_str(&format!("  - {} @ {} (turn {}, {} tokens)\n", l.skill, l.at, l.turn, l.tokens));
    }
    out
}
```

- [ ] **Step 4: Wire the three subcommands in `src/cli.rs`**

```rust
Sub::Check(c) => {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let file = std::path::PathBuf::from(&c.file);
    let content = if c.stdin {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).ok();
        s
    } else {
        std::fs::read_to_string(&file).unwrap_or_default()
    };
    let global = directories::ProjectDirs::from("", "", "skillforcer").map(|d| d.config_dir().join("config.toml"));
    crate::commands::run_check(&cwd, &file, &content, global.as_deref())?;
    Ok(0)
}
Sub::Status(s) => {
    let store = crate::state::Store::discover()?;
    let session = s.session.clone().unwrap_or_default();
    print!("{}", crate::commands::run_status(&store, &session));
    Ok(0)
}
Sub::ListPresets(_) => {
    print!("{}", crate::commands::run_list_presets());
    Ok(0)
}
```

- [ ] **Step 5: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: check, status, list-presets commands"
```

---

### Task 12: Install / uninstall + config scaffold

**Files:**
- Create: `src/install.rs`
- Modify: `src/commands.rs`, `src/cli.rs`, `src/lib.rs`

**Interfaces:**
- Produces:
  - `install::Target { Project, Local, User }` with `Target::settings_path(project_dir: &Path) -> anyhow::Result<PathBuf>`.
  - `install::install(target: Target, exe: &str, project_dir: &Path, dry_run: bool) -> anyhow::Result<String>` — reads (or creates) the settings JSON, adds a `PreToolUse` entry (matcher `Write|Edit|MultiEdit|NotebookEdit`) and a `PostToolUse` entry (matcher `Skill`), each with a `command` `"{exe} hook"`. **Idempotent**: identified by the command containing `skillforcer` and `hook`; running twice adds no duplicate. Returns a summary string. On `dry_run`, does not write.
  - `install::uninstall(target: Target, project_dir: &Path) -> anyhow::Result<()>` — removes hook entries whose command contains `skillforcer`.
  - `install::scaffold_config(project_dir: &Path) -> anyhow::Result<bool>` — writes a commented `.skillforcer.toml` if absent; returns whether created.
  - The pure JSON transform is factored as `install::add_hooks(root: &mut serde_json::Value, exe: &str)` and `install::remove_hooks(root: &mut serde_json::Value)` for direct unit testing.

- [ ] **Step 1: Write the failing test** (in `src/install.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_hooks_is_idempotent() {
        let mut root = json!({});
        add_hooks(&mut root, "/usr/bin/skillforcer");
        add_hooks(&mut root, "/usr/bin/skillforcer");
        let pre = &root["hooks"]["PreToolUse"];
        assert_eq!(pre.as_array().unwrap().len(), 1);
        let post = &root["hooks"]["PostToolUse"];
        assert_eq!(post.as_array().unwrap().len(), 1);
        // matcher + command present
        assert_eq!(pre[0]["matcher"], "Write|Edit|MultiEdit|NotebookEdit");
        assert!(pre[0]["hooks"][0]["command"].as_str().unwrap().contains("skillforcer"));
    }

    #[test]
    fn remove_hooks_drops_only_ours() {
        let mut root = json!({
            "hooks": { "PreToolUse": [
                {"matcher":"Bash","hooks":[{"type":"command","command":"other-tool"}]}
            ]}
        });
        add_hooks(&mut root, "/x/skillforcer");
        remove_hooks(&mut root);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "other-tool");
    }

    #[test]
    fn scaffold_creates_then_skips() {
        let proj = tempfile::tempdir().unwrap();
        assert!(scaffold_config(proj.path()).unwrap());
        assert!(!scaffold_config(proj.path()).unwrap());
        assert!(proj.path().join(".skillforcer.toml").exists());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib install`
Expected: FAIL — undefined.

- [ ] **Step 3: Implement `src/install.rs`**

```rust
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const PRE_MATCHER: &str = "Write|Edit|MultiEdit|NotebookEdit";
pub const POST_MATCHER: &str = "Skill";

#[derive(Debug, Clone, Copy)]
pub enum Target {
    Project,
    Local,
    User,
}

impl Target {
    pub fn settings_path(&self, project_dir: &Path) -> Result<PathBuf> {
        Ok(match self {
            Target::Project => project_dir.join(".claude").join("settings.json"),
            Target::Local => project_dir.join(".claude").join("settings.local.json"),
            Target::User => {
                let home = directories::BaseDirs::new().context("no home dir")?;
                home.home_dir().join(".claude").join("settings.json")
            }
        })
    }
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| arr.iter().any(|h| h.get("command").and_then(|c| c.as_str()).map(|c| c.contains("skillforcer")).unwrap_or(false)))
        .unwrap_or(false)
}

fn event_entry(matcher: &str, exe: &str) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [ { "type": "command", "command": format!("{exe} hook") } ]
    })
}

fn ensure_event(root: &mut Value, event: &str, matcher: &str, exe: &str) {
    let hooks = root.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    let arr = hooks.as_object_mut().unwrap().entry(event).or_insert_with(|| json!([]));
    let arr = arr.as_array_mut().unwrap();
    // idempotent: skip if one of ours with this matcher already exists
    let exists = arr.iter().any(|e| is_ours(e) && e.get("matcher").and_then(|m| m.as_str()) == Some(matcher));
    if !exists {
        arr.push(event_entry(matcher, exe));
    }
}

pub fn add_hooks(root: &mut Value, exe: &str) {
    if !root.is_object() {
        *root = json!({});
    }
    ensure_event(root, "PreToolUse", PRE_MATCHER, exe);
    ensure_event(root, "PostToolUse", POST_MATCHER, exe);
}

pub fn remove_hooks(root: &mut Value) {
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else { return };
    for event in ["PreToolUse", "PostToolUse"] {
        if let Some(arr) = hooks.get_mut(event).and_then(|a| a.as_array_mut()) {
            arr.retain(|e| !is_ours(e));
        }
    }
}

fn read_root(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Ok(serde_json::from_str(&s).context("parsing settings.json")?),
        _ => Ok(json!({})),
    }
}

fn write_root(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(root)?)?;
    Ok(())
}

pub fn install(target: Target, exe: &str, project_dir: &Path, dry_run: bool) -> Result<String> {
    let path = target.settings_path(project_dir)?;
    let mut root = read_root(&path)?;
    add_hooks(&mut root, exe);
    let pretty = serde_json::to_string_pretty(&root)?;
    if dry_run {
        return Ok(format!("[dry-run] would write {}:\n{}", path.display(), pretty));
    }
    write_root(&path, &root)?;
    Ok(format!("installed hooks into {}", path.display()))
}

pub fn uninstall(target: Target, project_dir: &Path) -> Result<()> {
    let path = target.settings_path(project_dir)?;
    let mut root = read_root(&path)?;
    remove_hooks(&mut root);
    write_root(&path, &root)
}

const SCAFFOLD: &str = r#"# skillforcer configuration.
# Docs: run `skillforcer list-presets` to see bundled presets.

[defaults]
fail_open = true          # a skillforcer error never blocks a write
combine_freshness = "all" # when multiple windows are set, all must hold

# Example: force the tech-writing skill before comment edits in Rust/TS.
# [[rule]]
# name = "comments-need-tech-writing"
# extends = ["code-comments"]
# path = ["src/**/*.rs", "**/*.ts"]
# requires = { any_skill = ["tech-writing:technical-writing"], minutes = 15 }
# message = "Editing comments in {file}. Load {skills} first — context may have rotted."
"#;

pub fn scaffold_config(project_dir: &Path) -> Result<bool> {
    let path = project_dir.join(".skillforcer.toml");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, SCAFFOLD)?;
    Ok(true)
}
```

- [ ] **Step 4: Wire `Sub::Install` / `Sub::Uninstall` in `src/cli.rs`**

```rust
Sub::Install(c) => {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "skillforcer".into());
    let target = if c.user { crate::install::Target::User } else if c.local { crate::install::Target::Local } else { crate::install::Target::Project };
    let summary = crate::install::install(target, &exe, &cwd, c.dry_run)?;
    println!("{summary}");
    if !c.dry_run {
        let created = crate::install::scaffold_config(&cwd)?;
        println!("{}", if created { "created .skillforcer.toml" } else { ".skillforcer.toml already present" });
    }
    Ok(0)
}
Sub::Uninstall(c) => {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let target = if c.user { crate::install::Target::User } else if c.local { crate::install::Target::Local } else { crate::install::Target::Project };
    crate::install::uninstall(target, &cwd)?;
    println!("uninstalled skillforcer hooks");
    Ok(0)
}
```

- [ ] **Step 5: Update `src/lib.rs`** — add `pub mod install;`

- [ ] **Step 6: Run tests + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "feat: install/uninstall and config scaffold"
```

---

### Task 13: End-to-end smoke test + README

**Files:**
- Create: `tests/e2e.rs`, `README.md`

**Interfaces:**
- Consumes: the compiled binary (via `env!("CARGO_BIN_EXE_skillforcer")`).

- [ ] **Step 1: Write the failing e2e test** (`tests/e2e.rs`)

```rust
use std::process::{Command, Stdio};
use std::io::Write;

#[test]
fn hook_denies_uncovered_comment_write() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(proj.path().join(".skillforcer.toml"), r#"[[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} first""#).unwrap();
    std::fs::write(proj.path().join("t.jsonl"), "").unwrap();

    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let hook = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
        esc(&proj.path().join("t.jsonl")), esc(proj.path()),
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_skillforcer"))
        .arg("hook")
        .current_dir(proj.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(hook.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"permissionDecision\": \"deny\""), "stdout was: {stdout}");
    assert!(stdout.contains("tech-writing"));
}

#[test]
fn list_presets_runs() {
    let out = Command::new(env!("CARGO_BIN_EXE_skillforcer")).arg("list-presets").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("code-comments"));
}
```

- [ ] **Step 2: Run to verify** (deny path exercised end-to-end)

Run: `cargo test --test e2e`
Expected: PASS.

- [ ] **Step 3: Write `README.md`**

Concise: what skillforcer does, install (`cargo install --path .` then `skillforcer install`), a sample `.skillforcer.toml`, the freshness modes table, presets, `check`/`status`, and fail-open behavior. Keep it terse and present-tense.

- [ ] **Step 4: Run full suite + clippy; commit**

```bash
cargo test && cargo clippy --all-targets -- -D warnings
git add tests/ README.md
git commit -m "test: end-to-end hook smoke test; docs: README"
```

---

### Task 14: Configuration skill + plugin manifest

**Files:**
- Create: `.claude-plugin/plugin.json`
- Create: `skills/skillforcer-config/SKILL.md`

**Interfaces:** none (docs/packaging). The plugin ships **only** the skill; hooks are installed by the CLI.

- [ ] **Step 1: Create `.claude-plugin/plugin.json`**

```json
{
  "name": "skillforcer",
  "description": "Guidance for configuring skillforcer, which forces skills to load before governed writes.",
  "version": "0.1.0",
  "author": { "name": "skillforcer" }
}
```

- [ ] **Step 2: Create `skills/skillforcer-config/SKILL.md`**

Frontmatter + terse body (authored per the tech-writing skill's style). Must cover: installing the binary and running `skillforcer install`; the `.skillforcer.toml` schema (`[defaults]`, `[[rule]]`, `extends`, `path`, `content`, `requires` with `any_skill`/`all_skills` and the four freshness windows); presets (`skillforcer list-presets`); `combine_freshness`; fail-open; and `skillforcer check`/`status` for debugging. Frontmatter:

```markdown
---
name: skillforcer-config
description: Use when configuring skillforcer — writing or editing .skillforcer.toml rules, choosing freshness windows (session/minutes/turns/tokens), using presets, or installing/uninstalling its Claude Code hooks.
---
```

Body sections (write real prose, no placeholders): Overview; Install; Rule anatomy with a complete example; Freshness windows table; Presets; Debugging with `check`/`status`; Fail-open note.

- [ ] **Step 3: Validate JSON + skill frontmatter**

Run: `python -c "import json; json.load(open('.claude-plugin/plugin.json'))"`
Expected: no error. Confirm `SKILL.md` starts with the `---` frontmatter block.

- [ ] **Step 4: Commit**

```bash
git add .claude-plugin/ skills/
git commit -m "feat: config skill and plugin manifest"
```

---

## Self-Review

**Spec coverage:**
- §3 two hooks / transcript authority → Tasks 3, 8, 10. ✓
- §4 freshness modes (all four + AND) → Task 7. ✓
- §5 rule model, skill-set semantics, content extraction, presets → Tasks 3, 4, 5, 6. ✓
- §6 config layering → Task 4. ✓
- §7 deny output + fail-open → Tasks 2, 10. ✓
- §8 state store + cursor → Tasks 8, 9. ✓
- §9 CLI (hook/install/uninstall/check/status/list-presets) → Tasks 1, 10, 11, 12. ✓
- §10 module boundaries → mirrored in file structure. ✓
- §11 distribution: plugin ships skill only; binary via cargo → Tasks 13, 14. ✓
- §12 testing (unit/golden/integration) → per-task tests + Task 13 e2e. ✓
- §13 deps → Task 1. ✓
- §14 risks (fail-open, transcript rebuild, unknown shapes) → Tasks 8, 10. ✓

**Placeholder scan:** No "TBD"/"handle edge cases" left; each code step has real code. The `jiff` duration API note in Task 7 is a version-adaptation instruction, and the test pins the required behavior.

**Type consistency:** `Config`/`RuleDef`/`Requires`/`SkillSet`/`Windows`/`Combine` names are consistent across Tasks 4–12. `Cursor`/`SkillLoad`/`Now`/`WriteEvent`/`Decision` from `model` used consistently. `run_hook` signature identical in Task 10 definition and Task 13 usage.
