//! Feeding the account index from what Horizon already knows about its panels: the live-session
//! file of a running Claude (session id, config dir and the profile that owns it) and, for a
//! Claude panel started from a profile preset, the session id a status hook reported.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::Context;
use horizon_core::claude_sessions::is_session_uuid;
use horizon_core::{PanelKind, PresetConfig};

use crate::app::HorizonApp;
use crate::app::claude_launcher::is_profile_preset;

/// Panels are looked at this often at most; a session id rarely changes.
const NOTE_INTERVAL: Duration = Duration::from_secs(3);
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// A session and the account it runs under, as found in one panel.
#[derive(Debug, PartialEq, Eq)]
struct Sighting {
    session_id: String,
    profile: String,
    config_dir: String,
}

/// Alias of the profile preset whose config dir is `dir` (trailing slashes ignored).
fn profile_alias_for_dir<'a>(presets: &'a [PresetConfig], dir: &str) -> Option<(&'a str, &'a str)> {
    let normal = |text: &str| Path::new(text).components().collect::<PathBuf>();
    let wanted = normal(dir);
    presets
        .iter()
        .filter(|preset| is_profile_preset(preset))
        .find_map(|preset| {
            let preset_dir = preset.env.get(CONFIG_DIR_ENV)?;
            if normal(preset_dir) != wanted {
                return None;
            }
            Some((preset.alias.as_deref()?, preset_dir.as_str()))
        })
}

impl HorizonApp {
    /// Records the session of every panel that shows one, at most every few seconds. Cheap when
    /// nothing is new: a repeat of a known pair changes nothing and writes nothing.
    pub(in crate::app) fn note_panel_sessions(&mut self, ctx: &Context) {
        let now = Instant::now();
        if self
            .hub
            .sessions
            .last_panel_scan
            .is_some_and(|last| now.saturating_duration_since(last) < NOTE_INTERVAL)
        {
            return;
        }
        self.hub.sessions.last_panel_scan = Some(now);
        let sightings: Vec<Sighting> = self
            .board
            .panels
            .iter()
            .filter(|panel| matches!(panel.kind, PanelKind::Shell | PanelKind::Command | PanelKind::Claude))
            .filter_map(|panel| {
                if let Some(live) = self.hub.status_ui.live(panel.id)
                    && let (Some(info), Some(profile)) = (live.info.as_ref(), live.profile.as_ref())
                {
                    return Some(Sighting {
                        session_id: info.session_id.clone(),
                        profile: profile.clone(),
                        config_dir: live.config_dir.to_string_lossy().into_owned(),
                    });
                }
                // A hook named the session of a Claude panel started from a profile preset.
                let session_id = panel.agent_session_id()?;
                let dir = panel.env.get(CONFIG_DIR_ENV)?;
                let (alias, preset_dir) = profile_alias_for_dir(&self.presets, dir)?;
                Some(Sighting {
                    session_id: session_id.to_owned(),
                    profile: alias.to_owned(),
                    config_dir: preset_dir.to_owned(),
                })
            })
            .filter(|sighting| is_session_uuid(&sighting.session_id))
            .collect();
        for sighting in sightings {
            self.hub
                .sessions
                .record(ctx, &sighting.session_id, &sighting.profile, &sighting.config_dir);
        }
        self.hub.sessions.attribute_open_modal();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use horizon_core::PanelResume;

    use super::*;

    fn preset(alias: &str, dir: &str) -> PresetConfig {
        PresetConfig {
            name: format!("Claude \u{b7} {alias}"),
            alias: Some(alias.to_owned()),
            kind: PanelKind::Claude,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: BTreeMap::from([(CONFIG_DIR_ENV.to_owned(), dir.to_owned())]),
        }
    }

    #[test]
    fn alias_is_found_by_config_dir_ignoring_a_trailing_slash() {
        let presets = vec![
            preset("conta1", "/home/u/.claude"),
            preset("conta2", "/home/u/.claude-conta2"),
        ];

        assert_eq!(
            profile_alias_for_dir(&presets, "/home/u/.claude-conta2/"),
            Some(("conta2", "/home/u/.claude-conta2"))
        );
        assert_eq!(profile_alias_for_dir(&presets, "/elsewhere"), None);
    }

    #[test]
    fn plain_claude_presets_without_a_config_dir_are_not_profiles() {
        let mut base = preset("cc", "/home/u/.claude");
        base.env.clear();

        assert_eq!(profile_alias_for_dir(&[base], "/home/u/.claude"), None);
    }
}
