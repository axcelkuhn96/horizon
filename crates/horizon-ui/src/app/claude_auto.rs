//! The "Auto" Claude account in the app: the row text the launcher and the titlebar strip show
//! (cached with their rows, rebuilt when the usage data moves) and the pick itself, which is
//! always re-evaluated at click time. The rules live in `horizon_core::claude_usage::pick_auto`.
//! Visual spec: `docs/design/claude-hub-ui.md` §12.

use horizon_core::claude_usage::{AutoPick, AutoReason, ProfileUsageView, pick_auto};

use super::HorizonApp;
use super::claude_launcher::claude_profile_names;

/// Name of the Auto entry in the launcher and the strip.
pub(super) const AUTO_NAME: &str = "Auto";
/// Profiles needed before Auto is offered: with one there is nothing to choose between.
pub(super) const MIN_PROFILES_FOR_AUTO: usize = 2;
/// Below this much weekly quota left the chosen account's text turns yellow.
const LOW_REMAINING_PCT: f32 = 20.0;
const NO_ACCOUNT_TEXT: &str = "No account available";

/// What the Auto row says right now: the account it would pick and why, or why it cannot.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AutoChoice {
    /// Profile Auto would pick at build time; informational, the click re-evaluates.
    pub(super) target: Option<String>,
    /// `→ conta2 · 66% left`, or `No account available`.
    pub(super) detail: String,
    /// The chosen account has little weekly quota left.
    pub(super) low: bool,
    /// Hover text: the rule, plus why each skipped profile is skipped.
    pub(super) tooltip: String,
}

impl AutoChoice {
    pub(super) fn from_views(views: &[ProfileUsageView]) -> Self {
        let pick = pick_auto(views);
        let (detail, low) = match &pick {
            Some(AutoPick {
                name,
                remaining_pct,
                reason: AutoReason::MostRemaining,
            }) => (
                format!("\u{2192} {name} \u{b7} {remaining_pct:.0}% left"),
                *remaining_pct < LOW_REMAINING_PCT,
            ),
            Some(AutoPick { name, .. }) => (format!("\u{2192} {name}"), false),
            None => (NO_ACCOUNT_TEXT.to_owned(), false),
        };
        let header = if pick.is_some() {
            "Picks the account with the most weekly quota left"
        } else if views.is_empty() {
            "No Claude profiles"
        } else {
            NO_ACCOUNT_TEXT
        };
        let skipped = views.iter().filter_map(|view| {
            view.skip_reason()
                .map(|reason| format!("{}: {}", view.name, reason.text()))
        });
        let tooltip = std::iter::once(header.to_owned())
            .chain(skipped)
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            target: pick.map(|pick| pick.name),
            detail,
            low,
            tooltip,
        }
    }

    pub(super) fn available(&self) -> bool {
        self.target.is_some()
    }
}

impl HorizonApp {
    /// Usage projection of every listed profile, in config order.
    fn auto_views(&self) -> Vec<ProfileUsageView> {
        claude_profile_names(&self.presets)
            .iter()
            .enumerate()
            .map(|(order, name)| ProfileUsageView::new(name, order, self.hub.usage.usage(name)))
            .collect()
    }

    /// Row text for the current snapshot.
    pub(super) fn auto_choice(&self) -> AutoChoice {
        AutoChoice::from_views(&self.auto_views())
    }

    /// The account Auto picks with the snapshot as it is now (never a value cached on a row).
    pub(super) fn current_auto_pick(&self) -> Option<AutoPick> {
        pick_auto(&self.auto_views())
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::claude_usage::UsageState;

    use super::*;

    fn view(name: &str, order: usize, state: UsageState, weekly: Option<f32>) -> ProfileUsageView {
        ProfileUsageView {
            name: name.to_owned(),
            state,
            weekly_pct: weekly,
            five_hour_pct: None,
            resets_at: None,
            order,
        }
    }

    #[test]
    fn auto_row_label_formats_remaining_and_reason() {
        let views = [
            view("conta1", 0, UsageState::Ok, Some(34.0)),
            view("conta2", 1, UsageState::Ok, Some(12.0)),
        ];

        let choice = AutoChoice::from_views(&views);

        assert_eq!(choice.detail, "\u{2192} conta2 \u{b7} 88% left");
        assert_eq!(choice.target.as_deref(), Some("conta2"));
        assert!(!choice.low && choice.available());

        let nearly_spent = [view("conta1", 0, UsageState::Ok, Some(85.0))];
        let choice = AutoChoice::from_views(&nearly_spent);
        assert_eq!(choice.detail, "\u{2192} conta1 \u{b7} 15% left");
        assert!(choice.low);

        let no_data = [view("conta1", 0, UsageState::Ok, None)];
        let choice = AutoChoice::from_views(&no_data);
        assert_eq!(choice.detail, "\u{2192} conta1");
        assert!(!choice.low);
    }

    #[test]
    fn auto_row_disabled_when_none_usable() {
        let views = [
            view("conta1", 0, UsageState::Expired, Some(10.0)),
            view("conta2", 1, UsageState::Missing, None),
            view("conta3", 2, UsageState::Ok, Some(100.0)),
        ];

        let choice = AutoChoice::from_views(&views);

        assert!(!choice.available());
        assert_eq!(choice.detail, "No account available");
        assert!(choice.tooltip.contains("conta1: expired"));
        assert!(choice.tooltip.contains("conta2: no login"));
        assert!(choice.tooltip.contains("conta3: limit reached"));
        assert_eq!(AutoChoice::from_views(&[]).tooltip, "No Claude profiles");
    }

    #[test]
    fn auto_row_tooltip_names_skipped_profiles_even_when_one_is_picked() {
        let views = [
            view("conta1", 0, UsageState::Expired, None),
            view("conta2", 1, UsageState::Ok, Some(10.0)),
        ];

        let choice = AutoChoice::from_views(&views);

        assert!(choice.tooltip.starts_with("Picks the account"));
        assert!(choice.tooltip.contains("conta1: expired"));
        assert!(!choice.tooltip.contains("conta2:"));
    }
}
