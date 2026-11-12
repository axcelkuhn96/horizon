//! The quick-start strip inside the titlebar row: state, layout cache, per-frame interaction and
//! the write into the panel's PTY. Widths are planned in [`super::layout`], drawing is in
//! [`super::paint`].

use std::sync::Arc;
use std::time::Instant;

use egui::{
    Color32, Context, FontId, Galley, Key, Modifiers, Painter, Pos2, Rect, Sense, Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::link::{LinkRequest, LinkResponse};
use horizon_core::{PanelId, PanelKind};

use crate::app::HorizonApp;
use crate::app::claude_launcher::{RowUsage, claude_profile_names, find_profile_preset};
use crate::app::hub_ui;
use crate::app::usage_chips::account_id;
use crate::theme;

use super::layout::{
    CHIP_CHROME, CLOSE_WIDTH, ChipSize, ITEM_GAP, LEAD_CHROME, MARK_WIDTH, MODE_PAD, MORE_WIDTH, NAME_MIN_VISIBLE,
    modes_total, plan_chips, plan_modes, profiles_total,
};
use super::titlebar::{self, BUSY_TOOLTIP, BUTTON_WIDTH, BusyCache, READY_TOOLTIP};
use super::{QuickStartMode, Stage, StartRequest, StripEvent, quick_start_command, quick_start_enabled};

const NAME_SIZE: f32 = 11.0;
const ID_SIZE: f32 = 10.0;
const MODE_SIZE: f32 = 10.5;
const ACCOUNT_ID_MAX_CHARS: usize = 16;
const NO_USAGE_TEXT: &str = "\u{2014}";

/// What a strip item is, and so what a click on it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemKind {
    /// The collapsed "Claude" button.
    Launch,
    /// The star that collapses the profile chips.
    Mark,
    Chip(usize),
    /// The profile name on the mode page.
    Back,
    Mode(QuickStartMode),
    Close,
    /// `+N` for profiles that did not fit.
    More(usize),
}

impl ItemKind {
    fn interactive(self) -> bool {
        !matches!(self, Self::More(_))
    }

    fn event(self) -> Option<StripEvent> {
        match self {
            Self::Launch | Self::Mark => Some(StripEvent::Toggle),
            Self::Chip(index) => Some(StripEvent::Pick(index)),
            Self::Back => Some(StripEvent::Back),
            Self::Mode(mode) => Some(StripEvent::Mode(mode)),
            Self::Close => Some(StripEvent::Dismiss),
            Self::More(_) => None,
        }
    }
}

/// One item of the strip, positioned relative to the strip's left edge.
struct Placed {
    kind: ItemKind,
    x: f32,
    w: f32,
    /// Profile name or button caption, already elided to the item's width.
    text: Option<Arc<Galley>>,
    /// Account identifier, only when there was room for it.
    id: Option<Arc<Galley>>,
}

struct StripLayout {
    items: Vec<Placed>,
    width: f32,
}

impl StripLayout {
    fn launch() -> Self {
        Self {
            items: vec![Placed {
                kind: ItemKind::Launch,
                x: 0.0,
                w: BUTTON_WIDTH,
                text: None,
                id: None,
            }],
            width: BUTTON_WIDTH,
        }
    }
}

/// One profile; built when the strip opens (and when the usage data moves), never per frame.
struct Row {
    name: String,
    accent: Color32,
    /// Account identifier (e-mail local part).
    account: Option<String>,
    /// Expired or missing login: drawn dimmed.
    dim: bool,
    /// Hover text: the account and the 7d usage (the 5h window is never shown here).
    tooltip: String,
}

