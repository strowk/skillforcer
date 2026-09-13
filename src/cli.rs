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
    Version(VersionCmd),
}

/// Run as an agent hook (reads hook JSON on stdin).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "hook")]
pub struct HookCmd {
    /// agent harness the hook serves: claude (default) or codex
    #[argh(option, default = "String::from(\"claude\")")]
    pub harness: String,
}

/// Install skillforcer hooks into the agent's settings (Claude Code or Codex CLI).
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
    /// force installing for Claude Code
    #[argh(switch)]
    pub claude: bool,
    /// force installing for Codex CLI
    #[argh(switch)]
    pub codex: bool,
}

/// Remove skillforcer hooks from the agent's settings (Claude Code or Codex CLI).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "uninstall")]
pub struct UninstallCmd {
    /// target .claude/settings.local.json
    #[argh(switch)]
    pub local: bool,
    /// target ~/.claude/settings.json
    #[argh(switch)]
    pub user: bool,
    /// force uninstalling for Claude Code
    #[argh(switch)]
    pub claude: bool,
    /// force uninstalling for Codex CLI
    #[argh(switch)]
    pub codex: bool,
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

/// Print the skillforcer version.
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "version")]
pub struct VersionCmd {}

/// Resolves which harnesses an install/uninstall run targets: `--claude`/`--codex`
/// pick explicitly, otherwise falls back to `detected`. `--local` has no Codex
/// analogue, so it drops Codex from the result unless `--codex` was explicit —
/// bare `--local` must still install/uninstall Claude-local even when `.codex/`
/// is also present. `--local` combined with an explicit `--codex` is a genuine
/// conflict and errors.
fn resolve_harnesses(
    explicit_claude: bool,
    explicit_codex: bool,
    local: bool,
    detected: Vec<crate::model::Harness>,
) -> anyhow::Result<Vec<crate::model::Harness>> {
    if local && explicit_codex {
        anyhow::bail!("--local is Claude-only; use --codex without --local");
    }
    let harnesses = if explicit_claude || explicit_codex {
        let mut v = Vec::new();
        if explicit_claude {
            v.push(crate::model::Harness::Claude);
        }
        if explicit_codex {
            v.push(crate::model::Harness::Codex);
        }
        v
    } else {
        detected
    };
    Ok(if local {
        harnesses
            .into_iter()
            .filter(|h| *h != crate::model::Harness::Codex)
            .collect()
    } else {
        harnesses
    })
}

/// Bare binary name for the hook `command`. A settings.json is often committed
/// or shared, so an absolute path (one user's install location) breaks on other
/// machines; the bare name resolves via PATH, where the installer puts
/// skillforcer. Falls back to `skillforcer` if the exe name is unavailable.
fn hook_exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "skillforcer".into())
}

pub fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match cli.sub {
        Sub::Hook(h) => {
            use std::io::Read;
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).ok();
            let store = crate::state::Store::discover().unwrap_or_else(|_| {
                crate::state::Store::with_base(std::env::temp_dir().join("skillforcer"))
            });
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let global = directories::ProjectDirs::from("", "", "skillforcer")
                .map(|d| d.config_dir().join("config.toml"));
            let harness = crate::model::Harness::from_flag(&h.harness);
            let decision =
                crate::commands::run_hook(&raw, &store, &cwd, global.as_deref(), harness);
            crate::commands::render_and_print(&decision);
            Ok(0)
        }
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
            let global = directories::ProjectDirs::from("", "", "skillforcer")
                .map(|d| d.config_dir().join("config.toml"));
            crate::commands::run_check(&cwd, &file, &content, global.as_deref())?;
            Ok(0)
        }
        Sub::Status(s) => {
            let store = crate::state::Store::discover().unwrap_or_else(|_| {
                crate::state::Store::with_base(std::env::temp_dir().join("skillforcer"))
            });
            let session = s.session.clone().unwrap_or_default();
            print!("{}", crate::commands::run_status(&store, &session));
            Ok(0)
        }
        Sub::ListPresets(_) => {
            print!("{}", crate::commands::run_list_presets());
            Ok(0)
        }
        Sub::Version(_) => {
            println!("skillforcer {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Sub::Install(c) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let exe = hook_exe_name();
            let target = if c.user {
                crate::install::Target::User
            } else if c.local {
                crate::install::Target::Local
            } else {
                crate::install::Target::Project
            };
            let harnesses = resolve_harnesses(
                c.claude,
                c.codex,
                c.local,
                crate::install::detect_harnesses(&cwd),
            )?;
            for h in harnesses {
                let summary = crate::install::install(target, &exe, &cwd, c.dry_run, h)?;
                println!("{summary}");
                if h == crate::model::Harness::Codex && !c.dry_run {
                    println!("{}", crate::install::CODEX_TRUST_NOTE);
                }
            }
            if !c.dry_run {
                let created = crate::install::scaffold_config(&cwd)?;
                println!(
                    "{}",
                    if created {
                        "created .skillforcer.toml"
                    } else {
                        ".skillforcer.toml already present"
                    }
                );
            }
            Ok(0)
        }
        Sub::Uninstall(c) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let target = if c.user {
                crate::install::Target::User
            } else if c.local {
                crate::install::Target::Local
            } else {
                crate::install::Target::Project
            };
            let harnesses = resolve_harnesses(
                c.claude,
                c.codex,
                c.local,
                crate::install::detect_harnesses(&cwd),
            )?;
            for h in harnesses {
                crate::install::uninstall(target, &cwd, h)?;
            }
            println!("uninstalled skillforcer hooks");
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_list_presets_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["list-presets"]).unwrap();
        assert!(matches!(cli.sub, Sub::ListPresets(_)));
    }
    #[test]
    fn hook_exe_name_is_not_an_absolute_path() {
        let name = hook_exe_name();
        assert!(!name.is_empty());
        assert!(!name.contains('/'), "hook command leaked a path: {name}");
        assert!(!name.contains('\\'), "hook command leaked a path: {name}");
    }

    #[test]
    fn parses_hook_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["hook"]).unwrap();
        assert!(matches!(cli.sub, Sub::Hook(_)));
    }

    #[test]
    fn parses_version_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["version"]).unwrap();
        assert!(matches!(cli.sub, Sub::Version(_)));
    }

    #[test]
    fn bare_local_drops_detected_codex_without_erroring() {
        use crate::model::Harness;
        let detected = vec![Harness::Claude, Harness::Codex];
        let harnesses = resolve_harnesses(false, false, true, detected).unwrap();
        assert_eq!(harnesses, vec![Harness::Claude]);
    }

    #[test]
    fn local_with_explicit_codex_flag_errors() {
        use crate::model::Harness;
        let detected = vec![Harness::Claude];
        assert!(resolve_harnesses(false, true, true, detected).is_err());
    }

    #[test]
    fn local_with_explicit_claude_flag_keeps_claude() {
        use crate::model::Harness;
        let harnesses = resolve_harnesses(true, false, true, vec![]).unwrap();
        assert_eq!(harnesses, vec![Harness::Claude]);
    }
}
