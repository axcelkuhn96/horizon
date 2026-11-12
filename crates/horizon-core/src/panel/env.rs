//! Environment handling for terminal panels: agent defaults, preset `env`
//! overrides and values injected by Horizon itself.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::PanelKind;
use crate::link::strip_escapes;

/// Variable Horizon owns for agent panels; presets may not override it.
const HORIZON_MARKER: &str = "HORIZON";

/// Variables Horizon injects for the panel link; presets may not set them.
const HORIZON_INJECTED_KEYS: [&str; 4] = [
    "HORIZON_PANEL_ID",
    "HORIZON_PANEL_NAME",
    "HORIZON_SOCKET",
    "HORIZON_BIN",
];

fn agent_env(kind: PanelKind) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if kind.is_agent() {
        env.insert(HORIZON_MARKER.to_string(), "1".to_string());
    }
    env
}

/// Linux appends " (deleted)" to `current_exe()` when the binary was replaced on disk
/// (e.g. by an update); the real path is what an agent can still execute.
fn strip_deleted_suffix(path: &str) -> &str {
    path.strip_suffix(" (deleted)").unwrap_or(path)
}

/// Titles can carry terminal-controlled escapes; an env value must stay printable (and
/// NUL would make the spawn fail).
fn sanitize_name(title: &str) -> String {
    strip_escapes(title).chars().filter(|ch| !ch.is_control()).collect()
}

/// Panel-link variables Horizon sets on every terminal panel: the panel's local id and
/// title at spawn, the link socket (when the server runs) and this executable's path, so
/// an agent can call `"$HORIZON_BIN" link ...` even when `horizon` is not on `PATH`.
pub(crate) fn horizon_injected_env(local_id: &str, title: &str, socket: Option<&Path>) -> HashMap<String, String> {
    let mut env = HashMap::from([
        ("HORIZON_PANEL_ID".to_string(), local_id.to_string()),
        ("HORIZON_PANEL_NAME".to_string(), sanitize_name(title)),
    ]);
    if let Some(socket) = socket.and_then(Path::to_str) {
        env.insert("HORIZON_SOCKET".to_string(), socket.to_string());
    }
    if let Some(exe) = std::env::current_exe().ok().as_deref().and_then(Path::to_str) {
        env.insert("HORIZON_BIN".to_string(), strip_deleted_suffix(exe).to_string());
    }
    env
}

