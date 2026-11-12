//! Claude profile config: named `CLAUDE_CONFIG_DIR` profiles, either configured
//! explicitly or discovered from `~/.claude*` directories holding credentials.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Config;
use crate::error::{Error, Result};

const CREDENTIALS_FILE: &str = ".credentials.json";
const DEFAULT_DIR_NAME: &str = ".claude";
const DEFAULT_PROFILE_NAME: &str = "claude";
const DIR_PREFIX: &str = ".claude-";

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

impl Config {
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
    use crate::config::Config;

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
}
