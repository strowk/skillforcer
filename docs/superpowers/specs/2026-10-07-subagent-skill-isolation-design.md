# Subagent Skill Isolation — Design Spec

Date: 2026-10-07
Status: Approved for planning
Scope: Claude Code only (core stays agent-agnostic; Codex deferred)

## 1. Problem

skillforcer scopes all state to `session_id`. In Claude Code a root session
and every subagent it spawns (via the Task tool) **share one `session_id`**,
but each runs in its own isolated context window: a skill loaded in one window
is never in another. The docs confirm skills do not cross the boundary — a root
load is not inherited by a subagent, and a subagent load does not persist to the
root or to sibling subagents.

Keying freshness by `session_id` therefore collapses N independent context
windows into one freshness record, producing two false-positive failure modes:

- **Subagent load leaks out.** A subagent loads the skill; the shared record now
  reads "fresh" for the root and for later subagents, none of which have the
  skill in context. Their governed writes pass unguarded.
- **Root load does not reach the subagent.** The root loads the skill; the
  subagent's governed write reads the shared record as "fresh" and passes, yet
  the skill was never in the subagent's context.

Both are the same bug in opposite directions: freshness is tracked per session,
but skill context is per context window.

### 1.1 Grounding (verified in live sessions)

Captured from real subagent hook payloads and on-disk transcripts:

- `session_id` is shared root↔subagent. `agent_id` (unique per subagent
  instance) and `agent_type` are present on `SubagentStart`, `PreToolUse`,
  `PostToolUse`, and `SubagentStop`. Root-session events carry no `agent_id`.
- `PostToolUse` with matcher `Skill` **fires inside subagents**, carrying the
  subagent's `agent_id` and `tool_input.skill`. The recorder works per-context.
- Inside a subagent, `PreToolUse.transcript_path` points at the **root**
  transcript (`<session>.jsonl`); only `SubagentStop` carries the subagent file
  (`agent_transcript_path`). The subagent's own transcript lives at
  `<session>/subagents/agent-<agent_id>.jsonl`, is self-contained (its own
  turns and `output_tokens`, its own `Skill`/`Edit` tool_uses), and is
  derivable from `transcript_path` + `agent_id`.
- The root transcript contains **no** subagent lines, so the leak is the shared
  state key alone — not transcript cross-reads.
- Neither the subagent transcript's init entries nor its `.meta.json` sidecar
  carry an available-tools list; tool availability can only be learned by
  resolving the agent definition.

## 2. Goals / Non-goals

Goals:

- Freshness is evaluated per context window. A skill load counts only for the
  context that loaded it (root, or one specific subagent instance).
- Enforcement never deadlocks a subagent that cannot load skills.
- All four freshness modes and the transcript-authoritative rebuild path remain
  correct inside subagents.

Non-goals:

- Codex subagent support. The core model stays agent-agnostic so a Codex
  adapter can add equivalent behavior later, but no Codex subagent code ships.
- Propagating a skill's context across the subagent boundary — impossible by
  design; the fix instead forces a real per-context load.
- Preload-seeding of skills listed in an agent's `skills:` frontmatter (see
  §7, YAGNI).

## 3. Core behavior

### 3.1 Context key

A **context key** identifies one context window:

- Root session (`agent_id` empty): the key is `session_id`, unchanged — existing
  state files keep matching.
- Subagent: the key is `format!("{session_id}--agent-{agent_id}")`.

Every place that keys the store by session today switches to the context key:
the `PostToolUse` recorder and `PreToolUse` guard in `commands.rs`,
`state::Store::path_for`, and `run_status`. The existing filename sanitizer
already maps any unexpected character to `_`, so the key is a safe stem.

Each context key owns its own `SessionState` (skill loads + transcript cursor).
A load recorded under one key is invisible to every other key, which closes both
failure modes:

- A subagent's load is recorded under its own key and never credits the root or
  siblings.
- The root's load is recorded under the root key; a subagent's governed write
  sees an empty record for its own key and is denied, which drives a genuine
  load into the subagent's context before the retry.

`session` and `minutes` rules are fully correct from re-keying alone, because
the recorder (confirmed to fire in subagents) writes each load under the right
key with a wall-clock timestamp; neither mode reads the transcript.

### 3.2 Context transcript resolution

`turns`/`tokens` freshness and the authoritative rebuild path read the
transcript. Because a subagent's `PreToolUse`/`PostToolUse` payload hands over
the *root* transcript path, the guard and recorder must resolve the context's
*own* transcript:

- Root (`agent_id` empty): use the payload `transcript_path`.
- Subagent: derive `<transcript_path without .jsonl>/subagents/agent-<agent_id>.jsonl`.

Both the guard and the recorder scan the resolved path, so per-context turn and
token cursors advance against the correct file and the rebuild path finds the
subagent's own `Skill` loads. If the derived file is absent or unreadable, the
scan fails open (the context keeps its recorder-driven state); `session`/
`minutes` rules are unaffected.

### 3.3 Enforcement gate (auto-exempt)

Re-keying means a subagent starts with an empty record, so its first governed
write is denied until it loads the skill in its own context. A subagent that
lacks the `Skill` tool could never satisfy that denial — a deadlock. To avoid
it, the `PreToolUse` guard decides whether to enforce *before* matching rules:

| Context | Enforce? |
| --- | --- |
| Root (no `agent_id`) | yes |
| Subagent, resolved, has `Skill` tool | yes |
| Subagent, resolved, no `Skill` tool | no — allow |
| Subagent, unresolved | no — allow |

