# skillforcer

Forces a Claude Code skill to load before it lets a governed write through.
`skillforcer` runs as a `PreToolUse` hook on `Write`/`Edit`/`MultiEdit`/`NotebookEdit`.
When a write matches a rule and the required skill hasn't loaded recently enough, the
write is denied; Claude receives the reason, loads the skill, and retries. A
`PostToolUse` hook on the `Skill` tool records each load so later writes can check it.

## Install

The install scripts download a prebuilt release binary for your platform.

macOS, Linux, or Windows Git Bash:

```sh
curl -fsSL https://raw.githubusercontent.com/strowk/skillforcer/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/strowk/skillforcer/main/install.ps1 | iex
```

The scripts install to `~/.local/bin` (POSIX) or `%LOCALAPPDATA%\skillforcer\bin`
(PowerShell). Override with `SKILLFORCER_INSTALL_DIR`, or pin a release with
`SKILLFORCER_VERSION=vX.Y.Z`. Supported targets: Linux (x86_64 gnu/musl, aarch64),
macOS (x86_64, arm64), Windows (x86_64).

### From source

```sh
cargo install --path .   # from a checkout
```

### Register the hooks

```sh
skillforcer install
```

`install` adds the two hooks to `.claude/settings.json` in the current project (use
`--local` for `settings.local.json`, `--user` for `~/.claude/settings.json`) and writes
a starter `.skillforcer.toml` if one doesn't exist. `uninstall` removes the hooks it
added; it never touches hooks belonging to other tools.

## Configuration

Rules live in `.skillforcer.toml` in the project root, optionally layered over a global
config in the platform config directory — `~/.config/skillforcer/config.toml` on
Linux/macOS, `%APPDATA%\skillforcer\config\config.toml` on Windows. A project rule with
the same `name` as a global one replaces it.

```toml
[defaults]
fail_open = true          # a skillforcer error never blocks a write
combine_freshness = "all" # when multiple windows are set, all must hold

[[rule]]
name = "comments-need-tech-writing"
extends = ["code-comments"]
path = ["src/**/*.rs", "**/*.ts"]
requires = { any_skill = ["tech-writing:technical-writing"], minutes = 15 }
message = "Editing comments in {file}. Load {skills} first — context may have rotted."
```

Each rule matches a write by `path` (glob patterns) and, optionally, `content` (a regex
tested against the new text). `extends` adds a preset's `path` patterns to the rule's
own and fills in `content` from the preset when the rule doesn't set one. `requires`
names the skill(s) (`any_skill` or `all_skills`, exactly one of the two) and the
freshness window(s) that must hold. `message` supports `{file}`, `{rule}`, `{skills}`,
and `{reason}` placeholders; omit it to use the built-in default message.

### Freshness modes

A rule can combine any of these; `combine_freshness` (`all` or `any`) decides whether
they all need to hold or just one:

| Mode | Config key | Satisfied when |
| --- | --- | --- |
| Session | `session = true` | the skill has loaded at least once this session |
| Minutes | `minutes = N` | the skill loaded within the last N minutes |
| Turns | `turns = N` | the skill loaded within the last N transcript turns |
| Tokens | `tokens = N` | the skill loaded within the last N output tokens |

With `combine_freshness = "all"` (the default), every window on the rule must pass; with
`"any"`, one passing window is enough.

### Presets

`skillforcer list-presets` prints the bundled presets and the glob paths each covers:

- `code-comments` — comment syntax across common languages (`//`, `/* */`, `<!--`, `#`, `;;`)
- `markdown-headings` — Markdown ATX headings (`#` through `######`)

Reference one from `extends` instead of copying its `path`/`content` into every rule.

## Debugging

- `skillforcer check <file> [--stdin]` — reports which rules match a file (and its
  content, from the file or stdin), without needing a session or transcript.
- `skillforcer status [--session ID]` — prints the recorded skill loads and transcript
  cursor (offset/turn/tokens) for a session, from the on-disk state store.

## Fail-open

skillforcer never blocks a write because of its own error: malformed hook JSON, a
missing or unreadable config, and an unreadable transcript line all resolve to `Allow`.
With `fail_open = true` (the default), a rule that fails to compile is skipped (other
rules still apply); set `fail_open = false` to make a bad rule deny instead. The binary
itself also exits `0` on any top-level error.
