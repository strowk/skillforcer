# skillforcer — Codex CLI Support Design Spec

Date: 2026-09-12
Status: Approved for planning

## 1. Problem & goal

skillforcer today enforces skill freshness only for Claude Code. OpenAI Codex
CLI (openai/codex) now has a lifecycle hook system directly comparable to
Claude Code's — a synchronous `PreToolUse` hook can deny a tool call with a
reason the model sees, using the same `hookSpecificOutput.permissionDecision`
JSON shape — and native Agent Skills (agentskills.io `SKILL.md` standard).

Goal: **full parity** for Codex — deny a governed write until the required
skill has been loaded recently enough, with all four freshness modes working —
plus publishing the `skillforcer-config` skill so Codex users can install it
quickly.

## 2. Scope

- In scope: a Codex adapter in the existing agent-agnostic core; a Codex
  rollout (transcript) scanner; harness auto-detection and `--codex`/`--claude`
  flags in `install`/`uninstall`; a Codex plugin (skill-only) publishing
  `skillforcer-config`; README updates covering both harnesses.
- Out of scope: shipping hooks inside the Codex plugin bundle (hook
  registration stays with `skillforcer install`, so the user sees what changes
  on their machine); gating raw-shell writes; further harnesses.

## 3. Verified Codex integration surface

Researched September 2026 against official docs (developers.openai.com/codex),
the openai/codex source, and community guides. Key facts the design rests on:

- **Hooks** (GA since ~May 2026, Codex ~0.150.x): configured in
  `~/.codex/hooks.json` or `<repo>/.codex/hooks.json`; event → matcher groups
  (regex on tool name) → handlers. A synchronous `"command"` handler on
  `PreToolUse` receives JSON on stdin (`session_id`, `transcript_path`, `cwd`,
  `hook_event_name`, `tool_name`, `tool_input`, plus extra fields) and denies
  via `{"hookSpecificOutput": {"hookEventName": "PreToolUse",
  "permissionDecision": "deny", "permissionDecisionReason": "..."}}` — the
  reason is fed back to the model. `async: true` handlers cannot block.
- **Trust gate**: non-managed hooks require hash-based trust review (`/hooks`
  command in Codex). In `codex exec` automation, untrusted hooks are
  **silently skipped**.
- **Writes** go through the `apply_patch` tool (patch envelope: `*** Begin
  Patch`, `*** Add File:`, `*** Update File:` hunks), sometimes invoked via the
  shell tool (`tool_name: "Bash"`) with the patch embedded in the command.
- **Skills**: no `Skill` tool. Explicit invocation (`$skill-name`) appends a
  `SkillInstructions` item to the rollout — it produces **no tool call**, so
  hooks never see it. Implicit invocation is the model reading `SKILL.md` with
  its shell tool; Codex's own extension API detects this by path-matching.
