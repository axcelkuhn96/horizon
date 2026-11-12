//! Claude quick-start: the "Claude" button in a shell panel's titlebar. It expands inline, in the
//! titlebar row, into one chip per profile and then `New` / `Continue` / `Resume…`, and types one
//! command line into THIS panel's PTY. The pure pieces live here (command line, enablement, stage
//! machine) and in [`layout`] (widths); state and wiring are in [`strip`], drawing in [`paint`].
//! Visual spec: `docs/design/claude-hub-ui.md` §0.

use std::path::Path;

use horizon_core::agent_detect::default_claude_config_dir;
use horizon_core::{PanelKind, PresetConfig};

use super::session::shell_single_quote;

mod layout;
mod paint;
mod strip;
pub(super) mod titlebar;

pub(super) use paint::paint_strip;
pub(super) use strip::{QuickStartUi, StripArgs};

const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
const DEFAULT_BINARY: &str = "claude";

/// How the session starts inside the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuickStartMode {
    New,
    Continue,
    Resume,
}

impl QuickStartMode {
    pub(super) const ALL: [Self; 3] = [Self::New, Self::Continue, Self::Resume];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::Continue => "Continue last",
            Self::Resume => "Resume\u{2026}",
        }
    }

    /// Short caption for the inline buttons.
    pub(super) fn inline_label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::Continue => "Continue",
            Self::Resume => "Resume\u{2026}",
        }
    }

    pub(super) fn hint(self) -> &'static str {
        match self {
            Self::New => "start a fresh session",
            Self::Continue => "--continue",
            Self::Resume => "--resume \u{b7} pick a session",
        }
    }

    fn flag(self) -> Option<&'static str> {
        match self {
            Self::New => None,
            Self::Continue => Some("--continue"),
            Self::Resume => Some("--resume"),
        }
    }
}

/// Word as the shell should read it: bare when it only holds safe characters, else single-quoted.
/// `None` when it cannot be quoted safely (control character).
fn shell_word(word: &str) -> Option<String> {
    let bare = !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | ',' | '@' | '%' | '+')
        });
    if bare {
        Some(word.to_owned())
    } else {
        shell_single_quote(word)
    }
}

/// The command line that starts Claude for the derived profile `preset` in `mode`: the
/// `CLAUDE_CONFIG_DIR` prefix (only outside the default `~/.claude`), the preset's own binary and
/// args, then the mode flag. `None` when the dir, binary or an arg cannot be quoted safely.
#[must_use]
pub(super) fn quick_start_command(preset: &PresetConfig, mode: QuickStartMode, home: &Path) -> Option<String> {
    let mut words = Vec::new();
    if let Some(dir) = preset.env.get(CONFIG_DIR_ENV)
        && Path::new(dir) != default_claude_config_dir(home)
    {
        words.push(format!("{CONFIG_DIR_ENV}={}", shell_single_quote(dir)?));
    }
    words.push(shell_word(preset.command.as_deref().unwrap_or(DEFAULT_BINARY))?);
    for arg in &preset.args {
        words.push(shell_word(arg)?);
    }
    words.extend(mode.flag().map(str::to_owned));
    Some(words.join(" "))
}

/// Whether the titlebar button is drawn at all: local shell panels, once a profile exists.
#[must_use]
pub(super) fn quick_start_visible(kind: PanelKind, is_ssh: bool, profile_count: usize) -> bool {
    kind == PanelKind::Shell && !is_ssh && profile_count > 0
}

/// Whether the button can be used now: a local shell with nothing else running in the foreground.
#[must_use]
pub(super) fn quick_start_enabled(kind: PanelKind, is_ssh: bool, busy: bool) -> bool {
    kind == PanelKind::Shell && !is_ssh && !busy
}

/// What the inline strip shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    /// Just the "Claude" button.
    Collapsed,
    /// One chip per profile.
    Profiles,
    /// `New` / `Continue` / `Resume…` for the chosen profile (index into the profile list).
    Modes { profile: usize },
}

/// One user intent on the strip, already decoded from a click or a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StripEvent {
    /// The "Claude" button (or the star that replaces it while open).
    Toggle,
    /// A profile chip.
    Pick(usize),
    /// The profile name on the mode page: back to the chips.
    Back,
    Mode(QuickStartMode),
    /// Esc, the close button or a click outside.
    Dismiss,
}

