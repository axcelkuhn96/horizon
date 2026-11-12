//! State of the quick-start strip owned by `HubState`: the profile rows of the one open strip, its
//! layout cache, the collapsed button's cached layout, the busy probes and the transient notice
//! shown when a start fails.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Painter};
use horizon_core::PanelId;

use crate::app::claude_launcher::RowUsage;
use crate::app::usage_chips::account_id;

use super::placement::{StripLayout, build_layout, launch_caption};
use super::titlebar::BusyCache;
use super::{Stage, StartFailure};

const ACCOUNT_ID_MAX_CHARS: usize = 16;
const NO_USAGE_TEXT: &str = "\u{2014}";
/// How long a failure notice stays in the titlebar.
pub(super) const NOTICE_TTL: Duration = Duration::from_secs(3);

/// One profile; built when the strip opens (and when the usage data moves), never per frame.
pub(super) struct Row {
    pub(super) name: String,
    pub(super) accent: Color32,
    /// Account identifier (e-mail local part).
    pub(super) account: Option<String>,
    /// Expired or missing login: drawn dimmed.
    pub(super) dim: bool,
    /// Hover text: the account and the 7d usage (the 5h window is never shown here).
    pub(super) tooltip: String,
}

impl Row {
    pub(super) fn new(name: String, accent: Color32, email: Option<&str>, usage: &RowUsage) -> Self {
        let status = match usage.problem {
            Some(problem) => problem.text().to_owned(),
            None => {
                let week = &usage.lines[1];
                if week.text == NO_USAGE_TEXT {
                    "no usage data yet".to_owned()
                } else {
                    format!("7d {}", week.text)
                }
            }
        };
        let email: String = email
            .map(|email| email.chars().filter(|ch| !ch.is_control()).collect())
            .unwrap_or_default();
        let account = if email.is_empty() {
            "account unknown".to_owned()
        } else {
            email.clone()
        };
        Self {
            tooltip: format!("{name}\n{account}\n{status}"),
            account: account_id((!email.is_empty()).then_some(email.as_str()), ACCOUNT_ID_MAX_CHARS),
            name,
            accent,
            dim: usage.problem.is_some(),
        }
    }
}

/// Layout cache key: stage, room, pixel density, rows version.
type LayoutKey = (Stage, u32, u32, u64);

/// The strip of the one panel that has it open.
pub(super) struct Expanded {
    pub(super) panel_id: PanelId,
    pub(super) stage: Stage,
    pub(super) rows: Vec<Row>,
    /// One row per saved prompt, listed by the prompt pages.
    pub(super) prompts: Vec<Row>,
    /// `HubState::rows_version` the rows were built from.
    pub(super) rows_version: u64,
    layout: Option<(LayoutKey, Arc<StripLayout>)>,
}

impl Expanded {
    pub(super) fn new(panel_id: PanelId, rows: Vec<Row>, rows_version: u64) -> Self {
        Self {
            panel_id,
            stage: Stage::Profiles,
            rows,
            prompts: Vec::new(),
            rows_version,
            layout: None,
        }
    }

    /// Replaces the prompt rows; the cached layout may include them, so it is dropped.
    pub(super) fn set_prompts(&mut self, prompts: Vec<Row>) {
        self.prompts = prompts;
        self.layout = None;
    }

    /// The rows `stage` lists: prompts on the prompt pages, profiles elsewhere.
    pub(super) fn rows_for(&self, stage: Stage) -> &[Row] {
        if stage.is_prompt_stage() {
            &self.prompts
        } else {
            &self.rows
        }
    }

    /// Swaps in rebuilt rows; the cached layout belongs to the old ones.
    pub(super) fn replace_rows(&mut self, stage: Stage, rows: Vec<Row>, rows_version: u64) {
        self.stage = stage;
        self.rows = rows;
        self.rows_version = rows_version;
        self.layout = None;
    }
}

/// Why and where a start failed, until `until`.
struct Notice {
    panel_id: PanelId,
    failure: StartFailure,
    until: Instant,
}

/// Quick-start state owned by `HubState`.
#[derive(Default)]
pub(in crate::app) struct QuickStartUi {
    pub(super) expanded: Option<Expanded>,
    pub(super) busy: BusyCache,
    /// The collapsed button's layout (with its measured caption), keyed by pixel density.
    launch: Option<(u32, Arc<StripLayout>)>,
    notice: Option<Notice>,
}

