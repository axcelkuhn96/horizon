//! Claude quick-start: the "Claude" button in a shell panel's titlebar. It expands inline, in the
//! titlebar row, into one chip per profile and then `New` / `Continue` / `Resume…`, and types one
//! command line into THIS panel's PTY. The pure pieces live here (modes, enablement, stage
//! machine) and in [`command`] (the command line) and [`layout`] (widths); state is in [`state`],
//! item placement in [`placement`], per-frame interaction in [`strip`], the start itself in
//! [`actions`] and drawing in [`paint`].
//! Visual spec: `docs/design/claude-hub-ui.md` §0.

use horizon_core::PanelKind;

mod actions;
mod command;
mod layout;
mod paint;
mod placement;
mod state;
mod strip;
pub(super) mod titlebar;

pub(super) use paint::paint_strip;
pub(super) use state::QuickStartUi;
pub(super) use strip::StripArgs;

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

/// Whether the collapsed button can be used, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Availability {
    Ready,
    /// A foreground program holds the terminal.
    Busy,
    /// The shell process is gone.
    Exited,
}

impl Availability {
    #[must_use]
    pub(super) fn of(kind: PanelKind, is_ssh: bool, busy: bool, exited: bool) -> Self {
        if exited {
            Self::Exited
        } else if quick_start_enabled(kind, is_ssh, busy) {
            Self::Ready
        } else {
            Self::Busy
        }
    }

    #[must_use]
    pub(super) fn enabled(self) -> bool {
        self == Self::Ready
    }

    #[must_use]
    pub(super) fn tooltip(self) -> &'static str {
        match self {
            Self::Ready => titlebar::READY_TOOLTIP,
            Self::Busy => titlebar::BUSY_TOOLTIP,
            Self::Exited => titlebar::EXITED_TOOLTIP,
        }
    }
}

/// Why a start was refused after the user asked for it; shown briefly in the titlebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StartFailure {
    /// The profile has no derived Claude preset any more.
    NoPreset,
    Busy,
    Exited,
    /// The preset's directory, binary or an argument holds a control character.
    Unquotable,
    /// The link write into the panel failed.
    WriteFailed,
    /// Auto found no profile with a usable login and weekly quota.
    NoAccount,
}

impl StartFailure {
    #[must_use]
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::NoPreset => "No Claude preset for this profile",
            Self::Busy => "Busy \u{2014} finish the running command first",
            Self::Exited => "Shell exited \u{2014} nothing to type into",
            Self::Unquotable => "The profile command can't be typed safely",
            Self::WriteFailed => "Could not write to this terminal",
            Self::NoAccount => "No Claude account available",
        }
    }
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
            (Self::Collapsed, _) | (_, StripEvent::Toggle) => (Self::Collapsed, None),
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
    use super::*;

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

    #[test]
    fn availability_tells_exited_from_busy() {
        assert_eq!(
            Availability::of(PanelKind::Shell, false, false, false),
            Availability::Ready
        );
        assert_eq!(
            Availability::of(PanelKind::Shell, false, true, false),
            Availability::Busy
        );
        assert_eq!(
            Availability::of(PanelKind::Shell, false, false, true),
            Availability::Exited
        );
        assert_eq!(
            Availability::of(PanelKind::Shell, false, true, true),
            Availability::Exited
        );
        assert_eq!(
            Availability::of(PanelKind::Shell, true, false, false),
            Availability::Busy
        );
        assert!(Availability::Ready.enabled());
        assert!(!Availability::Busy.enabled() && !Availability::Exited.enabled());
        assert!(Availability::Exited.tooltip().starts_with("Shell exited"));
        assert!(Availability::Busy.tooltip().starts_with("Busy"));
    }

    #[test]
    fn every_failure_has_its_own_short_message() {
        let all = [
            StartFailure::NoPreset,
            StartFailure::Busy,
            StartFailure::Exited,
            StartFailure::Unquotable,
            StartFailure::WriteFailed,
            StartFailure::NoAccount,
        ];
        let mut messages: Vec<_> = all.iter().map(|failure| failure.message()).collect();
        messages.sort_unstable();
        messages.dedup();

        assert_eq!(messages.len(), all.len());
        assert!(messages.iter().all(|message| message.len() < 48));
    }
}
