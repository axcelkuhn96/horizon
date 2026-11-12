//! Claude launcher: `Ctrl+Shift+L` opens a keyboard-first picker of the
//! configured Claude profiles; the choice opens a session in the active
//! workspace through the regular `add_panel_to_workspace` flow.
//! Visual spec: `docs/design/claude-hub-ui.md` §2.

use std::sync::Arc;

use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Galley, Key, Layout, Margin, Modifiers, Painter, Pos2, Rect,
    Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::claude_usage::{ProfileUsage, UsageState};
use horizon_core::{PanelKind, PresetConfig};

use super::HorizonApp;
use super::hub_ui::{self, ModalPlacement, Severity};
use super::usage_chips::{api_severity, severity_for, window_text};
use crate::theme;

const MODAL_WIDTH: f32 = 480.0;
const HEADER_HEIGHT: f32 = 44.0;
const FOOTER_HEIGHT: f32 = 36.0;
const SEPARATOR_HEIGHT: f32 = 1.0;
/// Header + separator + this = the 220 px empty state of the spec.
const EMPTY_BODY_HEIGHT: f32 = 175.0;
const ROW_HEIGHT: f32 = 52.0;
const ROW_RADIUS: u8 = 8;
const ROW_PADDING_X: f32 = 12.0;
const LIST_MARGIN_X: i8 = 8;
const LIST_MARGIN_Y: i8 = 4;
const LIST_MAX_HEIGHT: f32 = ROW_HEIGHT * 7.0;
const USAGE_COLUMN_WIDTH: f32 = 132.0;
const USAGE_BAR_SIZE: Vec2 = Vec2::new(56.0, 4.0);
const USAGE_LABEL_WIDTH: f32 = 18.0;
const USAGE_PCT_WIDTH: f32 = 30.0;
const USAGE_GAP: f32 = 6.0;
/// Bars of an expired or missing login still show the last known value, fainter.
const UNUSABLE_BAR_OPACITY: f32 = 0.4;
/// Space between the e-mail and the status pill.
const PILL_GAP: f32 = 8.0;
const HEADER_WORKSPACE_WIDTH: f32 = 160.0;
/// Quick-pick keys cover only the first nine profiles.
const QUICK_PICK_COUNT: usize = 9;

/// Pixels-per-point and available width a galley was laid out for.
type LayoutKey = (u32, u32);

/// Elided galley kept across frames; relaid out only when its [`LayoutKey`] changes.
struct CachedGalley {
    key: LayoutKey,
    galley: Arc<Galley>,
}

/// Open launcher overlay. Rows and header text are built when it opens (and
/// when `HubState::rows_version` moves), never per frame.
pub(super) struct ClaudeLauncherState {
    pub(super) selected: usize,
    rows: Vec<LauncherRow>,
    workspace_name: String,
    header_galley: Option<CachedGalley>,
    /// `HubState::rows_version` the rows were built from.
    rows_version: u64,
    /// Keyboard moved the selection: scroll it into view once.
    reveal_selected: bool,
}

/// Quick-pick key (`1`..`9`) to a profile index, `None` outside the list.
#[must_use]
pub(super) fn launcher_key_to_index(key: Key, len: usize) -> Option<usize> {
    let index = match key {
        Key::Num1 => 0,
        Key::Num2 => 1,
        Key::Num3 => 2,
        Key::Num4 => 3,
        Key::Num5 => 4,
        Key::Num6 => 5,
        Key::Num7 => 6,
        Key::Num8 => 7,
        Key::Num9 => 8,
        _ => return None,
    };
    (index < len).then_some(index)
}

/// Selection moved by `delta` rows, wrapping around both ends.
#[must_use]
pub(super) fn move_selection(selected: usize, delta: i32, len: usize) -> usize {
    let Ok(len) = i64::try_from(len) else {
        return selected;
    };
    if len == 0 {
        return 0;
    }
    let moved = (i64::try_from(selected).unwrap_or(0) + i64::from(delta)).rem_euclid(len);
    usize::try_from(moved).unwrap_or(0)
}

/// Row to preselect: the given profile when still listed, else the first.
#[must_use]
fn initial_selection(profiles: &[String], preferred: Option<&str>) -> usize {
    preferred
        .and_then(|name| profiles.iter().position(|profile| profile == name))
        .unwrap_or(0)
}

/// Derived profile presets are Claude presets carrying `CLAUDE_CONFIG_DIR`;
/// the built-in base preset (`Claude Code` / `cc`) is not a profile.
fn is_profile_preset(preset: &PresetConfig) -> bool {
    preset.kind == PanelKind::Claude && preset.env.contains_key("CLAUDE_CONFIG_DIR")
}