impl QuickStartUi {
    pub(super) fn stage_for(&self, panel_id: PanelId) -> Stage {
        self.expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == panel_id)
            .map_or(Stage::Collapsed, |expanded| expanded.stage)
    }

    pub(super) fn is_open_for(&self, panel_id: PanelId) -> bool {
        self.stage_for(panel_id) != Stage::Collapsed
    }

    /// Collapses the strip when it belongs to `panel_id`.
    pub(super) fn collapse(&mut self, panel_id: PanelId) {
        if self
            .expanded
            .as_ref()
            .is_some_and(|expanded| expanded.panel_id == panel_id)
        {
            self.expanded = None;
        }
    }

    /// Forgets everything kept for panels that no longer exist: the open strip, a pending notice
    /// and the busy probes.
    pub(in crate::app) fn drop_closed(&mut self, panel_count: usize, live: impl Fn(PanelId) -> bool) {
        if self.expanded.as_ref().is_some_and(|expanded| !live(expanded.panel_id)) {
            self.expanded = None;
        }
        if self.notice.as_ref().is_some_and(|notice| !live(notice.panel_id)) {
            self.notice = None;
        }
        self.busy.evict_closed(panel_count, live);
    }

    /// Shows `failure` next to `panel_id`'s strip for [`NOTICE_TTL`].
    pub(super) fn show_failure(&mut self, panel_id: PanelId, failure: StartFailure, now: Instant) {
        self.notice = Some(Notice {
            panel_id,
            failure,
            until: now + NOTICE_TTL,
        });
    }

    /// The failure to show for `panel_id` and how long it has left; clears an expired one.
    pub(super) fn notice_for(&mut self, panel_id: PanelId, now: Instant) -> Option<(StartFailure, Duration)> {
        let (owner, failure, until) = self
            .notice
            .as_ref()
            .map(|notice| (notice.panel_id, notice.failure, notice.until))?;
        if now >= until {
            self.notice = None;
            return None;
        }
        (owner == panel_id).then(|| (failure, until - now))
    }

    /// The collapsed button's layout; the caption is measured once per pixel density.
    fn launch_layout(&mut self, painter: &Painter) -> Arc<StripLayout> {
        let ppp = painter.ctx().pixels_per_point().to_bits();
        if let Some((cached, layout)) = &self.launch
            && *cached == ppp
        {
            return Arc::clone(layout);
        }
        let layout = Arc::new(StripLayout::launch(launch_caption(painter)));
        self.launch = Some((ppp, Arc::clone(&layout)));
        layout
    }

    /// Layout of `panel_id`'s strip for `stage` inside `room`; rebuilt only when the stage, the
    /// room, the pixel density or the rows change. A collapsed strip allocates nothing per frame.
    pub(super) fn layout_for(
        &mut self,
        painter: &Painter,
        panel_id: PanelId,
        stage: Stage,
        room: f32,
    ) -> Arc<StripLayout> {
        let ppp = painter.ctx().pixels_per_point().to_bits();
        if stage != Stage::Collapsed
            && let Some(expanded) = self.expanded.as_mut().filter(|expanded| expanded.panel_id == panel_id)
        {
            let key = (stage, room.to_bits(), ppp, expanded.rows_version);
            if let Some((cached, layout)) = &expanded.layout
                && *cached == key
            {
                return Arc::clone(layout);
            }
            let layout = Arc::new(build_layout(painter, stage, &expanded.rows, &expanded.prompts, room));
            expanded.layout = Some((key, Arc::clone(&layout)));
            return layout;
        }
        self.launch_layout(painter)
    }
}

#[cfg(test)]
mod tests {
    use horizon_core::claude_usage::{ProfileUsage, UsageState, UsageWindow};

    use super::*;

    fn usage(state: UsageState, five: Option<f32>, seven: Option<f32>) -> ProfileUsage {
        let window = |percent| UsageWindow {
            percent,
            resets_at: None,
        };
        ProfileUsage {
            name: "claude2".to_owned(),
            display_name: None,
            email: None,
            five_hour: window(five),
            seven_day: window(seven),
            severity: None,
            state,
            fetched_at_ms: Some(1_000),
        }
    }

    fn row(snapshot: Option<&ProfileUsage>, email: Option<&str>) -> Row {
        Row::new("claude2".to_owned(), Color32::WHITE, email, &RowUsage::new(snapshot))
    }