- **Rollouts**: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`
  (default `CODEX_HOME` is `~/.codex`). JSONL of typed items: `SessionMeta`
  (session id), `ResponseItem` (messages, tool calls with args),
  `TurnContext`, `TokenUsageRecord` (per-turn tokens), timestamps per line.
  Format is **not documented as stable**. Hooks hand the adapter
  `transcript_path` directly, so no path discovery is needed.
- **Skill locations**: `.agents/skills` (project, scanned cwd→repo root),
  `~/.agents/skills` (user), `~/.codex/skills` (legacy, still scanned).
- **Distribution**: Codex has a plugin system (marketplaces added from GitHub,
  `/plugins` in the CLI) and a built-in `$skill-installer` skill that installs
  a skill directory from any GitHub repo + path.

## 4. Core behavior (Codex)

Same two-hook shape as Claude, served by the same binary:

- **PreToolUse, matcher `apply_patch|Edit|Write|Bash`** — the *guard*. Extract
  `WriteEvent`s from the pending call; evaluate rules; deny with a templated
  reason if a matching rule's skill is not fresh.
- **PostToolUse, matcher `Bash|apply_patch`** — the *recorder*. If the
  completed shell command references a `SKILL.md` path, record an implicit
  skill load (named after the SKILL.md's parent directory) at the current
  cursor.

As on the Claude side, the transcript (rollout) is the source of truth and the
recorder is a fast path. The rollout scanner is additionally **required** for
correctness here: explicit `$skill` invocations appear only in the rollout,
never as tool calls, so a hooks-only recorder would wrongly deny users who
invoke skills the official way.

## 5. Codex adapter (`src/adapter/codex.rs`)

- **Hook parsing**: light variant of the Claude parser — Codex uses the same
  field names; unknown extra fields are ignored. Malformed input fails open.
- **Write extraction**: an `apply_patch` envelope parser producing one
  `WriteEvent { path, content }` per touched file; `content` is the added
  lines (what `content` regexes run against). For `Bash` calls, detect an
  embedded `apply_patch` invocation and parse its patch; any other shell
  command yields no events (allow). Unparseable patches fail open.
- **Deny rendering**: identical `hookSpecificOutput` JSON to the Claude
  adapter.
- **Skill-load detection (shared with the scanner)**: a command string
  referencing a path ending in `SKILL.md` counts as an implicit load of the
  skill named by that file's parent directory — the same heuristic Codex uses
  internally. Misses only make freshness conservative (an extra deny, never a
  wrong allow).

## 6. Codex rollout scanner (`src/transcript/codex.rs`)

Same contract as the existing `transcript::scan`: read from a byte-offset
cursor, return new `SkillLoad`s plus an updated cursor, so `SessionState`,
`Store`, and `refresh_state` are reused untouched. Per line:

- `SkillInstructions` items → explicit load, named from the item's skill
  path/name.
- Tool-call items whose shell command references a `SKILL.md` path → implicit
  load (backfills anything the live recorder missed).
- `TurnContext` → advance the turn counter; `TokenUsageRecord` → advance the
  token counter. These feed the `turns`/`tokens` freshness modes.

Because the rollout format is not stability-guaranteed, the parser is
deliberately tolerant: unknown item types are skipped; an unparseable line is
logged to stderr and skipped; a scan failure leaves prior state in place —
never a deny. Field names are pinned against hand-authored fixtures derived
from the documented format and upstream source (see §11); no live Codex
capture is required.

## 7. Core changes (harness-agnostic)

- `run_hook` takes the harness (from `--harness`) and dispatches to the
  matching adapter; it evaluates a **list** of `WriteEvent`s per call (an
  `apply_patch` may touch several files), denying on the first unsatisfied
  match.
- **Suffix matching** in freshness evaluation: rule skill `A` matches recorded
  load `B` if `A == B` or `tail(A) == tail(B)`, where `tail` is the segment
  after the last `:` or `/`. So `tech-writing:technical-writing` in a rule
  matches a Codex load of `technical-writing` and vice versa. One rule works
  across both harnesses; the small collision risk between same-named skills is
  accepted. Applies uniformly to both harnesses.
- **Per-harness default deny message**: the built-in fallback becomes
  harness-aware — Claude keeps `Load it now: Skill("{skills}")`; Codex says
  `Load it now: invoke ${skills} or read its SKILL.md`. Custom `message`
  templates are unchanged.

## 8. CLI & harness detection

- `skillforcer hook --harness <claude|codex>` — defaults to `claude`, so
  already-installed Claude hooks keep working. The installer writes the flag
  explicitly; the binary never sniffs the input format.
- `install`/`uninstall` gain `--claude` and `--codex` switches to force a
  harness. With neither: detect from the project root — `.claude/` → Claude;
  `.codex/` or `.agents/skills/` → Codex; **both present → install both**;
  neither → Claude (fallback). `--user` composes (`--user --codex` targets
  `~/.codex/hooks.json`). `--local` stays Claude-only and errors if combined
  with `--codex`.

## 9. Installer (Codex side)

`install --codex` merges two entries into `.codex/hooks.json` (project) or
`~/.codex/hooks.json` (`--user`), creating the file if absent: a `PreToolUse`
group (matcher `apply_patch|Edit|Write|Bash`) and a `PostToolUse` group
(matcher `Bash|apply_patch`), each with a synchronous command handler running
`<exe> hook --harness codex`. Same guarantees as the Claude installer:
idempotent, preserves other tools' entries, `uninstall` removes only ours.
`.skillforcer.toml` scaffolding is shared.

After installing, print a prominent trust-gate note: the hook must be approved
via `/hooks` in Codex before it enforces anything, and untrusted hooks are
silently skipped in `codex exec` — without trust, skillforcer is quietly off.

The exact hooks.json schema is verified against current Codex documentation at
implementation time (docs lag the engine; version-gated details are expected).

## 10. Publishing `skillforcer-config` for Codex users

One skill, one source: `skills/skillforcer-config/SKILL.md` stays the single
copy and becomes harness-aware — shared `.skillforcer.toml` reference plus
Codex-specific notes (`skillforcer install --codex`, the trust gate, skill
locations). Nothing is committed for in-repo auto-pickup; this is purely about
publishing to users' own Codex installs.

Three documented install routes, mirroring the README's Claude section:

1. **Codex plugin** (primary): a skill-only plugin manifest in this repo — no
   hooks bundled. Users add the repo as a marketplace and install via
   `/plugins`, the analogue of `/plugin marketplace add strowk/skillforcer`.
   Exact manifest schema and commands are version-gated; pinned at
   implementation.
2. **`$skill-installer` one-liner**: `$skill-installer install the skill from
   strowk/skillforcer, path skills/skillforcer-config`.
3. **Manual**: clone and `cp -r skills/skillforcer-config ~/.agents/skills/`.

## 11. Testing

Fixture-driven, following the existing style. Fixtures are **hand-authored**
from the documented formats and upstream source research — no live Codex
capture. The tolerant parser plus fail-open semantics bound the cost of any
divergence between fixtures and reality (worst case: a load goes undetected
and the deny message tells the model how to fix it).

- Unit: `apply_patch` envelope parser (add/update/multi-file; patch embedded
  in a shell command); Codex hook stdin parsing; rollout scanner (explicit
  `SkillInstructions`, implicit SKILL.md read, turn/token cursor advance,
  unknown-line tolerance); suffix matching; hooks.json merge / idempotence /
  uninstall; harness detection heuristic.
- End-to-end smoke mirroring the Claude one: deny → simulated skill load →
  retried write allowed, through `run_hook` with Codex fixtures.

## 12. Documentation

README states once, up front, that both **Claude Code and Codex CLI** are
supported; elsewhere it speaks of the "coding agent" generically (or
"Claude/Codex" where a concrete name is clearer) instead of saying "Claude"
everywhere — without overcomplicating and without implying more than the two
supported harnesses. It gains: a Codex install section (auto-detection,
`--codex`/`--claude` flags, the trust warning in bold) and the three
skill-install routes for Codex users.

## 13. Risks & mitigations

- **Rollout format is not stable** — tolerant parser (skip unknown, log +
  skip unparseable, fail open); occasional maintenance expected. (§6)
- **Hand-authored fixtures may diverge from real payloads** — fail-open
  semantics; worst case is a conservative deny with a self-explanatory
  remedy. (§11)
- **Hook trust gate silently disables enforcement** — loud installer warning;
  documented in README. (§9)
- **Implicit load detection is heuristic** — misses are conservative (extra
  deny), never a wrong allow. (§5)
- **hooks.json / plugin schema version drift** — details pinned at
  implementation against current docs; installer only touches its own tagged
  entries. (§9, §10)

## 14. Deferred (YAGNI)

- Shipping hooks inside a Codex plugin bundle.
- Gating raw-shell writes (`echo >`, `sed -i`) — same gap as Claude Code.
- Warn-only mode; further harnesses.