/// A start the strip asks the app to type into the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StartRequest {
    pub(super) profile: usize,
    pub(super) mode: QuickStartMode,
}

impl Stage {
    /// Next stage after `event` with `profiles` profiles listed, plus the start to perform.
    #[must_use]
    pub(super) fn apply(self, event: StripEvent, profiles: usize) -> (Self, Option<StartRequest>) {
        match (self, event) {
            (_, StripEvent::Dismiss) => (Self::Collapsed, None),
            (Self::Collapsed, StripEvent::Toggle) if profiles > 0 => (Self::Profiles, None),
            (Self::Collapsed, _) => (Self::Collapsed, None),
            (_, StripEvent::Toggle) => (Self::Collapsed, None),
            (stage, StripEvent::Pick(index)) => {
                if index < profiles {
                    (Self::Modes { profile: index }, None)
                } else {
                    (stage, None)
                }
            }
            (Self::Modes { .. }, StripEvent::Back) => (Self::Profiles, None),
            (stage, StripEvent::Back) => (stage, None),
            (Self::Modes { profile }, StripEvent::Mode(mode)) => {
                let request = (profile < profiles).then_some(StartRequest { profile, mode });
                (Self::Collapsed, request)
            }
            (stage, StripEvent::Mode(_)) => (stage, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use horizon_core::{PanelKind, PanelResume, PresetConfig};

    use super::*;

    const HOME: &str = "/home/u";

    fn preset(dir: Option<&str>, command: Option<&str>, args: &[&str]) -> PresetConfig {
        PresetConfig {
            name: "Claude \u{b7} conta2".to_owned(),
            alias: Some("conta2".to_owned()),
            kind: PanelKind::Claude,
            command: command.map(str::to_owned),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: dir
                .map(|dir| BTreeMap::from([(CONFIG_DIR_ENV.to_owned(), dir.to_owned())]))
                .unwrap_or_default(),
        }
    }

    fn command(preset: &PresetConfig, mode: QuickStartMode) -> Option<String> {
        quick_start_command(preset, mode, Path::new(HOME))
    }

    #[test]
    fn other_profile_gets_a_quoted_config_dir_prefix_and_the_preset_args() {
        let preset = preset(
            Some("/home/u/.claude-conta2"),
            Some("claude"),
            &["--dangerously-skip-permissions"],
        );

        assert_eq!(
            command(&preset, QuickStartMode::Continue).as_deref(),
            Some("CLAUDE_CONFIG_DIR='/home/u/.claude-conta2' claude --dangerously-skip-permissions --continue")
        );
    }

    #[test]
    fn default_config_dir_omits_the_prefix_even_with_a_trailing_slash() {
        for dir in ["/home/u/.claude", "/home/u/.claude/"] {
            let preset = preset(Some(dir), Some("claude"), &[]);

            assert_eq!(
                command(&preset, QuickStartMode::New).as_deref(),
                Some("claude"),
                "{dir}"
            );
        }
        assert_eq!(
            command(&preset(None, None, &[]), QuickStartMode::New).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn modes_add_exactly_their_own_flag_last() {
        let preset = preset(None, Some("claude"), &["--model", "opus"]);

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("claude --model opus")
        );
        assert_eq!(
            command(&preset, QuickStartMode::Continue).as_deref(),
            Some("claude --model opus --continue")
        );
        assert_eq!(
            command(&preset, QuickStartMode::Resume).as_deref(),
            Some("claude --model opus --resume")
        );
    }

    #[test]
    fn hostile_config_dir_stays_inside_the_quotes() {
        let preset = preset(Some("/tmp/it's $(reboot);x"), Some("claude"), &[]);

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("CLAUDE_CONFIG_DIR='/tmp/it'\\''s $(reboot);x' claude")
        );
    }

    #[test]
    fn control_characters_in_dir_binary_or_args_refuse_the_command() {
        assert_eq!(
            command(&preset(Some("/tmp/a\nrm -rf /"), None, &[]), QuickStartMode::New),
            None
        );
        assert_eq!(command(&preset(None, Some("claude\r"), &[]), QuickStartMode::New), None);
        assert_eq!(command(&preset(None, None, &["--x\n"]), QuickStartMode::New), None);
    }

    #[test]
    fn awkward_binary_and_args_are_quoted_not_dropped() {
        let preset = preset(
            None,
            Some("/opt/my tools/claude"),
            &["--append-system-prompt", "be brief; ok", ""],
        );

        assert_eq!(
            command(&preset, QuickStartMode::New).as_deref(),
            Some("'/opt/my tools/claude' --append-system-prompt 'be brief; ok' ''")
        );
    }

    #[test]
    fn button_shows_only_on_local_shells_with_a_profile() {
        assert!(quick_start_visible(PanelKind::Shell, false, 1));
        assert!(!quick_start_visible(PanelKind::Shell, false, 0));
        assert!(!quick_start_visible(PanelKind::Shell, true, 2));
        assert!(!quick_start_visible(PanelKind::Claude, false, 2));
        assert!(!quick_start_visible(PanelKind::Ssh, false, 2));
        assert!(!quick_start_visible(PanelKind::Editor, false, 2));
    }

    #[test]
    fn button_is_disabled_while_a_foreground_program_runs() {
        assert!(quick_start_enabled(PanelKind::Shell, false, false));
        assert!(!quick_start_enabled(PanelKind::Shell, false, true));
        assert!(!quick_start_enabled(PanelKind::Shell, true, false));
        assert!(!quick_start_enabled(PanelKind::Command, false, false));
    }

    fn request(profile: usize, mode: QuickStartMode) -> Option<StartRequest> {
        Some(StartRequest { profile, mode })
    }

    #[test]
    fn button_expands_into_the_chips_and_toggles_back() {
        assert_eq!(Stage::Collapsed.apply(StripEvent::Toggle, 3), (Stage::Profiles, None));
        assert_eq!(Stage::Profiles.apply(StripEvent::Toggle, 3), (Stage::Collapsed, None));
    }

    #[test]
    fn no_profiles_keeps_the_strip_collapsed() {
        assert_eq!(Stage::Collapsed.apply(StripEvent::Toggle, 0), (Stage::Collapsed, None));
    }

    #[test]
    fn picking_a_chip_opens_its_modes_only_inside_the_list() {
        assert_eq!(
            Stage::Profiles.apply(StripEvent::Pick(2), 3),
            (Stage::Modes { profile: 2 }, None)
        );
        assert_eq!(Stage::Profiles.apply(StripEvent::Pick(3), 3), (Stage::Profiles, None));
        assert_eq!(Stage::Collapsed.apply(StripEvent::Pick(0), 3), (Stage::Collapsed, None));
    }

    #[test]
    fn mode_buttons_start_the_chosen_profile_and_collapse() {
        let stage = Stage::Modes { profile: 1 };

        for mode in QuickStartMode::ALL {
            assert_eq!(
                stage.apply(StripEvent::Mode(mode), 3),
                (Stage::Collapsed, request(1, mode))
            );
        }
    }

    #[test]
    fn a_stale_profile_index_collapses_without_starting_anything() {
        assert_eq!(
            Stage::Modes { profile: 5 }.apply(StripEvent::Mode(QuickStartMode::New), 3),
            (Stage::Collapsed, None)
        );
    }

    #[test]
    fn mode_buttons_do_nothing_outside_the_mode_page() {
        assert_eq!(
            Stage::Profiles.apply(StripEvent::Mode(QuickStartMode::New), 3),
            (Stage::Profiles, None)
        );
        assert_eq!(
            Stage::Collapsed.apply(StripEvent::Mode(QuickStartMode::New), 3),
            (Stage::Collapsed, None)
        );
    }

    #[test]
    fn the_profile_name_goes_back_to_the_chips() {
        assert_eq!(
            Stage::Modes { profile: 1 }.apply(StripEvent::Back, 3),
            (Stage::Profiles, None)
        );
        assert_eq!(Stage::Profiles.apply(StripEvent::Back, 3), (Stage::Profiles, None));
    }

    #[test]
    fn dismiss_collapses_from_every_stage() {
        for stage in [Stage::Collapsed, Stage::Profiles, Stage::Modes { profile: 0 }] {
            assert_eq!(stage.apply(StripEvent::Dismiss, 3), (Stage::Collapsed, None));
        }
    }

    #[test]
    fn inline_labels_are_short() {
        let labels: Vec<_> = QuickStartMode::ALL
            .into_iter()
            .map(QuickStartMode::inline_label)
            .collect();

        assert_eq!(labels, ["New", "Continue", "Resume\u{2026}"]);
    }
}
