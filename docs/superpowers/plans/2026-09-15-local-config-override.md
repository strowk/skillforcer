# `.skillforcer.local.toml` Local Override Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a highest-priority, per-developer `.skillforcer.local.toml` layer that merges over `.skillforcer.toml`, including an `enabled = false` flag to silence an inherited rule.

**Architecture:** Extend the existing layered merge in `src/config.rs` (currently global → project) with a third top layer (local). Rules carry a new `enabled` flag; a disabled rule acts as a tombstone that overrides its same-named inherited rule and is dropped after all layers merge. `install` appends the local file to `.gitignore`.

**Tech Stack:** Rust, `serde`, `toml`, `anyhow`. Tests via `cargo test` (unit tests in-module, e2e in `tests/e2e.rs`).

**Spec:** No separate spec file — this plan implements the in-chat bounded design approved on 2026-09-15 (precedence global < project < local; per-rule `enabled = false` disable; gitignore the local file on install).

## Global Constraints

- Precedence order is **global → project → local**, lowest to highest. Later layer wins for `[defaults]` fields and same-named rules.
- `.skillforcer.local.toml` uses the **same schema** as `.skillforcer.toml`.
- A rule with `enabled = false` is a tombstone: it needs only `name`, must skip the skill/window validation, and is removed from the final config after merging.
- A rule that is enabled (default) and lacks `requires` must still error, exactly as today.
- skillforcer stays opt-in: with neither a project nor a local config file present, `load` returns `Config::default()` (no rules) — unchanged behavior for the no-config case.
- Follow existing code patterns; do not restructure unrelated code.

---

### Task 1: Add `enabled` flag and optional `requires` to config types

Add the `enabled` field to the rule types and teach `convert_rule` to treat a disabled rule as a tombstone that skips validation. No layering change yet — a disabled rule is parsed and carried, not yet filtered.