    #[test]
    fn prompt_pages_list_the_prompt_rows_and_the_others_the_profiles() {
        let mut expanded = Expanded::new(PanelId(1), vec![row(None, None)], 0);
        expanded.set_prompts(vec![row(None, None), row(None, None)]);

        assert_eq!(expanded.rows_for(Stage::Profiles).len(), 1);
        assert_eq!(expanded.rows_for(Stage::Modes { profile: 0 }).len(), 1);
        assert_eq!(expanded.rows_for(Stage::Prompts).len(), 2);
        assert_eq!(expanded.rows_for(Stage::PromptModes { prompt: 1 }).len(), 2);
    }

    #[test]
    fn tooltip_names_the_account_and_the_weekly_usage_only() {
        let snapshot = usage(UsageState::Ok, Some(37.0), Some(42.0));

        let row = row(Some(&snapshot), Some("gutosjc@example.com"));

        assert_eq!(row.tooltip, "claude2\ngutosjc@example.com\n7d 42%");
        assert!(!row.tooltip.contains("5h"));
        assert_eq!(row.account.as_deref(), Some("gutosjc"));
        assert!(!row.dim);
    }

    #[test]
    fn tooltip_without_a_five_hour_window_still_reads_the_weekly_one() {
        let snapshot = usage(UsageState::Ok, None, Some(12.0));

        assert!(row(Some(&snapshot), None).tooltip.ends_with("7d 12%"));
    }

    #[test]
    fn expired_and_missing_logins_dim_the_chip_and_say_why() {
        let expired = usage(UsageState::Expired, Some(10.0), Some(20.0));
        let missing = usage(UsageState::Missing, None, None);

        let expired = row(Some(&expired), None);
        let missing = row(Some(&missing), None);

        assert!(expired.dim && expired.tooltip.ends_with("expired"));
        assert!(missing.dim && missing.tooltip.ends_with("no login"));
    }

    #[test]
    fn without_usage_the_tooltip_says_so_and_the_account_is_unknown() {
        let row = row(None, None);

        assert_eq!(row.tooltip, "claude2\naccount unknown\nno usage data yet");
        assert_eq!(row.account, None);
        assert!(!row.dim);
    }

    #[test]
    fn control_characters_never_reach_the_tooltip() {
        let row = row(None, Some("a\nb@example.com"));

        assert!(row.tooltip.contains("ab@example.com"));
        assert_eq!(row.tooltip.lines().count(), 3);
    }

    fn open_strip(panel: u64, stage: Stage) -> QuickStartUi {
        let mut ui = QuickStartUi::default();
        let mut expanded = Expanded::new(PanelId(panel), vec![row(None, None)], 1);
        expanded.stage = stage;
        ui.expanded = Some(expanded);
        ui
    }

    #[test]
    fn only_the_owning_panel_sees_the_open_stage() {
        let ui = open_strip(1, Stage::Modes { profile: 0 });

        assert_eq!(ui.stage_for(PanelId(1)), Stage::Modes { profile: 0 });
        assert_eq!(ui.stage_for(PanelId(2)), Stage::Collapsed);
        assert!(ui.is_open_for(PanelId(1)));
        assert!(!ui.is_open_for(PanelId(2)));
    }

    #[test]
    fn collapsing_another_panel_keeps_the_open_strip() {
        let mut ui = open_strip(1, Stage::Profiles);

        ui.collapse(PanelId(2));
        assert!(ui.expanded.is_some());
        ui.collapse(PanelId(1));
        assert!(ui.expanded.is_none());
    }

    #[test]
    fn closed_panels_lose_their_strip_notice_and_probes() {
        let now = Instant::now();
        let mut ui = open_strip(1, Stage::Profiles);
        ui.show_failure(PanelId(1), StartFailure::Busy, now);
        assert!(ui.busy.is_busy(PanelId(1), now, true, || true));
        assert!(ui.busy.is_busy(PanelId(2), now, true, || true));

        ui.drop_closed(1, |id| id == PanelId(2));

        assert!(ui.expanded.is_none());
        assert_eq!(ui.notice_for(PanelId(1), now), None);
        assert_eq!(ui.busy.len(), 1);
    }

    #[test]
    fn a_notice_belongs_to_one_panel_and_expires() {
        let now = Instant::now();
        let mut ui = QuickStartUi::default();
        ui.show_failure(PanelId(1), StartFailure::NoPreset, now);

        assert_eq!(
            ui.notice_for(PanelId(1), now + Duration::from_secs(1)),
            Some((StartFailure::NoPreset, NOTICE_TTL - Duration::from_secs(1)))
        );
        assert_eq!(ui.notice_for(PanelId(2), now), None);
        assert_eq!(ui.notice_for(PanelId(1), now + NOTICE_TTL), None);
        assert!(ui.notice.is_none(), "an expired notice is dropped");
    }
}