When the gate resolves to "no", the guard returns `Allow` without evaluating any
rule. Rationale: removing the `Skill` tool from an agent is a deliberate signal
that the agent should not be loading skills, so skillforcer's requirements do
not apply; and leaning toward `Allow` on any uncertainty is consistent with
skillforcer's fail-open foundation — it never blocks on its own doubt.

The gate tightens behavior only where it can positively confirm a subagent can
load skills. Enforcement therefore applies to the root session and to subagents
known to have the `Skill` tool; everything uncertain passes.

### 3.4 Agent definition resolver

A new Claude-specific module resolves `agent_type` → tool availability:

1. Project agents: `<cwd>/.claude/agents/`.
2. User agents: `~/.claude/agents/`.
3. Built-in table (static): the default writing agents `general-purpose`,
   `claude`, and `fork` have the `Skill` tool (enforce); `statusline-setup` does
   not (exempt). Agents with no write tool (`Explore`, `Plan`,
   `claude-code-guide`) never reach the guard and need no entry.

Resolution matches an agent file by filename stem first, then by frontmatter
`name:`. The agent file is Markdown with a YAML frontmatter block; the resolver
parses that block and reads optional `tools`:

- `tools` omitted, `*`, or `all` ⇒ the agent has every tool, including `Skill`.
- `tools` an explicit list (YAML sequence or comma-separated string) ⇒ the agent
  has `Skill` only if the list contains it.

Any failure to resolve — no matching file, unknown built-in, malformed
frontmatter — yields "unresolved", which the gate treats as allow. Plugin-dir
resolution is intentionally out of scope (see §7).

A minimal YAML-frontmatter parse is required; the exact dependency is chosen at
implementation (prefer a maintained crate over hand-parsing, since `tools` may
be a sequence or a scalar).

## 4. Module boundaries

The agent-agnostic core (`rules`, `freshness`, `state`, `config`) is unchanged
except that the store is keyed by an opaque context key instead of a session id
— a string swap, not a model change. All Claude-specific knowledge is confined
to the adapter layer:

- `adapter/claude.rs` — parse `agent_id`/`agent_type`; compute the context key;
  resolve the context transcript path.
- `adapter/agents.rs` (new) — agent definition resolver and built-in table;
  answers the single question "does `agent_type` have the `Skill` tool?".

`commands.rs` wires the gate in before rule evaluation and threads the context
key and resolved transcript path through the recorder and guard. A future Codex
adapter can supply its own context-key and tool-availability logic without
touching the core.

## 5. Error handling

The fail-open philosophy is unchanged and extended to the new paths: an
unresolved agent, a missing or unreadable subagent transcript, and a malformed
agent frontmatter all resolve toward `Allow`, never `Deny`. The only behavioral
tightening is §3.1 re-keying, which merely stops crediting loads across context
boundaries; it introduces no new denial that a correctly-attributed load cannot
clear.

## 6. Testing

Unit:

- Context-key derivation (root vs subagent; sanitized stem).
- Subagent transcript-path derivation from `transcript_path` + `agent_id`.
- Resolver: project file with `Skill` in `tools`, project file without it,
  `tools` omitted/`*`/`all`, built-in table entries, unresolved ⇒ exempt.
- Enforcement-gate table (§3.3), including early `Allow` before rule matching.

Integration (hook fixtures with `agent_id`):

- A load recorded under one subagent's key does not satisfy a second subagent's
  governed write (no cross-credit).
- A root load does not satisfy a subagent's governed write (mode 2).
- A subagent load does not satisfy a later root write (mode 1).
- A subagent whose resolved definition lacks `Skill` is allowed without a rule
  firing.

New fixtures: a `PreToolUse` payload carrying `agent_id`/`agent_type`, and a
sample agent `.md` with a restricted `tools` list.

Documentation: a short "How it works" note in the README on per-subagent
isolation and the no-`Skill` exemption.

## 7. Deferred (YAGNI)

- **Preload-seeding.** Skills preloaded via an agent's `skills:` frontmatter need
  no special handling. If such a subagent has the `Skill` tool, its first
  governed write is denied once and an idempotent re-load clears it — which also
  refreshes against drift since the preload happened at the subagent's turn 0.
  If it lacks the `Skill` tool, the §3.3 gate already exempts it. Seeding would
  only save a single, arguably-healthy reload, so it is dropped along with the
  `SubagentStart` hook, the installer change, and `skills:` parsing it would
  require.
- **Plugin-dir agent resolution.** Globbing `~/.claude/plugins/**/agents/` to
  classify plugin agents is high-effort and leans on the undocumented plugin
  cache layout. Plugin and unknown agents fall through to "unresolved ⇒ allow".
- **Config knobs** for subagent scoping. The auto-exempt rule derives intent
  from the agent's own tool list, so no new configuration is introduced.

## 8. Risks & mitigations

- **Subagent transcript path convention is undocumented.** Verified on disk, but
  may change between Claude Code versions. Mitigation: fail-open on a missing
  file; `session`/`minutes` rules keep working via the recorder.
- **Built-in tool sets change between versions.** A stale table entry degrades
  gracefully — a missing or wrong "has `Skill`" mapping falls to "unresolved ⇒
  allow", never a deadlock. The table's only critical job is classifying the
  default writing subagents as enforceable.
- **`agent_id`/`agent_type` absent on some event.** Verified present on all
  relevant events; if ever empty, the context is treated as root (enforce) or
  unresolved (allow) respectively — both safe.
- **Plugin/unknown agents go un-enforced.** Accepted: this matches today's
  behavior and preserves the leak fix, which does not depend on subagent
  enforcement.