**Files:**
- Modify: `src/config.rs` (`RuleDef`, `RawRule`, `convert_rule`, `extends_tests` literals)
- Test: `src/config.rs` (in-module `tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `RuleDef` gains `pub enabled: bool`.
  - `RawRule` gains `enabled: Option<bool>` and changes `requires` from `RawRequires` to `Option<RawRequires>`.
  - `convert_rule(r: RawRule) -> Result<RuleDef>` unchanged signature; behavior: when `enabled == Some(false)`, returns a tombstone `RuleDef { enabled: false, .. }` with placeholder `requires` and no validation; otherwise requires `requires` to be present and validates as before.

- [ ] **Step 1: Write the failing test**

Add to the in-module `tests` module in `src/config.rs`:

```rust
#[test]
fn disabled_rule_parses_without_requires() {
    // A pure-disable stanza needs only a name; validation is skipped.
    let toml = r#"[[rule]]
        name = "x"
        enabled = false"#;
    let cfg = parse_str(toml, None).unwrap();
    // Not yet filtered in this task — it is present but marked disabled.
    assert_eq!(cfg.rules.len(), 1);
    assert_eq!(cfg.rules[0].name, "x");
    assert!(!cfg.rules[0].enabled);
}

#[test]
fn enabled_rule_missing_requires_still_errors() {
    let toml = r#"[[rule]]
        name = "x""#;
    assert!(parse_str(toml, None).is_err());
}

#[test]
fn enabled_defaults_true() {
    let cfg = parse_str(SAMPLE, None).unwrap();
    assert!(cfg.rules[0].enabled);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::tests::disabled_rule_parses_without_requires config::tests::enabled_rule_missing_requires_still_errors config::tests::enabled_defaults_true`
Expected: FAIL to compile (`enabled` field missing; `requires` not optional).

- [ ] **Step 3: Add `enabled` to `RuleDef`**

In `src/config.rs`, add the field to `RuleDef` (place it right after `name`):

```rust
#[derive(Debug, Clone)]
pub struct RuleDef {
    pub name: String,
    pub enabled: bool,
    pub extends: Vec<String>,
    pub path: Vec<String>,
    pub content: Option<String>,
    pub requires: Requires,
    pub message: Option<String>,
}
```

- [ ] **Step 4: Make `RawRule.requires` optional and add `enabled`**

```rust
#[derive(Debug, Deserialize)]
struct RawRule {
    name: String,
    #[serde(default)]
    extends: Vec<String>,
    #[serde(default)]
    path: Vec<String>,
    content: Option<String>,
    requires: Option<RawRequires>,
    message: Option<String>,
    enabled: Option<bool>,
}
```

- [ ] **Step 5: Rewrite `convert_rule` to handle the tombstone**

Replace the body of `convert_rule` with:

```rust
fn convert_rule(r: RawRule) -> Result<RuleDef> {
    let enabled = r.enabled.unwrap_or(true);

    // A disabled rule is a tombstone: it only carries a name (plus whatever
    // else was written) and overrides its same-named inherited rule. It never
    // runs, so it skips skill/window validation and gets a placeholder Requires.
    if !enabled {
        return Ok(RuleDef {
            name: r.name,
            enabled: false,
            extends: r.extends,
            path: r.path,
            content: r.content,
            requires: Requires {
                skills: SkillSet::Any(Vec::new()),
                windows: Windows::default(),
            },
            message: r.message,
        });
    }

    let req = r
        .requires
        .ok_or_else(|| anyhow::anyhow!("rule '{}': requires is required", r.name))?;

    let skills = match (req.any_skill, req.all_skills) {
        (Some(_), Some(_)) => bail!("rule '{}': set only one of any_skill/all_skills", r.name),
        (Some(a), None) => SkillSet::Any(a),
        (None, Some(a)) => SkillSet::All(a),
        (None, None) => bail!("rule '{}': one of any_skill/all_skills is required", r.name),
    };
    let windows = Windows {
        session: req.session,
        minutes: req.minutes,
        turns: req.turns,
        tokens: req.tokens,
    };
    if !windows.session
        && windows.minutes.is_none()
        && windows.turns.is_none()
        && windows.tokens.is_none()
    {
        bail!(
            "rule '{}': at least one freshness window (session/minutes/turns/tokens) is required",
            r.name
        );
    }
    Ok(RuleDef {
        name: r.name,
        enabled: true,
        extends: r.extends,
        path: r.path,
        content: r.content,
        requires: Requires { skills, windows },
        message: r.message,
    })
}
```

- [ ] **Step 6: Fix `RuleDef` literals in `extends_tests`**

Both `RuleDef { ... }` literals in the `extends_tests` module lack the new field. Add `enabled: true,` right after the `name:` line in each (the test named `extends_fills_content_from_preset` and `unknown_preset_errors`). Example:

```rust
let rule = RuleDef {
    name: "c".into(),
    enabled: true,
    extends: vec!["code-comments".into()],
    // ...unchanged...
};
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test --lib config`
Expected: PASS (all config tests, including the three new ones and the untouched existing ones).

- [ ] **Step 8: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add enabled flag with tombstone convert_rule"
```

---

### Task 2: Add the local layer to `parse_str` and drop disabled rules

Add the third layer parameter, merge it highest, and `retain` only enabled rules after merging. Update existing 2-arg call sites.

**Files:**
- Modify: `src/config.rs` (`parse_str`, new `parse_layer` helper, existing test call sites)
- Test: `src/config.rs` (in-module `tests`)

**Interfaces:**
- Consumes: `RuleDef.enabled` from Task 1.
- Produces:
  - `parse_str(project_toml: &str, global_toml: Option<&str>, local_toml: Option<&str>) -> Result<Config>` — the new 3-arg signature. Precedence global < project < local.
  - `fn parse_layer(toml_str: Option<&str>, label: &str) -> Result<RawConfig>` — private helper.

- [ ] **Step 1: Write the failing tests**

Add to the in-module `tests` module in `src/config.rs`:

```rust
#[test]
fn local_overrides_project_defaults() {
    let project = r#"[defaults]
        fail_open = true"#;
    let local = r#"[defaults]
        fail_open = false"#;
    let cfg = parse_str(project, None, Some(local)).unwrap();
    assert!(!cfg.defaults.fail_open);
}

#[test]
fn local_disables_inherited_project_rule() {
    let project = r#"[[rule]]
        name = "comments"
        requires = { any_skill = ["a"], session = true }"#;
    let local = r#"[[rule]]
        name = "comments"
        enabled = false"#;
    let cfg = parse_str(project, None, Some(local)).unwrap();
    assert!(cfg.rules.is_empty(), "disabled rule should be dropped");
}

#[test]
fn local_rule_replaces_same_named_project_rule() {
    let project = r#"[[rule]]
        name = "dup"
        requires = { all_skills = ["p"], session = true }"#;
    let local = r#"[[rule]]
        name = "dup"
        requires = { any_skill = ["l"], session = true }"#;
    let cfg = parse_str(project, None, Some(local)).unwrap();
    assert_eq!(cfg.rules.len(), 1);
    assert!(matches!(&cfg.rules[0].requires.skills, SkillSet::Any(v) if v == &["l"]));
}

#[test]
fn local_adds_new_rule() {
    let project = r#"[[rule]]
        name = "a"
        requires = { any_skill = ["x"], session = true }"#;
    let local = r#"[[rule]]
        name = "b"
        requires = { any_skill = ["y"], session = true }"#;
    let cfg = parse_str(project, None, Some(local)).unwrap();
    assert_eq!(cfg.rules.len(), 2);
}

#[test]
fn precedence_global_project_local() {
    let global = r#"[defaults]
        fail_open = true
        combine_freshness = "all""#;
    let project = r#"[defaults]
        combine_freshness = "any""#;
    let local = r#"[defaults]
        fail_open = false"#;
    let cfg = parse_str(project, Some(global), Some(local)).unwrap();
    assert!(!cfg.defaults.fail_open); // from local
    assert_eq!(cfg.defaults.combine_freshness, Combine::Any); // from project (global had "all")
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config`
Expected: FAIL to compile (`parse_str` takes 2 args, not 3).

- [ ] **Step 3: Add the `parse_layer` helper**

Add near `parse_str` in `src/config.rs`:

```rust
fn parse_layer(toml_str: Option<&str>, label: &str) -> Result<RawConfig> {
    match toml_str {
        Some(t) => toml::from_str(t).with_context(|| format!("parsing {label} config")),
        None => Ok(RawConfig::default()),
    }
}
```

- [ ] **Step 4: Rewrite `parse_str` with three layers and a final `retain`**

```rust
pub fn parse_str(
    project_toml: &str,
    global_toml: Option<&str>,
    local_toml: Option<&str>,
) -> Result<Config> {
    let global = parse_layer(global_toml, "global")?;
    let project: RawConfig = toml::from_str(project_toml).context("parsing project config")?;
    let local = parse_layer(local_toml, "local")?;

    let defaults = Defaults {
        fail_open: local
            .defaults
            .fail_open
            .or(project.defaults.fail_open)
            .or(global.defaults.fail_open)
            .unwrap_or(true),
        combine_freshness: local
            .defaults
            .combine_freshness
            .or(project.defaults.combine_freshness)
            .or(global.defaults.combine_freshness)
            .unwrap_or_default(),
    };

    let mut rules: Vec<RuleDef> = Vec::new();
    for raw in global
        .rules
        .into_iter()
        .chain(project.rules)
        .chain(local.rules)
    {
        let converted = convert_rule(raw)?;
        if let Some(existing) = rules.iter_mut().find(|x| x.name == converted.name) {
            *existing = converted; // later layer wins on same name
        } else {
            rules.push(converted);
        }
    }
    // Drop tombstones after all layers merge, so a disabled stanza in any layer
    // removes the inherited rule of that name.
    rules.retain(|r| r.enabled);
    Ok(Config { defaults, rules })
}
```

- [ ] **Step 5: Update existing 2-arg call sites**

The existing in-module tests call `parse_str(..)` with two args. Update each to pass `None` as the third argument:
- `parses_rule_and_requires`: `parse_str(SAMPLE, None, None)`
- `rejects_both_skill_keys`: `parse_str(bad, None, None)`
- `rejects_no_freshness`: `parse_str(bad, None, None)`
- `project_overrides_global_defaults`: `parse_str(project, Some(global), None)`
- `rejects_when_neither_skill_key_set`: `parse_str(bad, None, None)`
- `project_rule_replaces_same_named_global_rule`: `parse_str(project, Some(global), None)`

(These are the only `parse_str` call sites in the file; `load` is updated in Task 3. Verify with `grep -n "parse_str(" src/config.rs` that none remain with two args.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib config`
Expected: PASS (new layering tests plus all updated existing tests).

- [ ] **Step 7: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): merge .skillforcer.local.toml as highest layer"
```

---

### Task 3: `load` reads `.skillforcer.local.toml`

Wire the file read into `load` and keep skillforcer opt-in.

**Files:**
- Modify: `src/config.rs` (`load`)
- Test: `src/config.rs` (in-module `tests`)

**Interfaces:**
- Consumes: `parse_str` 3-arg from Task 2.
- Produces: `load(project_dir: &Path, global_path: Option<&Path>) -> Result<Config>` — unchanged signature; now also reads `<project_dir>/.skillforcer.local.toml`.

- [ ] **Step 1: Write the failing test**

Add to the in-module `tests` module in `src/config.rs` (uses `tempfile`, already a dev-dependency — confirm it is under `[dev-dependencies]` in `Cargo.toml`; it is used by `install.rs` tests):

```rust
#[test]
fn load_applies_local_over_project() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".skillforcer.toml"),
        r#"[[rule]]
        name = "comments"
        requires = { any_skill = ["a"], session = true }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".skillforcer.local.toml"),
        r#"[[rule]]
        name = "comments"
        enabled = false"#,
    )
    .unwrap();
    let cfg = load(dir.path(), None).unwrap();
    assert!(cfg.rules.is_empty());
}

#[test]
fn load_no_config_files_is_default() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = load(dir.path(), None).unwrap();
    assert!(cfg.rules.is_empty());
    assert!(cfg.defaults.fail_open);
}
```

Add `use super::*;` is already present at top of the `tests` module — no import change needed for `tempfile` since it is referenced by full path.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config::tests::load_applies_local_over_project`
Expected: FAIL (`load` does not read the local file; rule is not dropped).

- [ ] **Step 3: Rewrite `load`**

Replace the `load` function body:

```rust
pub fn load(project_dir: &Path, global_path: Option<&Path>) -> Result<Config> {
    let project_toml = std::fs::read_to_string(project_dir.join(".skillforcer.toml")).ok();
    let local_toml = std::fs::read_to_string(project_dir.join(".skillforcer.local.toml")).ok();

    // Opt-in: with neither a project nor a local config, do nothing.
    if project_toml.is_none() && local_toml.is_none() {
        return Ok(Config::default());
    }

    let global_toml = global_path.and_then(|p| std::fs::read_to_string(p).ok());
    parse_str(
        project_toml.as_deref().unwrap_or(""),
        global_toml.as_deref(),
        local_toml.as_deref(),
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): load reads .skillforcer.local.toml"
```

---

### Task 4: Gitignore the local file on install

Add an idempotent `ensure_gitignored` helper and call it from the install command.

**Files:**
- Modify: `src/install.rs` (new `LOCAL_CONFIG_NAME` const + `ensure_gitignored`)
- Modify: `src/cli.rs` (install branch, after the scaffold_config block, ~line 233)
- Test: `src/install.rs` (in-module `tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn ensure_gitignored(project_dir: &Path) -> Result<bool>` — appends `.skillforcer.local.toml` to `<project_dir>/.gitignore` (creating the file if absent), returns `true` when it added the entry, `false` if already present. Idempotent.

- [ ] **Step 1: Write the failing tests**

Add to the in-module `tests` module in `src/install.rs`:

```rust
#[test]
fn ensure_gitignored_creates_and_is_idempotent() {
    let proj = tempfile::tempdir().unwrap();
    assert!(ensure_gitignored(proj.path()).unwrap());
    assert!(!ensure_gitignored(proj.path()).unwrap());
    let body = std::fs::read_to_string(proj.path().join(".gitignore")).unwrap();
    assert_eq!(
        body.lines().filter(|l| l.trim() == ".skillforcer.local.toml").count(),
        1
    );
}

#[test]
fn ensure_gitignored_appends_to_existing() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(proj.path().join(".gitignore"), "/target\n").unwrap();
    assert!(ensure_gitignored(proj.path()).unwrap());
    let body = std::fs::read_to_string(proj.path().join(".gitignore")).unwrap();
    assert!(body.contains("/target"));
    assert!(body.contains(".skillforcer.local.toml"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib install::tests::ensure_gitignored_creates_and_is_idempotent install::tests::ensure_gitignored_appends_to_existing`
Expected: FAIL to compile (`ensure_gitignored` not defined).

- [ ] **Step 3: Add the const and helper to `src/install.rs`**

Near the top-level consts (after `POST_MATCHER_CODEX`), add:

```rust
pub const LOCAL_CONFIG_NAME: &str = ".skillforcer.local.toml";
```

Add the function next to `scaffold_config`:

```rust
/// Ensure the personal `.skillforcer.local.toml` is git-ignored. Creates
/// `.gitignore` if absent, appends the entry only when missing. Idempotent;
/// returns true if it added the entry.
pub fn ensure_gitignored(project_dir: &Path) -> Result<bool> {
    let path = project_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == LOCAL_CONFIG_NAME) {
        return Ok(false);
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(LOCAL_CONFIG_NAME);
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(true)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib install`
Expected: PASS.

- [ ] **Step 5: Wire it into the install command**

In `src/cli.rs`, in the `Sub::Install` branch, inside the existing `if !c.dry_run { ... }` block (right after the scaffold_config `println!`, before the closing brace at ~line 233), add:

```rust
                if crate::install::ensure_gitignored(&cwd)? {
                    println!("added {} to .gitignore", crate::install::LOCAL_CONFIG_NAME);
                }
```

- [ ] **Step 6: Verify the crate builds**

Run: `cargo build`
Expected: builds clean, no warnings introduced.

- [ ] **Step 7: Commit**

```bash
git add src/install.rs src/cli.rs
git commit -m "feat(install): gitignore .skillforcer.local.toml"
```

---

### Task 5: End-to-end test — local disable lets a governed write through

Prove the whole path: a project rule that would deny a write is silenced by a local file, and the retried write passes.

**Files:**
- Test: `tests/e2e.rs` (new test `local_disable_allows_write`)

**Interfaces:**
- Consumes: the built binary via `CARGO_BIN_EXE_skillforcer` (same spawn pattern as `hook_denies_uncovered_comment_write`).
- Produces: nothing (test only).

- [ ] **Step 1: Write the failing test**

Add to `tests/e2e.rs`:

```rust
#[test]
fn local_disable_allows_write() {
    let proj = tempfile::tempdir().unwrap();
    // Project rule would deny a comment write with no skill loaded.
    std::fs::write(
        proj.path().join(".skillforcer.toml"),
        r#"[defaults]
        fail_open = false
        [[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} first""#,
    )
    .unwrap();
    // Local file disables it.
    std::fs::write(
        proj.path().join(".skillforcer.local.toml"),
        r#"[[rule]]
        name = "comments"
        enabled = false"#,
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
    // No matching rule remains → allow → empty stdout (no deny JSON).
    assert!(!stdout.contains("deny"), "expected allow, got: {stdout}");
}
```

- [ ] **Step 2: Run test to verify it fails or passes for the right reason**

Run: `cargo test --test e2e local_disable_allows_write`
Expected: PASS once Tasks 1–3 are merged (the config path already supports disable). If this is executed before Tasks 1–3, it FAILS with a `deny` in stdout. Confirm it PASSES on the current branch state.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e.rs
git commit -m "test(e2e): local disable lets governed write through"
```

---

### Task 6: Document the local override

Add docs to the README config section and the `skillforcer-config` skill.

**Files:**
- Modify: `README.md` (config section, around line 137 "Rules live in `.skillforcer.toml`…")
- Modify: `skills/skillforcer-config/SKILL.md` (rules section, around line 51)

**Interfaces:** none (docs only).

- [ ] **Step 1: Read the current doc sections**

Read `README.md` lines 130–160 and `skills/skillforcer-config/SKILL.md` lines 30–60 to match existing tone and wording. Use the tech-writing skill conventions: terse, present tense, no filler.

- [ ] **Step 2: Add a README paragraph**

After the existing sentence describing project/global layering (README ~line 137), add a short paragraph:

```markdown
### Personal overrides

`.skillforcer.local.toml` is an optional per-developer file layered **over**
`.skillforcer.toml` (precedence: global → project → local). It uses the same
schema. Use it to tweak rules for your own machine without committing the
change — `install` adds it to `.gitignore`. Set `enabled = false` on a rule to
silence one inherited from the committed config:

    [[rule]]
    name = "comments-need-tech-writing"
    enabled = false
```

- [ ] **Step 3: Add a SKILL.md note**

In `skills/skillforcer-config/SKILL.md`, in the rules section, add a sentence:

```markdown
A personal `.skillforcer.local.toml` (git-ignored, added by `install`) layers
over `.skillforcer.toml` with the same schema and highest precedence. Add
`enabled = false` to a rule there to silence an inherited rule locally.
```

- [ ] **Step 4: Verify docs render / no broken references**

Run: `grep -n "skillforcer.local.toml" README.md skills/skillforcer-config/SKILL.md`
Expected: entries present in both files.

- [ ] **Step 5: Commit**

```bash
git add README.md skills/skillforcer-config/SKILL.md
git commit -m "docs: document .skillforcer.local.toml personal override"
```

---

## Final Verification

- [ ] Run the full suite: `cargo test`
- [ ] Run: `cargo build --release` (no new warnings)
- [ ] Confirm `grep -rn "parse_str(" src/` shows only 3-arg calls.