/// Derived preset of `profile_name` (`PanelKind::Claude`, alias == profile name,
/// carrying `CLAUDE_CONFIG_DIR`).
#[must_use]
fn find_profile_preset<'a>(presets: &'a [PresetConfig], profile_name: &str) -> Option<&'a PresetConfig> {
    presets
        .iter()
        .find(|preset| is_profile_preset(preset) && preset.alias.as_deref() == Some(profile_name))
}

/// Profile names the picker lists: only derived profile presets (they carry
/// `CLAUDE_CONFIG_DIR`), never the built-in base preset (`Claude Code` / `cc`).
/// Each is resolvable by [`find_profile_preset`], so a listed profile launches.
#[must_use]
fn claude_profile_names(presets: &[PresetConfig]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for alias in presets
        .iter()
        .filter(|preset| is_profile_preset(preset))
        .filter_map(|preset| preset.alias.as_deref())
    {
        if !names.iter().any(|name| name == alias) {
            names.push(alias.to_owned());
        }
    }
    names
}

/// What the user asked for this frame.
enum LauncherAction {
    None,
    Launch(usize),
    OpenSettings,
}

/// One `5h` / `7d` line of a row's usage column, formatted when the rows are built.
struct UsageLine {
    label: &'static str,
    /// Used fraction `0..=1`; `None` draws no bar.
    frac: Option<f32>,
    /// `42%`, or an em dash.
    text: String,
    severity: Severity,
}

/// Why a login cannot be used right now; drawn as a pill on the text column's second line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoginProblem {
    Expired,
    Missing,
}

impl LoginProblem {
    fn text(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Missing => "no login",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Expired => theme::PALETTE_YELLOW(),
            Self::Missing => theme::FG_DIM(),
        }
    }
}

/// Usage column data of one row, derived from the cached snapshot entry.
struct RowUsage {
    lines: [UsageLine; 2],
    problem: Option<LoginProblem>,
}

impl RowUsage {
    fn new(usage: Option<&ProfileUsage>) -> Self {
        let line = |label, five_hour: bool| {
            let window = usage.map(|usage| if five_hour { &usage.five_hour } else { &usage.seven_day });
            let percent = window.and_then(|window| window.percent);
            UsageLine {
                label,
                frac: percent.map(|percent| percent / 100.0),
                text: window_text(window),
                severity: severity_for(percent, usage.and_then(|usage| api_severity(usage, five_hour))),
            }
        };
        Self {
            lines: [line("5h", true), line("7d", false)],
            problem: usage.and_then(|usage| match usage.state {
                UsageState::Expired => Some(LoginProblem::Expired),
                UsageState::Missing => Some(LoginProblem::Missing),
                UsageState::Ok | UsageState::Stale => None,
            }),
        }
    }
}

/// Per-row data and layout caches; built on open, not per frame.
struct LauncherRow {
    name: String,
    accent: Color32,
    /// Account e-mail; `None` renders `—` (filled by the usage task).
    email: Option<String>,
    usage: RowUsage,
    name_galley: Option<CachedGalley>,
    email_galley: Option<CachedGalley>,
    /// Pill width for the current pixels-per-point; measured once, not per frame.
    pill_width: Option<(u32, f32)>,
}

impl HorizonApp {
    /// Account e-mail of `profile_name`, once the usage snapshot knows it.
    fn profile_account_email(&self, profile_name: &str) -> Option<&str> {
        self.hub.usage.account_email(profile_name)
    }

    fn active_workspace_label(&self) -> String {
        self.board
            .active_workspace
            .and_then(|id| self.board.workspace(id))
            .map_or_else(|| "new workspace".to_owned(), |workspace| workspace.name.clone())
    }

