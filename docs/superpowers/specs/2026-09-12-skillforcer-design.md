# skillforcer — Design Spec

Date: 2026-09-12
Status: Approved for planning

## 1. Problem

An agent has a project skill (e.g. `tech-writing`) whose description alone is
insufficient — its body must be loaded for the guidance to be applied. Over a
long session, context rot means the skill body drifts out of the model's
working context, so writes that the skill governs (e.g. code comments) stop
following it. Re-reading the skill fixes the output, but nothing forces the
re-read at the moment it matters.

skillforcer forces it: before an agent may make a write that matches a
configured rule, the skill that governs that write must have been loaded
recently enough. Otherwise the write is denied with an instruction to load the
skill.

## 2. Scope

- In scope: a Rust CLI that acts as Claude Code hooks; a configurable rule
  engine; a bundled preset library; a self-installer; a skill documenting
  configuration; a plugin (this repo) that ships that skill.
- Out of scope for now: adapters for agents other than Claude (Codex, etc.) —
  the internal model is kept agent-agnostic so they can be added later, but no
  non-Claude code ships.
- Non-goal: shipping compiled binaries inside the plugin. The binary is
  distributed via `cargo install` / release artifacts; the plugin ships only
  the skill.

## 3. Core behavior

Two Claude Code hooks, both served by the same binary via `skillforcer hook`,
which reads the hook JSON on stdin and dispatches on `hook_event_name`:

- **PostToolUse, matcher `Skill`** — the *recorder*. When the `Skill` tool
  runs, record `{skill, timestamp, transcript cursor}` to the session state
  file.
- **PreToolUse, matcher `Write|Edit|MultiEdit|NotebookEdit`** — the *guard*.
  Evaluate rules against the pending write. If a rule fires and its required
  skill is not fresh, deny the write with a templated reason.

### 3.1 Transcript is the source of truth

The `Skill` tool is confirmed to appear in the session transcript as a
`tool_use` entry with `input.skill`, and every transcript line carries an ISO
`timestamp`; assistant messages carry token `usage`. The transcript path is
handed to every hook via `transcript_path`.

The docs do not guarantee that `PostToolUse` fires for the `Skill` tool.
Therefore the transcript is authoritative and the state file is a cache/fast
path. The guard MUST be able to reconcile against — or fully rebuild from — the
transcript, so detection is correct even if the recorder hook never fires.

To avoid re-scanning large transcripts on every write, state stores a **cursor**
(byte offset + turn index + cumulative token count). Incremental reads resume
from the cursor.

## 4. Freshness modes

A rule's `requires` block declares how recent a skill load must be. All four
modes are supported and may be combined:

- `session` — loaded at any point this session.
- `minutes = N` — within the last N minutes (wall clock, from timestamps).
- `turns = N` — within the last N turns (transcript message ordering).
- `tokens = N` — within the last N tokens (summed transcript `usage`).

`session` and `minutes` are answerable from the state file alone. `turns` and
`tokens` trigger a cursor-based transcript read.

When more than one window is declared, the combinator is **AND** by default
(the load must be within *every* declared window). Overridable per-rule and via
`[defaults] combine_freshness`.

## 5. Rule model

A rule is an AND of independent predicates plus a freshness requirement:

- **path** — extension/glob patterns (`globset`). Match against the write's
  target file path.
- **content** — a regex (`regex`) tested against the *written text*.
- **requires** — the skill set that must be fresh, plus a freshness spec.
- **extends** — names of presets merged in before local overrides.
- **message** — optional custom deny message template.

### 5.1 Skill-set semantics

`requires` names the skill set explicitly — never a bare `skills` key, which
reads ambiguously:

- `any_skill = ["a", "b"]` — satisfied if *at least one* is fresh.
- `all_skills = ["a", "b"]` — satisfied only if *all* are fresh.

Exactly one of the two is given per rule.

### 5.2 Content extraction per tool

The "written text" a `content` regex runs against depends on the tool:

- `Write` → `tool_input.content`
- `Edit` → `tool_input.new_string`
- `MultiEdit` → concatenation of each `edits[].new_string`
- `NotebookEdit` → `tool_input.new_source`

Exact field names are verified against live payloads during implementation
(`skillforcer check --stdin` and a debug logging path assist this); the guard
fails open on an unrecognized shape.

### 5.3 Presets

Presets are named, reusable predicate fragments (e.g. `code-comments`,
`markdown-headings`) that supply common `content` regexes and default `path`
globs. A rule `extends` one or more presets; the effective rule is
`merge(presets...) + local overrides` (local wins).

Presets ship embedded in the binary. The loader is structured so future sources
(a presets directory, or remote) can register additional presets without
changing the rule engine. `skillforcer list-presets` enumerates them.

## 6. Configuration

TOML, layered: `./.skillforcer.toml` (project, committable) merged over an
optional `~/.config/skillforcer/config.toml` (global defaults, resolved via the
`directories` crate). Project values win.

```toml
[defaults]
fail_open = true            # a skillforcer error/missing config never traps the user
combine_freshness = "all"   # all declared freshness windows must hold

[[rule]]
name = "comments-need-tech-writing"
extends = ["code-comments"]         # preset supplies content regex + default exts
path = ["src/**/*.rs", "**/*.ts"]   # augments/narrows the preset
requires = { any_skill = ["tech-writing:technical-writing"], minutes = 15 }
message = "Editing comments in {file}. Load {skills} first — context may have rotted."
```