/// Environment for a terminal panel's PTY.
///
/// Starts from the per-kind defaults, applies the preset `env` (except the
/// reserved `HORIZON` key), then applies `injected` last so values set by
/// Horizon itself (e.g. `HORIZON_PANEL_*`) can never be overridden by a preset.
pub(crate) fn merge_spawn_env(
    kind: PanelKind,
    preset_env: &BTreeMap<String, String>,
    injected: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = agent_env(kind);
    env.extend(
        preset_env
            .iter()
            .filter(|(key, _)| key.as_str() != HORIZON_MARKER && !HORIZON_INJECTED_KEYS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    env.extend(injected.iter().map(|(key, value)| (key.clone(), value.clone())));
    env
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::{horizon_injected_env, merge_spawn_env, strip_deleted_suffix};
    use crate::config::PresetConfig;
    use crate::panel::PanelKind;

    fn preset_env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn merge_keeps_horizon_one_for_agents() {
        let merged = merge_spawn_env(PanelKind::Claude, &BTreeMap::new(), &HashMap::new());
        assert_eq!(merged.get("HORIZON").map(String::as_str), Some("1"));

        let shell = merge_spawn_env(PanelKind::Shell, &BTreeMap::new(), &HashMap::new());
        assert!(!shell.contains_key("HORIZON"));
    }

    #[test]
    fn merge_preset_overrides_other_keys() {
        let preset = preset_env(&[("CLAUDE_CONFIG_DIR", "/home/u/.claude-work"), ("FOO", "bar")]);
        let merged = merge_spawn_env(PanelKind::Claude, &preset, &HashMap::new());
        assert_eq!(
            merged.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/u/.claude-work")
        );
        assert_eq!(merged.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(merged.get("HORIZON").map(String::as_str), Some("1"));
    }

    #[test]
    fn merge_preset_cannot_override_horizon() {
        let preset = preset_env(&[("HORIZON", "0")]);
        let agent = merge_spawn_env(PanelKind::Claude, &preset, &HashMap::new());
        assert_eq!(agent.get("HORIZON").map(String::as_str), Some("1"));

        let shell = merge_spawn_env(PanelKind::Shell, &preset, &HashMap::new());
        assert!(!shell.contains_key("HORIZON"));
    }

    #[test]
    fn merge_injected_wins_over_preset() {
        let preset = preset_env(&[("HORIZON_PANEL_ID", "spoofed"), ("FOO", "bar")]);
        let injected = HashMap::from([("HORIZON_PANEL_ID".to_string(), "42".to_string())]);
        let merged = merge_spawn_env(PanelKind::Claude, &preset, &injected);
        assert_eq!(merged.get("HORIZON_PANEL_ID").map(String::as_str), Some("42"));
        assert_eq!(merged.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn preset_env_cannot_override_horizon_panel_vars() {
        let preset = preset_env(&[
            ("HORIZON_PANEL_ID", "spoofed"),
            ("HORIZON_PANEL_NAME", "spoofed"),
            ("HORIZON_SOCKET", "/tmp/evil.sock"),
            ("HORIZON_BIN", "/tmp/evil"),
            ("FOO", "bar"),
        ]);
        let socket = std::path::Path::new("/run/user/1/horizon/7.sock");
        let injected = horizon_injected_env("abc", "My Panel", Some(socket));
        let merged = merge_spawn_env(PanelKind::Shell, &preset, &injected);
        assert_eq!(merged.get("HORIZON_PANEL_ID").map(String::as_str), Some("abc"));
        assert_eq!(merged.get("HORIZON_PANEL_NAME").map(String::as_str), Some("My Panel"));
        assert_eq!(
            merged.get("HORIZON_SOCKET").map(String::as_str),
            Some("/run/user/1/horizon/7.sock")
        );
        assert_ne!(merged.get("HORIZON_BIN").map(String::as_str), Some("/tmp/evil"));
        assert_eq!(merged.get("FOO").map(String::as_str), Some("bar"));

        let without_socket = merge_spawn_env(PanelKind::Shell, &preset, &horizon_injected_env("abc", "n", None));
        assert!(!without_socket.contains_key("HORIZON_SOCKET"));
    }

    #[test]
    fn injected_bin_is_normalized_current_exe() {
        let env = horizon_injected_env("a", "n", None);
        let exe = std::env::current_exe().expect("current exe");
        let exe = exe.to_str().expect("utf-8 exe path");
        assert_eq!(
            env.get("HORIZON_BIN").map(String::as_str),
            Some(strip_deleted_suffix(exe))
        );
        assert!(!env["HORIZON_BIN"].ends_with(" (deleted)"));
    }

    #[test]
    fn strip_deleted_suffix_only_trims_the_kernel_marker() {
        assert_eq!(strip_deleted_suffix("/usr/bin/horizon (deleted)"), "/usr/bin/horizon");
        assert_eq!(strip_deleted_suffix("/usr/bin/horizon"), "/usr/bin/horizon");
        assert_eq!(strip_deleted_suffix("/a (deleted)/horizon"), "/a (deleted)/horizon");
    }

    #[test]
    fn injected_panel_name_drops_control_characters() {
        let env = horizon_injected_env("id", "a\u{1b}[31mb\0c\nd\r\u{7}e", None);
        assert_eq!(env.get("HORIZON_PANEL_NAME").map(String::as_str), Some("abcde"));
    }

    #[test]
    fn preset_without_env_serializes_without_key() {
        let preset: PresetConfig = serde_yaml::from_str("name: Shell\n").expect("parse preset");
        assert!(preset.env.is_empty());
        let yaml = serde_yaml::to_string(&preset).expect("serialize preset");
        assert!(!yaml.contains("env"), "empty env must not be serialized: {yaml}");
    }

    #[test]
    fn preset_env_reaches_panel_options() {
        let preset: PresetConfig =
            serde_yaml::from_str("name: Work\nkind: claude\nenv:\n  CLAUDE_CONFIG_DIR: /tmp/work\n").expect("parse");
        let options = preset.to_panel_options();
        assert_eq!(
            options.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/tmp/work")
        );
    }
}