    fn build_launcher_rows(&mut self, names: Vec<String>) -> Vec<LauncherRow> {
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
                let email = self.profile_account_email(&name).map(str::to_owned);
                // The usage poller calls `HubState::invalidate_launcher_rows` when the snapshot changes.
                let usage = RowUsage::new(self.hub.usage.usage(&name));
                LauncherRow {
                    name,
                    accent,
                    email,
                    usage,
                    name_galley: None,
                    email_galley: None,
                    pill_width: None,
                }
            })
            .collect()
    }

    pub(in crate::app) fn toggle_claude_launcher(&mut self) {
        if self.hub.claude_launcher.is_some() {
            self.close_claude_launcher();
        } else {
            self.open_claude_launcher();
        }
    }

    pub(in crate::app) fn open_claude_launcher(&mut self) {
        // Listed profiles are the derived presets launch resolves from.
        let names = claude_profile_names(&self.presets);
        let selected = initial_selection(&names, self.hub.last_launched_profile.as_deref());
        let rows = self.build_launcher_rows(names);
        // One modal at a time: the palette, remote hosts and sessions close.
        self.command_palette = None;
        self.remote_hosts_overlay = None;
        self.session_manager = None;
        self.hub.claude_launcher = Some(ClaudeLauncherState {
            selected,
            rows,
            workspace_name: self.active_workspace_label(),
            header_galley: None,
            rows_version: self.hub.rows_version,
            reveal_selected: true,
        });
    }

    pub(in crate::app) fn close_claude_launcher(&mut self) {
        self.hub.claude_launcher = None;
    }

    /// Opens a Claude session for `profile_name` in the active workspace
    /// (creating one when the board is empty). The workspace without a cwd
    /// gets the directory picker through `add_panel_to_workspace`.
    pub(in crate::app) fn launch_claude_profile(&mut self, ctx: &Context, profile_name: &str) {
        let Some(preset) = find_profile_preset(&self.presets, profile_name).cloned() else {
            tracing::warn!(profile = profile_name, "no derived Claude preset for profile");
            return;
        };
        let workspace_id = self
            .board
            .active_workspace
            .unwrap_or_else(|| self.ensure_workspace_visible(ctx));
        self.add_panel_to_workspace(workspace_id, preset, None);
        self.hub.last_launched_profile = Some(profile_name.to_owned());
    }

    /// Opens Settings (when closed) on the Claude tab.
    fn open_settings_claude_tab(&mut self) {
        if self.settings.is_none() {
            self.toggle_settings();
        }
        if let Some(editor) = self.settings.as_mut() {
            super::settings::select_claude_tab(editor);
        }
    }

    /// Rebuilds rows when the profile list or usage data changed since they were built.
    fn refresh_launcher_rows(&mut self, state: &mut ClaudeLauncherState) {
        if state.rows_version == self.hub.rows_version {
            return;
        }
        let current = state.rows.get(state.selected).map(|row| row.name.clone());
        let names = claude_profile_names(&self.presets);
        state.selected = initial_selection(&names, current.as_deref());
        state.rows = self.build_launcher_rows(names);
        state.workspace_name = self.active_workspace_label();
        state.header_galley = None;
        state.rows_version = self.hub.rows_version;
        state.reveal_selected = true;
    }

    pub(in crate::app) fn render_claude_launcher(&mut self, ctx: &Context) {
        let Some(mut state) = self.hub.claude_launcher.take() else {
            return;
        };
        self.refresh_launcher_rows(&mut state);

        let mut action = handle_launcher_keys(ctx, &mut state);
        let outcome = hub_ui::show_modal(
            ctx,
            egui::Id::new("claude_launcher"),
            MODAL_WIDTH,
            ModalPlacement::TopFraction(0.18),
            None,
            |ui| {
                let from_ui = show_launcher_contents(ui, &mut state);
                if matches!(action, LauncherAction::None) {
                    action = from_ui;
                }
            },
        );

        match action {
            LauncherAction::Launch(index) => {
                if let Some(name) = state.rows.get(index).map(|row| row.name.clone()) {
                    self.launch_claude_profile(ctx, &name);
                }
            }
            LauncherAction::OpenSettings => self.open_settings_claude_tab(),
            LauncherAction::None if !outcome.dismissed => self.hub.claude_launcher = Some(state),
            LauncherAction::None => {}
        }
    }
}

/// Keyboard handling for one frame. Runs before the rows draw so a consumed
/// key never reaches a focused row or the terminals behind the modal.
fn handle_launcher_keys(ctx: &Context, state: &mut ClaudeLauncherState) -> LauncherAction {
    let len = state.rows.len();
    ctx.input_mut(|input| {
        if len == 0 {
            return if input.consume_key(Modifiers::NONE, Key::Enter) {
                LauncherAction::OpenSettings
            } else {
                LauncherAction::None
            };
        }
        for key in [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ] {
            if let Some(index) = launcher_key_to_index(key, len)
                && input.consume_key(Modifiers::NONE, key)
            {
                return LauncherAction::Launch(index);
            }
        }
        if input.consume_key(Modifiers::NONE, Key::Enter) {
            return LauncherAction::Launch(state.selected.min(len - 1));
        }
        let before = state.selected;
        if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
            state.selected = move_selection(state.selected, 1, len);
        }
        if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
            state.selected = move_selection(state.selected, -1, len);
        }
        if input.consume_key(Modifiers::NONE, Key::Home) {
            state.selected = 0;
        }
        if input.consume_key(Modifiers::NONE, Key::End) {
            state.selected = len - 1;
        }
        state.reveal_selected |= state.selected != before;
        LauncherAction::None
    })
}

