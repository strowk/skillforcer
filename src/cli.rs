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
        Sub::Hook(_) => {
            use std::io::Read;
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).ok();
            let store = crate::state::Store::discover()
                .unwrap_or_else(|_| crate::state::Store::with_base(std::env::temp_dir().join("skillforcer")));
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let global = directories::ProjectDirs::from("", "", "skillforcer")
                .map(|d| d.config_dir().join("config.toml"));
            let decision = crate::commands::run_hook(&raw, &store, &cwd, global.as_deref());
            crate::commands::render_and_print(&decision);
            Ok(0)
        }
        Sub::Install(_) | Sub::Uninstall(_) | Sub::Check(_) | Sub::Status(_) | Sub::ListPresets(_) => {
            eprintln!("not implemented");
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
    fn parses_hook_subcommand() {
        let cli = Cli::from_args(&["skillforcer"], &["hook"]).unwrap();
        assert!(matches!(cli.sub, Sub::Hook(_)));
    }
}
