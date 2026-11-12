//! UI toggles that survive a relaunch (minimap and HUD visibility).

use serde::{Deserialize, Serialize};

/// Visibility the minimap has when nothing was persisted.
const DEFAULT_MINIMAP_VISIBLE: bool = true;
/// Visibility the HUD has when nothing was persisted.
const DEFAULT_HUD_VISIBLE: bool = false;

/// Persisted user choices for fixed canvas overlays.
///
/// A field is `None` while the user never moved it away from the built-in
/// default, so a pristine state file carries no `ui` block at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct UiPrefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimap_visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hud_visible: Option<bool>,
}

impl UiPrefs {
    /// Captures the live toggles, keeping only those that differ from the defaults.
    #[must_use]
    pub fn from_visibility(minimap_visible: bool, hud_visible: bool) -> Self {
        Self {
            minimap_visible: (minimap_visible != DEFAULT_MINIMAP_VISIBLE).then_some(minimap_visible),
            hud_visible: (hud_visible != DEFAULT_HUD_VISIBLE).then_some(hud_visible),
        }
    }

    #[must_use]
    pub fn minimap_visible_or_default(&self) -> bool {
        self.minimap_visible.unwrap_or(DEFAULT_MINIMAP_VISIBLE)
    }

    #[must_use]
    pub fn hud_visible_or_default(&self) -> bool {
        self.hud_visible.unwrap_or(DEFAULT_HUD_VISIBLE)
    }

    /// Serde predicate: nothing to persist.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