fn show_launcher_contents(ui: &mut Ui, state: &mut ClaudeLauncherState) -> LauncherAction {
    // Every block is sized explicitly; the theme's item spacing must not add to it.
    ui.spacing_mut().item_spacing.y = 0.0;
    let width = ui.available_width();
    paint_header(ui, width, &state.workspace_name, &mut state.header_galley);
    paint_separator(ui, width);
    let action = if state.rows.is_empty() {
        show_empty_body(ui, width)
    } else {
        let action = show_rows(ui, state);
        paint_separator(ui, width);
        paint_footer(ui, width);
        action
    };
    state.selected = state.selected.min(state.rows.len().saturating_sub(1));
    action
}

fn layout_key(painter: &Painter, width: f32) -> LayoutKey {
    (painter.ctx().pixels_per_point().to_bits(), width.to_bits())
}

fn paint_header(ui: &mut Ui, width: f32, workspace_name: &str, name_galley: &mut Option<CachedGalley>) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEADER_HEIGHT), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, "Open Claude session"));
    let painter = ui.painter();
    painter.text(
        Pos2::new(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Open Claude session",
        FontId::proportional(13.0),
        theme::FG(),
    );
    let right = rect.right() - 16.0;
    painter.text(
        Pos2::new(right, rect.center().y - 7.0),
        Align2::RIGHT_CENTER,
        "workspace:",
        FontId::proportional(10.5),
        theme::FG_DIM(),
    );
    let key = layout_key(painter, HEADER_WORKSPACE_WIDTH);
    let name = cached_elided(
        name_galley,
        key,
        painter,
        workspace_name,
        FontId::proportional(10.5),
        HEADER_WORKSPACE_WIDTH,
    );
    painter.galley(
        Pos2::new(right - name.size().x, rect.center().y + 1.0),
        name,
        theme::FG_DIM(),
    );
}

fn paint_separator(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, SEPARATOR_HEIGHT), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SUBTLE());
}

fn paint_footer(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FOOTER_HEIGHT), Sense::hover());
    let builder = UiBuilder::new()
        .max_rect(rect.shrink2(Vec2::new(16.0, 0.0)))
        .layout(Layout::left_to_right(Align::Center));
    // `new_child`, not `scope_builder`: the latter would allocate again in `ui`.
    let mut footer = ui.new_child(builder);
    footer.spacing_mut().item_spacing.x = 14.0;
    hub_ui::kbd_hint(&mut footer, "\u{2191}\u{2193}", "move");
    hub_ui::kbd_hint(&mut footer, "Enter", "open");
    hub_ui::kbd_hint(&mut footer, "1\u{2013}9", "pick");
    hub_ui::kbd_hint(&mut footer, "Esc", "close");
}

fn show_empty_body(ui: &mut Ui, width: f32) -> LauncherAction {
    let mut action = LauncherAction::None;
    // Reserve the exact body height so the modal totals the spec's 220 px.
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, EMPTY_BODY_HEIGHT), Sense::hover());
    let builder = UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center));
    // `new_child`, not `scope_builder`: the latter would allocate again in `ui`.
    let mut body = ui.new_child(builder);
    let ui = &mut body;
    ui.add_space(24.0);
    let (icon, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
    let painter = ui.painter();
    painter.circle_stroke(icon.center(), 14.0, Stroke::new(1.5_f32, theme::BORDER_STRONG()));
    painter.circle_filled(icon.center(), 2.5, theme::BORDER_STRONG());
    ui.add_space(10.0);
    ui.label(egui::RichText::new("No Claude profiles").size(13.0).color(theme::FG()));
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("Add one in Settings \u{2192} Claude or run Detect accounts.")
            .size(11.0)
            .color(theme::FG_DIM()),
    );
    ui.add_space(14.0);
    let response = hub_ui::secondary_button(ui, "Open Settings \u{2192} Claude");
    // The only actionable control: always shown focused (Enter activates it).
    ui.painter().rect_stroke(
        response.rect,
        CornerRadius::same(ROW_RADIUS),
        Stroke::new(2.0_f32, theme::ACCENT()),
        StrokeKind::Outside,
    );
    if response.clicked() {
        action = LauncherAction::OpenSettings;
    }
    action
}

