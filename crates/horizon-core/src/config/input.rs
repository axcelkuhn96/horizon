use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::shortcuts::ShortcutModifiers;

/// Default for [`InputConfig::scroll_pans_over_panels`].
const DEFAULT_SCROLL_PANS_OVER_PANELS: bool = false;
/// Default for [`InputConfig::pan_modifier`].
const DEFAULT_PAN_MODIFIER: &str = "Alt";

fn default_scroll_pans_over_panels() -> bool {
    DEFAULT_SCROLL_PANS_OVER_PANELS
}

fn default_pan_modifier() -> String {
    DEFAULT_PAN_MODIFIER.to_string()
}

/// Pointer/scroll input behaviour for board navigation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct InputConfig {
    /// When `true`, scrolling the wheel over a panel pans the board instead of
    /// being forwarded to the panel.
    #[serde(default = "default_scroll_pans_over_panels")]
    pub scroll_pans_over_panels: bool,
    /// Modifier that, while held, switches pointer drags into board panning.
    /// Parsed via [`InputConfig::resolve_pan_modifier`].
    #[serde(default = "default_pan_modifier")]
    pub pan_modifier: String,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            scroll_pans_over_panels: default_scroll_pans_over_panels(),
            pan_modifier: default_pan_modifier(),
        }
    }
}

impl InputConfig {
    /// Parse the configured [`pan_modifier`](Self::pan_modifier) string into a
    /// [`ShortcutModifiers`] value.
    ///
    /// The error is surfaced from [`Config::validate`](crate::config::Config::validate)
    /// rather than being swallowed with a default fallback, matching the way
    /// `ShortcutsConfig::resolve` validates shortcut strings — an invalid
    /// config should fail loudly at load/validate time rather than silently
    /// behaving differently than the user wrote.
    ///
    /// # Errors
    ///
    /// Returns an error if the configured modifier is not a recognised
    /// modifier name (e.g. `Alt`, `Ctrl`, `Shift`).
    pub fn resolve_pan_modifier(&self) -> Result<ShortcutModifiers> {
        ShortcutModifiers::parse_single(&self.pan_modifier)
    }
}

/// Pure decision: should a wheel/2-finger scroll over a terminal panel pan the
/// board instead of being forwarded to the panel?
///
/// Returns `true` when either the configured `pan_modifier` is currently held
/// (`held_modifiers` contains it) or `scroll_pans_over_panels` is enabled. With
/// neither condition met the scroll is left for the terminal (upstream
/// behaviour: scrollback / PTY), so the result is `false`.
///
/// This is intentionally allocation-free and branch-light: it is evaluated on
/// the input hot path, but only when a scroll event is actually present.
#[must_use]
pub fn scroll_should_pan_canvas(
    held_modifiers: ShortcutModifiers,
    pan_modifier: ShortcutModifiers,
    scroll_pans_over_panels: bool,
) -> bool {
    scroll_pans_over_panels || held_modifiers.contains(pan_modifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_centralized_literals() {
        let input = InputConfig::default();

        assert!(!input.scroll_pans_over_panels);
        assert_eq!(input.pan_modifier, "Alt");
    }

    #[test]
    fn missing_input_section_deserializes_to_defaults() {
        let input: InputConfig = serde_yaml::from_str("{}\n").expect("empty map should deserialize");

        assert_eq!(input, InputConfig::default());
    }

    #[test]
    fn explicit_input_section_round_trips() {
        let yaml = "\
scroll_pans_over_panels: true
pan_modifier: Ctrl
";
        let input: InputConfig = serde_yaml::from_str(yaml).expect("should deserialize");

        assert!(input.scroll_pans_over_panels);
        assert_eq!(input.pan_modifier, "Ctrl");

        let reserialized = serde_yaml::to_string(&input).expect("should serialize");
        let reparsed: InputConfig = serde_yaml::from_str(&reserialized).expect("should re-deserialize");
        assert_eq!(reparsed, input);
    }

    #[test]
    fn legacy_auto_fit_on_focus_key_is_ignored_not_rejected() {
        // The `auto_fit_on_focus` field was removed when directional focus
        // became focus-only (no auto-fit/zoom). Existing user configs may still
        // carry the key; serde must ignore the unknown field rather than fail
        // to load. There is no `#[serde(deny_unknown_fields)]` on this struct or
        // its parents, so the legacy key is silently dropped.
        let yaml = "\
scroll_pans_over_panels: true
pan_modifier: Ctrl
auto_fit_on_focus: true
";
        let input: InputConfig = serde_yaml::from_str(yaml).expect("legacy key should be ignored");

        assert!(input.scroll_pans_over_panels);
        assert_eq!(input.pan_modifier, "Ctrl");
    }

    #[test]
    fn resolve_pan_modifier_parses_alt() {
        let input = InputConfig::default();

        assert_eq!(
            input.resolve_pan_modifier().expect("Alt should resolve"),
            ShortcutModifiers::ALT
        );
    }

    #[test]
    fn scroll_pans_when_pan_modifier_held() {
        assert!(scroll_should_pan_canvas(
            ShortcutModifiers::ALT,
            ShortcutModifiers::ALT,
            false
        ));
    }

    #[test]
    fn scroll_pans_when_toggle_enabled_without_modifier() {
        assert!(scroll_should_pan_canvas(
            ShortcutModifiers::NONE,
            ShortcutModifiers::ALT,
            true
        ));
    }

    #[test]
    fn scroll_does_not_pan_without_modifier_or_toggle() {
        assert!(!scroll_should_pan_canvas(
            ShortcutModifiers::NONE,
            ShortcutModifiers::ALT,
            false
        ));
    }

    #[test]
    fn scroll_does_not_pan_when_only_other_modifier_held() {
        assert!(!scroll_should_pan_canvas(
            ShortcutModifiers::SHIFT,
            ShortcutModifiers::ALT,
            false
        ));
    }

    #[test]
    fn resolve_pan_modifier_rejects_invalid_value() {
        let input = InputConfig {
            pan_modifier: "Bogus".to_string(),
            ..InputConfig::default()
        };

        let error = input
            .resolve_pan_modifier()
            .expect_err("invalid modifier should be a typed error");
        assert!(error.to_string().contains("unsupported modifier"));
    }
}