impl Row {
    fn new(name: String, accent: Color32, email: Option<&str>, usage: &RowUsage) -> Self {
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

type LayoutKey = (Stage, u32, u32, u64);

/// The strip of the one panel that has it open.
struct Expanded {
    panel_id: PanelId,
    stage: Stage,
    rows: Vec<Row>,
    /// `HubState::rows_version` the rows were built from.
    rows_version: u64,
    layout: Option<(LayoutKey, Arc<StripLayout>)>,
}

/// Quick-start state owned by `HubState`.
pub(in crate::app) struct QuickStartUi {
    expanded: Option<Expanded>,
    busy: BusyCache,
    launch: Arc<StripLayout>,
}

impl Default for QuickStartUi {
    fn default() -> Self {
        Self {
            expanded: None,
            busy: BusyCache::default(),
            launch: Arc::new(StripLayout::launch()),
        }
    }
}

impl QuickStartUi {
    fn stage_for(&self, panel_id: PanelId) -> Stage {
        self.expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == panel_id)
            .map_or(Stage::Collapsed, |expanded| expanded.stage)
    }

    /// Collapses the strip when it belongs to `panel_id`.
    fn collapse(&mut self, panel_id: PanelId) {
        if self
            .expanded
            .as_ref()
            .is_some_and(|expanded| expanded.panel_id == panel_id)
        {
            self.expanded = None;
        }
    }

    /// Layout of `panel_id`'s strip for `stage` inside `room`; rebuilt only when the stage, the
    /// room, the pixel density or the rows change.
    fn layout_for(&mut self, painter: &Painter, panel_id: PanelId, stage: Stage, room: f32) -> Arc<StripLayout> {
        let Some(expanded) = self
            .expanded
            .as_mut()
            .filter(|expanded| expanded.panel_id == panel_id && stage != Stage::Collapsed)
        else {
            return Arc::clone(&self.launch);
        };
        let key = (
            stage,
            room.to_bits(),
            painter.ctx().pixels_per_point().to_bits(),
            expanded.rows_version,
        );
        if let Some((cached, layout)) = &expanded.layout
            && *cached == key
        {
            return Arc::clone(layout);
        }
        let layout = Arc::new(build_layout(painter, stage, &expanded.rows, room));
        expanded.layout = Some((key, Arc::clone(&layout)));
        layout
    }
}

/// One item as drawn this frame.
pub(super) struct ViewItem {
    pub(super) rect: Rect,
    pub(super) kind: ItemKind,
    pub(super) accent: Color32,
    pub(super) text: Option<Arc<Galley>>,
    pub(super) id: Option<Arc<Galley>>,
    pub(super) dim: bool,
    pub(super) hovered: bool,
    pub(super) pressed: bool,
    pub(super) focused: bool,
}

/// Everything the painter needs for one panel's strip.
pub(in crate::app) struct StripView {
    /// Screen-independent (panel layer) rect of the whole strip.
    pub(in crate::app) rect: Rect,
    pub(super) items: Vec<ViewItem>,
    /// `false` while a command runs in the shell: the button is drawn faded and inert.
    pub(super) enabled: bool,
    /// An item was clicked this frame.
    pub(in crate::app) clicked: bool,
}

/// What the panel draw code tells the strip about the titlebar this frame.
pub(in crate::app) struct StripArgs {
    pub(in crate::app) panel_id: PanelId,
    pub(in crate::app) panel_width: f32,
    /// Collapsed or renaming panel: no strip.
    pub(in crate::app) hidden: bool,
    pub(in crate::app) titlebar: Rect,
    /// X where the strip ends (left of the history meter or the controls).
    pub(in crate::app) right: f32,
    pub(in crate::app) interactive: bool,
}

fn measure(painter: &Painter, text: &str, size: f32) -> Arc<Galley> {
    painter.layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::PLACEHOLDER)
}

/// `galley` when it fits `room`, else `text` elided to it; `None` when `room` is too small to
/// show any text.
fn fitted(painter: &Painter, galley: &Arc<Galley>, text: &str, size: f32, room: f32) -> Option<Arc<Galley>> {
    if room < NAME_MIN_VISIBLE {
        return None;
    }
    if galley.size().x <= room {
        Some(Arc::clone(galley))
    } else {
        Some(hub_ui::elided(painter, text, FontId::proportional(size), room))
    }
}

fn build_layout(painter: &Painter, stage: Stage, rows: &[Row], room: f32) -> StripLayout {
    match stage {
        Stage::Collapsed => StripLayout::launch(),
        Stage::Modes { profile } => rows.get(profile).map_or_else(
            || profiles_layout(painter, rows, room),
            |row| modes_layout(painter, row, room),
        ),
        Stage::Profiles => profiles_layout(painter, rows, room),
    }
}