fn show_rows(ui: &mut Ui, state: &mut ClaudeLauncherState) -> LauncherAction {
    let mut action = LauncherAction::None;
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
    let ClaudeLauncherState {
        selected,
        rows,
        reveal_selected,
        ..
    } = state;
    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT + f32::from(LIST_MARGIN_Y) * 2.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(LIST_MARGIN_X, LIST_MARGIN_Y))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (index, row) in rows.iter_mut().enumerate() {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
                        response.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Button, true, format!("Open Claude session {}", row.name))
                        });
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            if pointer_moved {
                                *selected = index;
                            }
                        }
                        if *reveal_selected && *selected == index {
                            ui.scroll_to_rect(rect, None);
                            *reveal_selected = false;
                        }
                        if response.clicked() {
                            action = LauncherAction::Launch(index);
                        }
                        if ui.is_rect_visible(rect) {
                            let is_selected = *selected == index;
                            let pressed = response.is_pointer_button_down_on();
                            paint_row(ui.painter(), rect, index, row, is_selected, pressed);
                            hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
                        }
                    }
                });
        });
    action
}

fn paint_row(painter: &Painter, rect: Rect, index: usize, row: &mut LauncherRow, selected: bool, pressed: bool) {
    let accent = row.accent;
    if selected {
        let tint = if pressed { 0.30 } else { 0.22 };
        painter.rect(
            rect,
            CornerRadius::same(ROW_RADIUS),
            theme::blend(theme::PANEL_BG_ALT(), accent, tint),
            Stroke::new(1.0_f32, theme::alpha(accent, 150)),
            StrokeKind::Inside,
        );
        let bar = Rect::from_center_size(Pos2::new(rect.left() + 4.5, rect.center().y), Vec2::new(3.0, 28.0));
        painter.rect_filled(bar, CornerRadius::same(2), accent);
    }

    let left = rect.left() + ROW_PADDING_X;
    if index < QUICK_PICK_COUNT {
        let badge = Rect::from_min_size(Pos2::new(left, rect.center().y - 10.0), Vec2::splat(20.0));
        hub_ui::key_badge(painter, badge, index + 1, selected, accent);
    }
    let dot_x = left + 20.0 + 10.0 + 4.0;
    painter.circle_filled(Pos2::new(dot_x, rect.center().y), 4.0, accent);

    let usage_left = rect.right() - ROW_PADDING_X - USAGE_COLUMN_WIDTH;
    let text_left = dot_x + 4.0 + 8.0;
    let text_width = (usage_left - 8.0 - text_left).max(0.0);
    let key = layout_key(painter, text_width);
    let name = cached_elided(
        &mut row.name_galley,
        key,
        painter,
        &row.name,
        FontId::proportional(13.0),
        text_width,
    );
    painter.galley(Pos2::new(text_left, rect.top() + 10.0), name, theme::FG());

    let pill_width = row.usage.problem.map(|problem| {
        let ppp = painter.ctx().pixels_per_point().to_bits();
        match row.pill_width {
            Some((cached_ppp, width)) if cached_ppp == ppp => width,
            _ => {
                let width = hub_ui::pill_width(painter, problem.text());
                row.pill_width = Some((ppp, width));
                width
            }
        }
    });
    let email_width = (text_width - pill_width.map_or(0.0, |width| width + PILL_GAP)).max(0.0);
    let email = cached_elided(
        &mut row.email_galley,
        layout_key(painter, email_width),
        painter,
        row.email.as_deref().unwrap_or("\u{2014}"),
        FontId::proportional(11.0),
        email_width,
    );
    let email_right = text_left + email.size().x;
    painter.galley(Pos2::new(text_left, rect.top() + 29.0), email, theme::FG_DIM());
    if let (Some(problem), Some(width)) = (row.usage.problem, pill_width) {
        let pill = hub_ui::pill_rect(Pos2::new(email_right + PILL_GAP, rect.top() + 28.0), width);
        hub_ui::pill(painter, pill, problem.text(), problem.color());
    }

    paint_usage_column(painter, rect, &row.usage);
}

/// Right column: `5h` / `7d` lines, each a caption, a 56x4 bar and the percentage in the
/// severity color. Without data both read as an em dash and no bar is drawn; an expired or
/// missing login keeps its last known value on faded bars.
fn paint_usage_column(painter: &Painter, rect: Rect, usage: &RowUsage) {
    let group_width = USAGE_LABEL_WIDTH + USAGE_BAR_SIZE.x + USAGE_GAP + USAGE_PCT_WIDTH;
    let left = rect.right() - ROW_PADDING_X - group_width;
    let mut bar_painter = painter.clone();
    if usage.problem.is_some() {
        bar_painter.multiply_opacity(UNUSABLE_BAR_OPACITY);
    }
    for (line, offset) in usage.lines.iter().zip([-10.0_f32, 10.0]) {
        let y = rect.center().y + offset;
        painter.text(
            Pos2::new(left, y),
            Align2::LEFT_CENTER,
            line.label,
            FontId::proportional(8.5),
            theme::FG_DIM(),
        );
        let color = match (line.frac, line.severity) {
            (None, _) => theme::FG_DIM(),
            (Some(_), Severity::Normal) => theme::FG_SOFT(),
            (Some(frac), severity) => hub_ui::severity_color(frac * 100.0, Some(severity)),
        };
        if let Some(frac) = line.frac {
            let bar = Rect::from_min_size(
                Pos2::new(left + USAGE_LABEL_WIDTH, y - USAGE_BAR_SIZE.y / 2.0),
                USAGE_BAR_SIZE,
            );
            let fill = hub_ui::severity_color(frac * 100.0, Some(line.severity));
            hub_ui::mini_bar(&bar_painter, bar, frac, fill);
        }
        painter.text(
            Pos2::new(left + group_width, y),
            Align2::RIGHT_CENTER,
            &line.text,
            FontId::monospace(10.5),
            color,
        );
    }
}

