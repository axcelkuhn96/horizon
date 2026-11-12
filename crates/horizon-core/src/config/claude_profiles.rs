//! Claude profile config: named `CLAUDE_CONFIG_DIR` profiles, either configured
//! explicitly or discovered from `~/.claude*` directories holding credentials.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Config, PresetConfig, default_claude_preset};
use crate::error::{Error, Result};
use crate::panel::PanelKind;

const CREDENTIALS_FILE: &str = ".credentials.json";
const DEFAULT_DIR_NAME: &str = ".claude";
const DEFAULT_PROFILE_NAME: &str = "claude";
const DIR_PREFIX: &str = ".claude-";
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Name of the user-facing base Claude preset that derived profile presets inherit from.
pub const CLAUDE_BASE_PRESET_NAME: &str = "Claude Code";

/// A named Claude account profile, selected through `CLAUDE_CONFIG_DIR`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeProfile {
    pub name: String,
    /// Directory handed to Claude as `CLAUDE_CONFIG_DIR`; may start with `~`.
    pub config_dir: String,
}

/// Expand a leading `~` / `~/` in `raw` against `home`.
#[must_use]
pub fn expand_config_dir(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Discover profiles under `home`: `~/.claude` first, then `~/.claude-<x>`
/// alphabetically, keeping only directories that contain `.credentials.json`.
#[must_use]
pub fn discover_claude_profiles(home: &Path) -> Vec<ClaudeProfile> {
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut found: Vec<ClaudeProfile> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let dir_name = file_name.to_str()?;
            let name = if dir_name == DEFAULT_DIR_NAME {
                DEFAULT_PROFILE_NAME
            } else {
                dir_name.strip_prefix(DIR_PREFIX).filter(|suffix| !suffix.is_empty())?
            };
            let path = entry.path();
            (path.is_dir() && path.join(CREDENTIALS_FILE).is_file()).then(|| ClaudeProfile {
                name: name.to_string(),
                config_dir: format!("~/{dir_name}"),
            })
        })
        .collect();
    found.sort_by(|a, b| {
        let rank = |p: &ClaudeProfile| p.name != DEFAULT_PROFILE_NAME;
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    found
}

/// Validate configured profiles: non-empty names (case-insensitively unique)
/// and non-empty directories.
///
/// # Errors
///
/// Returns [`Error::Config`] describing the first offending profile.
pub fn validate_claude_profiles(profiles: &[ClaudeProfile]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for profile in profiles {
        let name = profile.name.trim();
        if name.is_empty() {
            return Err(Error::Config(
                "claude_profiles: profile name must not be empty".to_string(),
            ));
        }
        if profile.config_dir.trim().is_empty() {
            return Err(Error::Config(format!(
                "claude_profiles: profile '{name}' has an empty config_dir"
            )));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(Error::Config(format!(
                "claude_profiles: duplicate profile name '{name}'"
            )));
        }
    }
    Ok(())
}

/// Derive a launcher preset for `profile` from `base`: same command, args and
/// resume behaviour, with `CLAUDE_CONFIG_DIR` pointing at the profile directory.
#[must_use]
pub fn claude_profile_preset(base: &PresetConfig, profile: &ClaudeProfile, home: &Path) -> PresetConfig {
    PresetConfig {
        name: format!("Claude \u{b7} {}", profile.name),
        alias: Some(profile.name.clone()),
        kind: PanelKind::Claude,
        command: base.command.clone(),
        args: base.args.clone(),
        resume: base.resume.clone(),
        ssh_connection: None,
        env: std::iter::once((
            CONFIG_DIR_ENV.to_string(),
            expand_config_dir(&profile.config_dir, home)
                .to_string_lossy()
                .into_owned(),
        ))
        .collect(),
    }
}

impl Config {
    /// Base for derived Claude presets: the preset named `Claude Code`, else the
    /// first `claude` preset, else the built-in default.
    fn claude_base_preset(&self) -> PresetConfig {
        self.presets
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(CLAUDE_BASE_PRESET_NAME))
            .or_else(|| self.presets.iter().find(|preset| preset.kind == PanelKind::Claude))
            .cloned()
            .unwrap_or_else(default_claude_preset)
    }

    /// One derived preset per effective Claude profile. Never persisted.
    pub(super) fn derived_claude_presets(&self, home: &Path) -> Vec<PresetConfig> {
        let base = self.claude_base_preset();
        self.effective_claude_profiles()
            .iter()
            .map(|profile| claude_profile_preset(&base, profile, home))
            .collect()
    }

    /// Configured Claude profiles, or the ones discovered under `$HOME` when none are configured.
    #[must_use]
    pub fn effective_claude_profiles(&self) -> Vec<ClaudeProfile> {
        if !self.claude_profiles.is_empty() {
            return self.claude_profiles.clone();
        }
        std::env::var_os("HOME")
            .map(|home| discover_claude_profiles(Path::new(&home)))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::{Config, PresetConfig};
    use crate::panel::{PanelKind, PanelResume};

    fn make_profile_dir(home: &Path, dir: &str, with_creds: bool) {
        let path = home.join(dir);
        fs::create_dir_all(&path).unwrap();
        if with_creds {
            fs::write(path.join(".credentials.json"), "{}").unwrap();
        }
    }

    fn profile(name: &str, dir: &str) -> ClaudeProfile {
        ClaudeProfile {
            name: name.to_string(),
            config_dir: dir.to_string(),
        }
    }

    #[test]
    fn discover_orders_claude_first_then_alpha() {
        let home = tempfile::tempdir().unwrap();
        make_profile_dir(home.path(), ".claude-conta3", true);
        make_profile_dir(home.path(), ".claude", true);
        make_profile_dir(home.path(), ".claude-conta2", true);
        make_profile_dir(home.path(), ".claude-nocreds", false);

        let names: Vec<String> = discover_claude_profiles(home.path())
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["claude", "conta2", "conta3"]);
    }

    #[test]
    fn discover_derives_names() {
        let home = tempfile::tempdir().unwrap();
        make_profile_dir(home.path(), ".claude", true);
        make_profile_dir(home.path(), ".claude-work", true);

        let found = discover_claude_profiles(home.path());
        assert_eq!(
            found,
            [profile("claude", "~/.claude"), profile("work", "~/.claude-work")]
        );
    }

    #[test]
    fn validate_rejects_case_insensitive_duplicates() {
        let err = validate_claude_profiles(&[profile("Work", "~/.a"), profile("work", "~/.b")]).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn validate_rejects_empty_name() {
        assert!(validate_claude_profiles(&[profile("  ", "~/.a")]).is_err());
    }

    #[test]
    fn validate_rejects_empty_dir() {
        assert!(validate_claude_profiles(&[profile("a", " ")]).is_err());
    }

    #[test]
    fn validate_accepts_distinct_profiles() {
        validate_claude_profiles(&[profile("a", "~/.a"), profile("b", "~/.b")]).unwrap();
    }

    #[test]
    fn expand_config_dir_handles_tilde() {
        let home = Path::new("/home/x");
        assert_eq!(expand_config_dir("~", home), PathBuf::from("/home/x"));
        assert_eq!(
            expand_config_dir("~/.claude-a", home),
            PathBuf::from("/home/x/.claude-a")
        );
        assert_eq!(expand_config_dir("/abs/dir", home), PathBuf::from("/abs/dir"));
    }

    #[test]
    fn config_without_profiles_round_trips_without_key() {
        let yaml = Config::default().to_yaml().unwrap();
        assert!(!yaml.contains("claude_profiles"), "{yaml}");
    }

    #[test]
    fn config_with_profiles_parses() {
        let config = Config::from_yaml("claude_profiles:\n  - name: work\n    config_dir: ~/.claude-work\n").unwrap();
        assert_eq!(config.claude_profiles, [profile("work", "~/.claude-work")]);
        assert_eq!(config.effective_claude_profiles(), config.claude_profiles);
    }

    #[test]
    fn config_with_duplicate_profiles_fails_validation() {
        let yaml = "claude_profiles:\n  - {name: a, config_dir: ~/.a}\n  - {name: A, config_dir: ~/.b}\n";
        assert!(Config::from_yaml(yaml).is_err());
    }

    fn config_with_profiles(extra: &str) -> Config {
        let yaml = format!("claude_profiles:\n  - {{name: work, config_dir: ~/.claude-work}}\n{extra}");
        Config::from_yaml(&yaml).unwrap()
    }

    #[test]
    fn derived_preset_inherits_base_args_and_sets_config_dir() {
        let base = PresetConfig {
            name: "Claude Code".to_string(),
            alias: Some("cc".to_string()),
            kind: PanelKind::Claude,
            command: Some("claude-wrapper".to_string()),
            args: vec!["--model".to_string(), "opus".to_string()],
            resume: PanelResume::Last,
            ssh_connection: None,
            env: std::collections::BTreeMap::new(),
        };
        let derived = claude_profile_preset(&base, &profile("work", "~/.claude-work"), Path::new("/home/x"));

        assert_eq!(derived.name, "Claude \u{b7} work");
        assert_eq!(derived.alias.as_deref(), Some("work"));
        assert_eq!(derived.kind, PanelKind::Claude);
        assert_eq!(derived.command.as_deref(), Some("claude-wrapper"));
        assert_eq!(derived.args, base.args);
        assert_eq!(derived.resume, PanelResume::Last);
        assert_eq!(
            derived.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/x/.claude-work")
        );
        assert_eq!(derived.env.len(), 1);
    }

    #[test]
    fn resolved_presets_lists_profile_presets_after_base() {
        let presets = config_with_profiles("").resolved_presets();
        let base = presets.iter().position(|p| p.name == CLAUDE_BASE_PRESET_NAME).unwrap();
        let derived = presets.iter().position(|p| p.name == "Claude \u{b7} work").unwrap();
        assert!(derived > base, "derived at {derived}, base at {base}");
        assert!(presets[derived].env.contains_key("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn derived_presets_not_serialized_to_yaml() {
        let config = config_with_profiles("");
        let _ = config.resolved_presets();
        let yaml = config.to_yaml().unwrap();
        assert!(!yaml.contains("Claude \u{b7}"), "{yaml}");
        assert!(!yaml.contains("CLAUDE_CONFIG_DIR"), "{yaml}");
    }

    #[test]
    fn user_preset_with_same_name_wins() {
        let mut config = config_with_profiles("");
        config.presets.push(PresetConfig {
            name: "claude \u{b7} WORK".to_string(),
            alias: None,
            kind: PanelKind::Shell,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: std::collections::BTreeMap::new(),
        });
        let matching: Vec<_> = config
            .resolved_presets()
            .into_iter()
            .filter(|p| p.name.eq_ignore_ascii_case("Claude \u{b7} work"))
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].kind, PanelKind::Shell);
    }
}