fn profiles_layout(painter: &Painter, rows: &[Row], room: f32) -> StripLayout {
    let names: Vec<Arc<Galley>> = rows.iter().map(|row| measure(painter, &row.name, NAME_SIZE)).collect();
    let ids: Vec<Option<Arc<Galley>>> = rows
        .iter()
        .map(|row| row.account.as_deref().map(|account| measure(painter, account, ID_SIZE)))
        .collect();
    let sizes: Vec<ChipSize> = names
        .iter()
        .zip(&ids)
        .map(|(name, id)| ChipSize {
            name: name.size().x,
            id: id.as_ref().map(|id| id.size().x),
        })
        .collect();
    let plan = plan_chips(room - MARK_WIDTH - ITEM_GAP, &sizes);
    let mut items = vec![Placed {
        kind: ItemKind::Mark,
        x: 0.0,
        w: MARK_WIDTH,
        text: None,
        id: None,
    }];
    let mut x = MARK_WIDTH + ITEM_GAP;
    for (index, (((row, name), id), width)) in rows
        .iter()
        .zip(&names)
        .zip(&ids)
        .zip(&plan.widths)
        .take(plan.shown)
        .enumerate()
    {
        let (text, id) = if plan.show_ids {
            (Some(Arc::clone(name)), id.clone())
        } else {
            (fitted(painter, name, &row.name, NAME_SIZE, width - CHIP_CHROME), None)
        };
        items.push(Placed {
            kind: ItemKind::Chip(index),
            x,
            w: *width,
            text,
            id,
        });
        x += width + ITEM_GAP;
    }
    if plan.shown < rows.len() {
        let hidden = rows.len() - plan.shown;
        items.push(Placed {
            kind: ItemKind::More(hidden),
            x,
            w: MORE_WIDTH,
            text: Some(measure(painter, &format!("+{hidden}"), ID_SIZE)),
            id: None,
        });
    }
    StripLayout {
        items,
        width: profiles_total(&plan),
    }
}

fn modes_layout(painter: &Painter, row: &Row, room: f32) -> StripLayout {
    let name = measure(painter, &row.name, NAME_SIZE);
    let captions: Vec<Arc<Galley>> = QuickStartMode::ALL
        .iter()
        .map(|mode| measure(painter, mode.inline_label(), MODE_SIZE))
        .collect();
    let natural: Vec<f32> = captions
        .iter()
        .map(|caption| caption.size().x + MODE_PAD * 2.0)
        .collect();
    let (lead, widths) = plan_modes(room, LEAD_CHROME + name.size().x, &natural);
    let mut items = vec![Placed {
        kind: ItemKind::Back,
        x: 0.0,
        w: lead,
        text: fitted(painter, &name, &row.name, NAME_SIZE, lead - LEAD_CHROME),
        id: None,
    }];
    let mut x = lead + ITEM_GAP;
    for ((mode, caption), width) in QuickStartMode::ALL.iter().zip(&captions).zip(&widths) {
        let inner = width - MODE_PAD * 2.0;
        let text = if caption.size().x <= inner {
            Arc::clone(caption)
        } else {
            hub_ui::elided(painter, mode.inline_label(), FontId::proportional(MODE_SIZE), inner)
        };
        items.push(Placed {
            kind: ItemKind::Mode(*mode),
            x,
            w: *width,
            text: Some(text),
            id: None,
        });
        x += width + ITEM_GAP;
    }
    items.push(Placed {
        kind: ItemKind::Close,
        x,
        w: CLOSE_WIDTH,
        text: None,
        id: None,
    });
    StripLayout {
        items,
        width: modes_total(lead, &widths),
    }
}

/// Profile an item belongs to, for its colour and dimming.
fn row_of(rows: &[Row], stage: Stage, kind: ItemKind) -> Option<&Row> {
    match (kind, stage) {
        (ItemKind::Chip(index), _) => rows.get(index),
        (ItemKind::Back | ItemKind::Mode(_), Stage::Modes { profile }) => rows.get(profile),
        _ => None,
    }
}