/// [`hub_ui::elided`] reused from `slot` while `key` is unchanged.
fn cached_elided(
    slot: &mut Option<CachedGalley>,
    key: LayoutKey,
    painter: &Painter,
    text: &str,
    font: FontId,
    max_width: f32,
) -> Arc<Galley> {
    if let Some(cached) = slot.as_ref()
        && cached.key == key
    {
        return Arc::clone(&cached.galley);
    }
    let galley = hub_ui::elided(painter, text, font, max_width);
    *slot = Some(CachedGalley {
        key,
        galley: Arc::clone(&galley),
    });
    galley
}

#[cfg(test)]
mod tests {
    use horizon_core::PanelKind;

    use super::*;
    use crate::app::test_support::with_key_press;

    fn preset(name: &str, alias: Option<&str>, kind: PanelKind) -> PresetConfig {
        PresetConfig {
            name: name.to_owned(),
            alias: alias.map(str::to_owned),
            kind,
            command: None,
            args: Vec::new(),
            resume: horizon_core::PanelResume::Fresh,
            ssh_connection: None,
            env: std::collections::BTreeMap::new(),
        }
    }

    fn with_config_dir(mut preset: PresetConfig) -> PresetConfig {
        preset
            .env
            .insert("CLAUDE_CONFIG_DIR".to_owned(), "/home/u/.claude-x".to_owned());
        preset
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn number_keys_map_to_zero_based_indices_within_the_list() {
        assert_eq!(launcher_key_to_index(Key::Num1, 4), Some(0));
        assert_eq!(launcher_key_to_index(Key::Num2, 4), Some(1));
        assert_eq!(launcher_key_to_index(Key::Num4, 4), Some(3));
        assert_eq!(launcher_key_to_index(Key::Num9, 12), Some(8));
    }

    #[test]
    fn number_keys_past_the_list_or_non_digits_map_to_nothing() {
        assert_eq!(launcher_key_to_index(Key::Num5, 4), None);
        assert_eq!(launcher_key_to_index(Key::Num1, 0), None);
        assert_eq!(launcher_key_to_index(Key::Num0, 4), None);
        assert_eq!(launcher_key_to_index(Key::A, 4), None);
    }

    #[test]
    fn move_selection_wraps_in_both_directions() {
        assert_eq!(move_selection(0, 1, 3), 1);
        assert_eq!(move_selection(2, 1, 3), 0);
        assert_eq!(move_selection(0, -1, 3), 2);
        assert_eq!(move_selection(1, -1, 3), 0);
    }

    #[test]
    fn move_selection_handles_empty_and_single_lists() {
        assert_eq!(move_selection(0, 1, 0), 0);
        assert_eq!(move_selection(0, -1, 0), 0);
        assert_eq!(move_selection(0, 1, 1), 0);
        assert_eq!(move_selection(0, -1, 1), 0);
    }

    #[test]
    fn initial_selection_prefers_last_launched_profile() {
        let profiles = names(&["conta1", "conta2", "pessoal"]);
        assert_eq!(initial_selection(&profiles, Some("pessoal")), 2);
        assert_eq!(initial_selection(&profiles, Some("gone")), 0);
        assert_eq!(initial_selection(&profiles, None), 0);
        assert_eq!(initial_selection(&[], Some("pessoal")), 0);
    }

    #[test]
    fn find_profile_preset_matches_claude_presets_by_alias_only() {
        let presets = vec![
            preset("Shell", None, PanelKind::Shell),
            preset("Other", Some("conta2"), PanelKind::Codex),
            with_config_dir(preset("Claude \u{b7} conta2", Some("conta2"), PanelKind::Claude)),
        ];

        let found = find_profile_preset(&presets, "conta2").expect("derived preset");
        assert_eq!(found.name, "Claude \u{b7} conta2");
        assert!(find_profile_preset(&presets, "conta3").is_none());
    }

    #[test]
    fn find_profile_preset_ignores_the_base_preset_even_when_a_profile_shares_its_alias() {
        let presets = vec![preset("Claude Code", Some("cc"), PanelKind::Claude)];

        assert!(find_profile_preset(&presets, "cc").is_none());
    }

    #[test]
    fn profile_names_follow_the_claude_presets_launch_resolves() {
        let presets = vec![
            preset("Shell", None, PanelKind::Shell),
            preset("Claude Code", None, PanelKind::Claude),
            preset("Other", Some("conta2"), PanelKind::Codex),
            with_config_dir(preset("Claude \u{b7} conta1", Some("conta1"), PanelKind::Claude)),
            with_config_dir(preset("Claude \u{b7} conta2", Some("conta2"), PanelKind::Claude)),
        ];

        let names = claude_profile_names(&presets);

        assert_eq!(names, vec!["conta1".to_owned(), "conta2".to_owned()]);
        for name in &names {
            assert!(find_profile_preset(&presets, name).is_some(), "{name} must launch");
        }
    }

    #[test]
    fn default_config_has_no_profiles_so_the_launcher_is_empty() {
        // Raw presets: `resolved_presets()` would also auto-discover `~/.claude-*`
        // dirs of the machine running the tests.
        let presets = horizon_core::Config::default().presets;

        assert!(
            presets
                .iter()
                .any(|p| p.kind == PanelKind::Claude && p.alias.as_deref() == Some("cc")),
            "built-in base preset must exist for this test to be meaningful"
        );
        assert!(claude_profile_names(&presets).is_empty());
    }

    #[test]
    fn base_cc_alias_is_skipped_but_derived_profiles_are_listed() {
        let config = horizon_core::Config {
            claude_profiles: vec![horizon_core::ClaudeProfile {
                name: "conta2".to_owned(),
                config_dir: "/home/u/.claude-conta2".to_owned(),
            }],
            ..horizon_core::Config::default()
        };

        let presets = config.resolved_presets();
        let names = claude_profile_names(&presets);

        assert_eq!(names, vec!["conta2".to_owned()]);
        assert!(find_profile_preset(&presets, "conta2").is_some());
    }

    fn usage_with(state: UsageState, five: Option<f32>, seven: Option<f32>, severity: Option<&str>) -> ProfileUsage {
        use horizon_core::claude_usage::UsageWindow;
        let window = |percent| UsageWindow {
            percent,
            resets_at: None,
        };
        ProfileUsage {
            name: "conta1".to_owned(),
            display_name: None,
            email: None,
            five_hour: window(five),
            seven_day: window(seven),
            severity: severity.map(str::to_owned),
            state,
            fetched_at_ms: Some(1_000),
        }
    }

    #[test]
    fn row_usage_without_a_snapshot_reads_dashes_and_draws_no_bars() {
        let usage = RowUsage::new(None);

        assert_eq!(usage.lines.each_ref().map(|line| line.label), ["5h", "7d"]);
        assert!(
            usage
                .lines
                .iter()
                .all(|line| line.frac.is_none() && line.text == "\u{2014}")
        );
        assert_eq!(usage.problem, None);
    }

    #[test]
    fn row_usage_formats_percent_fraction_and_severity_per_window() {
        let snapshot = usage_with(UsageState::Ok, Some(42.0), Some(91.0), None);

        let usage = RowUsage::new(Some(&snapshot));

        assert_eq!(usage.lines[0].text, "42%");
        assert_eq!(usage.lines[0].frac, Some(0.42));
        assert_eq!(usage.lines[0].severity, Severity::Normal);
        assert_eq!(usage.lines[1].text, "91%");
        assert_eq!(usage.lines[1].severity, Severity::Critical);
        assert_eq!(usage.problem, None);
    }

    #[test]
    fn row_usage_flags_expired_and_missing_logins_but_keeps_the_last_values() {
        let expired = usage_with(UsageState::Expired, Some(10.0), Some(20.0), None);
        let missing = usage_with(UsageState::Missing, None, None, None);

        let expired = RowUsage::new(Some(&expired));
        let missing = RowUsage::new(Some(&missing));

        assert_eq!(expired.problem, Some(LoginProblem::Expired));
        assert_eq!(expired.lines[0].frac, Some(0.1));
        assert_eq!(missing.problem, Some(LoginProblem::Missing));
        assert_eq!(LoginProblem::Expired.text(), "expired");
        assert_eq!(LoginProblem::Missing.text(), "no login");
    }

    #[test]
    fn row_usage_applies_the_api_verdict_to_the_window_closest_to_its_limit_only() {
        let snapshot = usage_with(UsageState::Ok, Some(10.0), Some(60.0), Some("critical"));

        let usage = RowUsage::new(Some(&snapshot));

        assert_eq!(usage.lines[0].severity, Severity::Normal);
        assert_eq!(usage.lines[1].severity, Severity::Critical);
    }

    fn row(name: &str) -> LauncherRow {
        LauncherRow {
            name: name.to_owned(),
            accent: Color32::WHITE,
            email: None,
            usage: RowUsage::new(None),
            name_galley: None,
            email_galley: None,
            pill_width: None,
        }
    }

    fn state_with(rows: Vec<LauncherRow>) -> ClaudeLauncherState {
        ClaudeLauncherState {
            selected: 0,
            rows,
            workspace_name: "ws".to_owned(),
            header_galley: None,
            rows_version: 0,
            reveal_selected: false,
        }
    }

    /// Height `show_launcher_contents` claims in a parent that has the default
    /// (8, 8) item spacing, like the real modal.
    fn contents_height(rows: Vec<LauncherRow>) -> f32 {
        let ctx = Context::default();
        let mut height = 0.0;
        let mut state = state_with(rows);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(MODAL_WIDTH);
                    let top = ui.cursor().top();
                    let _ = show_launcher_contents(ui, &mut state);
                    height = ui.cursor().top() - top;
                });
            });
        }
        height
    }

    #[test]
    fn empty_state_is_220_px_tall_regardless_of_theme_spacing() {
        assert!((contents_height(Vec::new()) - 220.0).abs() < 0.01);
    }

    #[test]
    fn list_state_adds_no_item_spacing_between_blocks() {
        let expected = HEADER_HEIGHT
            + SEPARATOR_HEIGHT
            + ROW_HEIGHT
            + f32::from(LIST_MARGIN_Y) * 2.0
            + SEPARATOR_HEIGHT
            + FOOTER_HEIGHT;
        assert!((contents_height(vec![row("conta1")]) - expected).abs() < 0.01);
    }

    fn launcher_with(names: &[&str], selected: usize) -> ClaudeLauncherState {
        let mut state = state_with(names.iter().map(|name| row(name)).collect());
        state.selected = selected;
        state
    }

    fn launcher_keys(key: Key, state: &mut ClaudeLauncherState) -> LauncherAction {
        with_key_press(key, Modifiers::NONE, |ctx| handle_launcher_keys(ctx, state))
    }

    #[test]
    fn arrows_move_the_selection_and_wrap_around_the_list() {
        let mut state = launcher_with(&["a", "b", "c"], 2);

        assert!(matches!(
            launcher_keys(Key::ArrowDown, &mut state),
            LauncherAction::None
        ));
        assert_eq!(state.selected, 0, "Down from the last row wraps to the first");
        assert!(state.reveal_selected);

        let mut state = launcher_with(&["a", "b", "c"], 0);
        launcher_keys(Key::ArrowUp, &mut state);
        assert_eq!(state.selected, 2, "Up from the first row wraps to the last");

        let mut state = launcher_with(&["a", "b", "c"], 1);
        launcher_keys(Key::ArrowDown, &mut state);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let mut state = launcher_with(&["a", "b", "c"], 1);
        launcher_keys(Key::End, &mut state);
        assert_eq!(state.selected, 2);
        launcher_keys(Key::Home, &mut state);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn enter_launches_the_selected_profile() {
        let mut state = launcher_with(&["a", "b", "c"], 1);

        assert!(matches!(
            launcher_keys(Key::Enter, &mut state),
            LauncherAction::Launch(1)
        ));
    }

    #[test]
    fn number_keys_launch_directly_and_ignore_missing_rows() {
        let mut state = launcher_with(&["a", "b", "c"], 0);
        assert!(matches!(
            launcher_keys(Key::Num3, &mut state),
            LauncherAction::Launch(2)
        ));

        let mut state = launcher_with(&["a", "b", "c"], 0);
        assert!(matches!(launcher_keys(Key::Num4, &mut state), LauncherAction::None));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn enter_on_an_empty_launcher_opens_settings_and_other_keys_do_nothing() {
        let mut empty = launcher_with(&[], 0);
        assert!(matches!(
            launcher_keys(Key::Enter, &mut empty),
            LauncherAction::OpenSettings
        ));

        let mut empty = launcher_with(&[], 0);
        assert!(matches!(
            launcher_keys(Key::ArrowDown, &mut empty),
            LauncherAction::None
        ));
        assert!(matches!(launcher_keys(Key::Num1, &mut empty), LauncherAction::None));
    }
}
