---
name: skillforcer-config
description: Use when configuring skillforcer — writing or editing .skillforcer.toml rules, choosing freshness windows (session/minutes/turns/tokens), using presets, or installing/uninstalling its Claude Code hooks.
---

## Overview

skillforcer forces a skill to have loaded recently enough before it lets a governed
write through. It runs as a `PreToolUse` hook on `Write`/`Edit`/`MultiEdit`/`NotebookEdit`:
when a write matches a rule and none of the required skills loaded within the rule's
freshness window, skillforcer denies the write and returns a message telling Claude
which skill to load. A `PostToolUse` hook on the `Skill` tool records each load so the
next write can check it.

## Install

Install the binary, then register the hooks in the project. The install scripts fetch a
prebuilt release binary:

- macOS, Linux, or Windows Git Bash: `curl -fsSL https://raw.githubusercontent.com/strowk/skillforcer/main/install.sh | sh`
- Windows PowerShell: `irm https://raw.githubusercontent.com/strowk/skillforcer/main/install.ps1 | iex`
- From source: `cargo install --git https://github.com/strowk/skillforcer` (or `cargo install --path .` from a local checkout)

Then, from the project's root directory:

```
skillforcer install
```

`install` adds a `PreToolUse` entry (matcher `Write|Edit|MultiEdit|NotebookEdit`) and a
`PostToolUse` entry (matcher `Skill`) to Claude settings, and scaffolds
`.skillforcer.toml` in the project if one doesn't exist. By default it targets
`.claude/settings.json`; `--local` targets `.claude/settings.local.json`, `--user`
targets `~/.claude/settings.json`. `--dry-run` prints the resulting settings without
writing them. `skillforcer uninstall` (same target flags) removes only the hook entries
it added.

## Rule anatomy

Rules live in `.skillforcer.toml` at the project root. `[defaults]` sets `fail_open`
(default `true`) and `combine_freshness` (`"all"` or `"any"`, default `"all"`). Each
`[[rule]]` needs a `name`, a `requires`, and enough of `path`/`content`/`extends` to
match the writes it governs.

```toml
[defaults]
fail_open = true
combine_freshness = "all"

[[rule]]
name = "comments-need-tech-writing"
extends = ["code-comments"]
path = ["src/**/*.rs", "**/*.ts"]
requires = { any_skill = ["tech-writing:technical-writing"], minutes = 15 }
message = "Editing comments in {file}. Load {skills} first — context may have rotted."
```

- `path` — glob patterns; the rule applies only to files that match one of them.
- `content` — a regex tested against the new file content; omit to match on path alone.
- `extends` — names of presets (see below) to merge in.
- `requires` — exactly one of `any_skill = [...]` (fresh if any listed skill is fresh)
  or `all_skills = [...]` (all listed skills must be fresh), plus at least one freshness
  window. A rule with neither or both of `any_skill`/`all_skills`, or with no freshness
  window, fails to load.
- `message` — the denial text; omit to use the built-in default.

A project rule and a global rule (`~/.config/skillforcer/config.toml`, or the
platform-equivalent config dir) with the same `name` do not merge — the project rule
replaces the global one entirely.

## Freshness windows

| Key | Satisfied when |
| --- | --- |
| `session = true` | the skill has loaded at least once this session |
| `minutes = N` | the skill loaded within the last N minutes |
| `turns = N` | the skill loaded within the last N transcript turns |
| `tokens = N` | the skill loaded within the last N cumulative output tokens |

A rule can set more than one window. `combine_freshness = "all"` requires every set
window to hold; `"any"` requires only one to hold.

## Presets

`skillforcer list-presets` lists the bundled presets:

- `code-comments` — matches comment syntax (`//`, `/* */`, `<!--`, trailing `#`, `;;`)
  across common source extensions.
- `markdown-headings` — matches Markdown ATX headings (`#` through `######`) in `.md`
  files.

`extends = ["preset-name"]` on a rule unions the preset's `path` globs into the rule's
own, and fills in `content` from the preset only if the rule doesn't set its own
`content`.

## Debugging

- `skillforcer check <file> [--stdin]` — shows which rules match the file (and its
  content, read from the file or stdin), without a live session or transcript.
- `skillforcer status [--session ID]` — prints recorded skill loads and the transcript
  cursor (offset/turn/tokens) for a session, from skillforcer's on-disk state store.

## Fail-open

skillforcer never blocks a write on its own error: malformed hook input, a missing or
unreadable config, an unparseable transcript line, and a rule that fails to compile
(under `fail_open = true`, the default) all resolve to allowing the write — other rules
still apply. Set `fail_open = false` to make a bad rule deny instead of skip.