fn accessible_label(kind: ItemKind, row: Option<&Row>) -> String {
    let name = row.map_or("", |row| row.name.as_str());
    match kind {
        ItemKind::Launch => "Start Claude in this terminal".to_owned(),
        ItemKind::Mark => "Collapse the Claude profiles".to_owned(),
        ItemKind::Chip(_) => format!("Start Claude with {name}"),
        ItemKind::Back => format!("{name}: back to profiles"),
        ItemKind::Mode(mode) => format!("{} Claude session with {name}", mode.label()),
        ItemKind::Close => "Close the Claude quick start".to_owned(),
        ItemKind::More(count) => format!("{count} more profiles"),
    }
}

fn is_profile(preset: &horizon_core::PresetConfig) -> bool {
    preset.kind == PanelKind::Claude && preset.env.contains_key("CLAUDE_CONFIG_DIR")
}

impl HorizonApp {
    /// Titlebar button state for `panel_id`: `None` hides it, `Some(enabled)` draws it.
    fn quick_start_button(&mut self, panel_id: PanelId, panel_width: f32) -> Option<bool> {
        if !titlebar::panel_is_wide_enough(panel_width) {
            return None;
        }
        let panel = self.board.panel(panel_id)?;
        let is_ssh = panel.kind == PanelKind::Ssh || panel.ssh_connection.is_some();
        let profiles = self.presets.iter().filter(|preset| is_profile(preset)).count();
        if !super::quick_start_visible(panel.kind, is_ssh, profiles) {
            return None;
        }
        let (kind, child_pid, exited) = (panel.kind, panel.child_pid(), panel.child_exited());
        let busy = exited
            || self.hub.quick_start.busy.is_busy(panel_id, Instant::now(), || {
                child_pid.is_some_and(titlebar::shell_is_busy)
            });
        Some(quick_start_enabled(kind, is_ssh, busy))
    }

    fn build_quick_rows(&mut self, names: Vec<String>) -> Vec<Row> {
        let accents: Vec<Color32> = self
            .hub
            .accent_slots(&names)
            .iter()
            .map(|slot| hub_ui::accent_color(*slot))
            .collect();
        names
            .into_iter()
            .zip(accents)
            .map(|(name, accent)| {
                let usage = RowUsage::new(self.hub.usage.usage(&name));
                let email = self.hub.usage.account_email(&name);
                Row::new(name, accent, email, &usage)
            })
            .collect()
    }

    fn open_quick_start_strip(&mut self, panel_id: PanelId) {
        let names = claude_profile_names(&self.presets);
        if names.is_empty() {
            return;
        }
        let rows = self.build_quick_rows(names);
        self.hub.quick_start.expanded = Some(Expanded {
            panel_id,
            stage: Stage::Profiles,
            rows,
            rows_version: self.hub.rows_version,
            layout: None,
        });
    }

    /// Rebuilds the open strip's rows when the profile list or the usage data moved.
    fn refresh_quick_start_rows(&mut self, panel_id: PanelId) {
        let version = self.hub.rows_version;
        let current = match self
            .hub
            .quick_start
            .expanded
            .as_ref()
            .filter(|expanded| expanded.panel_id == panel_id && expanded.rows_version != version)
        {
            None => return,
            Some(expanded) => match expanded.stage {
                Stage::Modes { profile } => expanded.rows.get(profile).map(|row| row.name.clone()),
                Stage::Collapsed | Stage::Profiles => None,
            },
        };
        let names = claude_profile_names(&self.presets);
        if names.is_empty() {
            self.hub.quick_start.expanded = None;
            return;
        }
        let remembered = current.and_then(|current| names.iter().position(|name| *name == current));
        let rows = self.build_quick_rows(names);
        if let Some(expanded) = self.hub.quick_start.expanded.as_mut() {
            expanded.stage = remembered.map_or(Stage::Profiles, |profile| Stage::Modes { profile });
            expanded.rows = rows;
            expanded.rows_version = version;
            expanded.layout = None;
        }
    }