Message template tokens: `{file}`, `{rule}`, `{skills}`, `{reason}` (e.g. "last
loaded 41m ago; window is 15m").

## 7. Deny output & failure mode

Deny is emitted as JSON on stdout:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"<message>"}}
```

The reason names the rule, the required skill(s), the freshness reason, and the
exact `Skill` invocation to run.

**Fail-open by default.** Any internal error, unreadable transcript, malformed
input, or missing config results in *allow* plus a note on stderr, so a
skillforcer bug cannot brick a session. `fail_open = false` opts into strict
(fail-closed) behavior.

## 8. State

Per-session JSON keyed by `session_id`, stored in a user state/cache directory
(via `directories`), never in the repo. Contents: recorded skill loads
(`{skill, timestamp, turn_index, token_count}`) and the transcript cursor. Old
sessions are pruned opportunistically on write.

## 9. CLI (argh)

- `skillforcer hook` — read hook JSON on stdin, dispatch on `hook_event_name`.
  Single command string for both hooks.
- `skillforcer install [--local | --user] [--dry-run]` — idempotently add the
  two hook entries to `.claude/settings.json` (default). Scaffold a commented
  `.skillforcer.toml` if absent. Record the binary's absolute path
  (`std::env::current_exe`). `--local` targets `.claude/settings.local.json`;
  `--user` targets `~/.claude/settings.json`. `--dry-run` prints the diff.
- `skillforcer uninstall [--local | --user]` — remove only the entries it added
  (tagged for safe identification).
- `skillforcer check <file> [--stdin]` — evaluate rules against a file or piped
  content; print which rules fire and why. Enables testing without Claude.
- `skillforcer status [--session ID]` — dump detected skill-load state and
  freshness for a session.
- `skillforcer list-presets` — list bundled presets.

## 10. Architecture & module boundaries

Agent-agnostic core with a thin per-agent adapter:

- **adapter (claude)** — parse hook stdin into a normalized model
  (`WriteEvent { path, content }`, `SkillLoad { name, when, turn, tokens }`),
  and render a `Decision` back into Claude's hook JSON. Only the Claude adapter
  ships; Codex/others slot in later.
- **config** — load, layer, and validate TOML; resolve `extends` against
  presets.
- **presets** — embedded library + registration surface for future sources.
- **rules** — evaluate `path` × `content` predicates against a `WriteEvent`.
- **freshness** — evaluate a `SkillLoad` set against a `requires` spec across
  the four modes.
- **state** — read/write per-session state; hold the transcript cursor.
- **transcript** — incremental, cursor-based reader of the JSONL transcript
  (skill loads, timestamps, turns, token usage); authoritative source and
  rebuild path.
- **install** — settings.json read/modify/write, idempotent, tagged entries;
  config scaffolding.
- **cli** — argh subcommand definitions dispatching to the above.

Each unit is testable in isolation against fixtures.

## 11. Distribution

- **Binary**: `cargo install skillforcer` or prebuilt release binaries.
  Compiled binaries are not shipped inside the plugin.
- **Plugin (this repo)**: ships **only the skill** documenting how to configure
  skillforcer. Hook registration is done by the CLI (`skillforcer install`),
  not by the plugin. Plugin manifest at `.claude-plugin/plugin.json`; skill at
  `skills/<name>/SKILL.md`.
- **Skill**: terse configuration reference (config schema, freshness modes,
  presets, troubleshooting), authored in the `tech-writing` style.

## 12. Testing

- Unit: rule matching (path × content × preset merge), freshness math per mode
  and the AND combinator, deny-JSON shape, skill-set `any`/`all` semantics.
- Golden: recorded hook-stdin fixtures (real `Write`/`Edit`/`MultiEdit`/`Skill`
  payloads) asserted to their decisions.
- Integration: `check` and `install` against temp dirs; install idempotency;
  fail-open on malformed input; transcript rebuild when state is absent.

## 13. Dependencies (initial)

`argh` (CLI), `serde` + `serde_json` (hook/state IO), `toml` (config), `regex`
(content), `globset` (paths), `directories` (config/state paths). Timestamp
parsing via a minimal ISO-8601 path (`jiff` or `time`, decided at
implementation). Error handling via `anyhow`/`thiserror`.

## 14. Risks & mitigations

- **`Skill` PostToolUse may not fire** — transcript is authoritative; state is a
  cache; guard can rebuild from transcript. (Section 3.1)
- **Tool input field names undocumented** — verified against live payloads;
  guard fails open on unknown shapes. (Section 5.2)
- **Large transcripts** — cursor-based incremental reads; `session`/`minutes`
  avoid transcript reads entirely. (Sections 3.1, 4)
- **Skillforcer bug traps the user** — fail-open default. (Section 7)
- **Multiple hook sources / most-restrictive-wins** — skillforcer only denies
  when a rule genuinely fires; fail-open keeps it from over-denying.

## 15. Deferred (YAGNI)

- Non-Claude adapters (Codex, etc.).
- Remote/external preset sources.
- `warn-only` enforcement mode.
- Override tokens / bypass surface (loading the skill is the intended escape).