    /// Draws-and-senses the quick-start strip of one panel's titlebar. Returns what to paint, or
    /// `None` when this panel gets no strip (not a local idle-capable shell, too narrow, hidden).
    pub(in crate::app) fn quick_start_strip(&mut self, ui: &Ui, args: &StripArgs) -> Option<StripView> {
        let panel_id = args.panel_id;
        let ctx = ui.ctx().clone();
        let enabled = if args.hidden {
            None
        } else {
            self.quick_start_button(panel_id, args.panel_width)
        };
        let Some(enabled) = enabled else {
            self.hub.quick_start.collapse(panel_id);
            return None;
        };
        if !enabled {
            self.hub.quick_start.collapse(panel_id);
        }
        let clickable = args.interactive && enabled;
        self.refresh_quick_start_rows(panel_id);

        let stage = self.hub.quick_start.stage_for(panel_id);
        let room = titlebar::strip_room(args.titlebar, args.right);
        let layout = self.hub.quick_start.layout_for(ui.painter(), panel_id, stage, room);
        let rect = titlebar::strip_rect(args.titlebar, args.right, layout.width);

        let mut event = None;
        let mut items = Vec::with_capacity(layout.items.len());
        {
            let expanded = self
                .hub
                .quick_start
                .expanded
                .as_ref()
                .filter(|expanded| expanded.panel_id == panel_id);
            let rows: &[Row] = expanded.map_or(&[][..], |expanded| expanded.rows.as_slice());
            for (index, placed) in layout.items.iter().enumerate() {
                let item_rect = Rect::from_min_size(
                    Pos2::new(rect.min.x + placed.x, rect.min.y),
                    Vec2::new(placed.w, rect.height()),
                );
                let live = clickable && placed.kind.interactive();
                let response = ui.interact(
                    item_rect.expand2(Vec2::new(0.0, 2.0)),
                    ui.make_persistent_id(("panel_quick_start", panel_id.0, index)),
                    if live { Sense::click() } else { Sense::hover() },
                );
                let row = row_of(rows, stage, placed.kind);
                response
                    .widget_info(|| WidgetInfo::labeled(WidgetType::Button, live, accessible_label(placed.kind, row)));
                let response = match placed.kind {
                    ItemKind::Launch => response.on_hover_text(if enabled { READY_TOOLTIP } else { BUSY_TOOLTIP }),
                    ItemKind::Mark => response.on_hover_text("Collapse"),
                    ItemKind::Chip(_) => response.on_hover_ui(|ui| {
                        if let Some(row) = row {
                            ui.label(&row.tooltip);
                        }
                    }),
                    ItemKind::Back => response.on_hover_text("Back to profiles"),
                    ItemKind::Mode(mode) => response.on_hover_text(mode.hint()),
                    ItemKind::Close => response.on_hover_text("Close (Esc)"),
                    ItemKind::More(count) => response.on_hover_ui(|ui| {
                        ui.label(format!("{count} more profiles: widen the panel to pick them"));
                    }),
                };
                if live && event.is_none() && response.clicked() {
                    event = placed.kind.event();
                }
                items.push(ViewItem {
                    rect: item_rect,
                    kind: placed.kind,
                    accent: row.map_or_else(theme::ACCENT, |row| row.accent),
                    text: placed.text.clone(),
                    id: placed.id.clone(),
                    dim: row.is_some_and(|row| row.dim),
                    hovered: live && response.hovered(),
                    pressed: live && response.is_pointer_button_down_on(),
                    focused: response.has_focus(),
                });
            }
        }
        let clicked = event.is_some();

        if stage != Stage::Collapsed && event.is_none() {
            if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
                event = Some(StripEvent::Dismiss);
            } else if ctx.input(|input| input.pointer.any_pressed()) {
                // The pointer is in screen space; the panel layer may be zoomed and panned.
                let screen = ctx
                    .layer_transform_to_global(ui.layer_id())
                    .map_or(rect, |transform| transform * rect);
                let outside = ctx
                    .input(|input| input.pointer.interact_pos())
                    .is_some_and(|pos| !screen.contains(pos));
                if outside {
                    event = Some(StripEvent::Dismiss);
                }
            }
        }
        if let Some(event) = event {
            self.apply_quick_start_event(&ctx, panel_id, stage, event);
            ctx.request_repaint();
        }
        Some(StripView {
            rect,
            items,
            enabled,
            clicked,
        })
    }

    fn apply_quick_start_event(&mut self, ctx: &Context, panel_id: PanelId, stage: Stage, event: StripEvent) {
        if stage == Stage::Collapsed {
            if event == StripEvent::Toggle {
                self.open_quick_start_strip(panel_id);
            }
            return;
        }
        let Some(expanded) = self
            .hub
            .quick_start
            .expanded
            .as_mut()
            .filter(|expanded| expanded.panel_id == panel_id)
        else {
            return;
        };
        let (next, request) = expanded.stage.apply(event, expanded.rows.len());
        let started = request
            .and_then(|StartRequest { profile, mode }| expanded.rows.get(profile).map(|row| (row.name.clone(), mode)));
        expanded.stage = next;
        if next == Stage::Collapsed {
            self.hub.quick_start.expanded = None;
        }
        if let Some((name, mode)) = started {
            self.run_claude_quick_start(ctx, panel_id, &name, mode);
        }
    }

    /// Types the start command into the panel's shell (bracketed paste when the terminal has it,
    /// Enter as a separate write), through the same board path as `horizon link send`.
    fn run_claude_quick_start(&mut self, ctx: &Context, panel_id: PanelId, profile_name: &str, mode: QuickStartMode) {
        let Some(preset) = find_profile_preset(&self.presets, profile_name).cloned() else {
            tracing::warn!(profile = profile_name, "no derived Claude preset for profile");
            return;
        };
        let Some(panel) = self.board.panel(panel_id) else {
            return;
        };
        let is_ssh = panel.kind == PanelKind::Ssh || panel.ssh_connection.is_some();
        // Fresh look: the cached answer may predate a command typed since.
        let busy = panel.child_exited() || panel.child_pid().is_some_and(titlebar::shell_is_busy);
        if !quick_start_enabled(panel.kind, is_ssh, busy) {
            tracing::info!("quick start skipped: the shell is busy");
            return;
        }
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let Some(command) = quick_start_command(&preset, mode, &home) else {
            tracing::warn!(profile = profile_name, "quick start command could not be quoted safely");
            return;
        };
        let target = panel.local_id.clone();
        let response = self.board.handle_link_request(LinkRequest::Send {
            target,
            text: command,
            enter: true,
            from: None,
        });
        if let LinkResponse::Err { message, .. } = response {
            tracing::warn!(%message, "quick start could not write to the panel");
            return;
        }
        self.hub.quick_start.busy.forget(panel_id);
        self.board.focus(panel_id);
        ctx.request_repaint();
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

    #[test]
    fn only_the_marker_is_inert() {
        assert!(ItemKind::Launch.interactive());
        assert!(ItemKind::Chip(3).interactive());
        assert!(!ItemKind::More(2).interactive());
        assert_eq!(ItemKind::More(2).event(), None);
        assert_eq!(ItemKind::Close.event(), Some(StripEvent::Dismiss));
        assert_eq!(ItemKind::Chip(3).event(), Some(StripEvent::Pick(3)));
    }

    #[test]
    fn items_resolve_their_profile_from_the_stage() {
        let rows = vec![row(None, None), row(None, None)];

        assert!(row_of(&rows, Stage::Profiles, ItemKind::Chip(1)).is_some());
        assert!(row_of(&rows, Stage::Profiles, ItemKind::Chip(2)).is_none());
        assert!(row_of(&rows, Stage::Modes { profile: 1 }, ItemKind::Back).is_some());
        assert!(row_of(&rows, Stage::Profiles, ItemKind::Mode(QuickStartMode::New)).is_none());
        assert!(row_of(&rows, Stage::Modes { profile: 0 }, ItemKind::Close).is_none());
    }
}
