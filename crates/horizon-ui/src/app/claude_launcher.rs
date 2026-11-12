//! Claude launcher: `Ctrl+Shift+L` opens a keyboard-first picker of the
//! configured Claude profiles; the choice opens a session in the active
//! workspace through the regular `add_panel_to_workspace` flow.
//! Visual spec: `docs/design/claude-hub-ui.md` §2.

use egui::{
    Align, Align2, Color32, Context, CornerRadius, FontId, Key, Layout, Margin, Modifiers, Painter, Pos2, Rect, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::{PanelKind, PresetConfig};

use super::HorizonApp;
use super::hub_ui::{self, ModalPlacement};
use crate::theme;

const MODAL_WIDTH: f32 = 480.0;
const HEADER_HEIGHT: f32 = 44.0;
const FOOTER_HEIGHT: f32 = 36.0;
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
/// Quick-pick keys cover only the first nine profiles.
const QUICK_PICK_COUNT: usize = 9;

/// Open launcher overlay. `profiles` is the name list captured when it opened.
pub(super) struct ClaudeLauncherState {
    pub(super) selected: usize,
    profiles: Vec<String>,
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

/// Row to preselect: the last launched profile when still listed, else the first.
#[must_use]
fn initial_selection(profiles: &[String], last_launched: Option<&str>) -> usize {
    last_launched
        .and_then(|last| profiles.iter().position(|name| name == last))
        .unwrap_or(0)
}

/// Derived preset of `profile_name` (`PanelKind::Claude`, alias == profile name).
#[must_use]
fn find_profile_preset<'a>(presets: &'a [PresetConfig], profile_name: &str) -> Option<&'a PresetConfig> {
    presets
        .iter()
        .find(|preset| preset.kind == PanelKind::Claude && preset.alias.as_deref() == Some(profile_name))
}

/// What the user asked for this frame.
enum LauncherAction {
    None,
    Launch(usize),
    OpenSettings,
}

/// Per-row data the contents function paints; built once per frame.
struct LauncherRow {
    name: String,
    accent: Color32,
    /// Account e-mail; `None` renders `—` (filled by the usage task).
    email: Option<String>,
    /// Pre-formatted usage summary; `None` renders `5h —` / `7d —` without bars.
    usage: Option<String>,
}

impl HorizonApp {
    /// Account e-mail of `profile_name`, once the usage snapshot knows it.
    #[expect(clippy::unused_self, reason = "seam: the usage snapshot is read here")]
    fn profile_account_email(&self, _profile_name: &str) -> Option<String> {
        None
    }

    /// Usage summary of `profile_name` from the usage snapshot, when present.
    #[expect(clippy::unused_self, reason = "seam: the usage snapshot is read here")]
    fn profile_usage_label(&self, _profile_name: &str) -> Option<String> {
        None
    }

    pub(in crate::app) fn toggle_claude_launcher(&mut self) {
        if self.hub.claude_launcher.is_some() {
            self.close_claude_launcher();
        } else {
            self.open_claude_launcher();
        }
    }

    pub(in crate::app) fn open_claude_launcher(&mut self) {
        let profiles: Vec<String> = self
            .template_config
            .effective_claude_profiles()
            .into_iter()
            .map(|profile| profile.name)
            .collect();
        let selected = initial_selection(&profiles, self.hub.last_launched_profile.as_deref());
        // One modal at a time: the palette and the remote hosts overlay close.
        self.command_palette = None;
        self.remote_hosts_overlay = None;
        self.hub.claude_launcher = Some(ClaudeLauncherState {
            selected,
            profiles,
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

    /// Seam for the Claude settings tab (Task 6): until it can open on its own
    /// tab this opens Settings.
    fn open_settings_claude_tab(&mut self) {
        if self.settings.is_none() {
            self.toggle_settings();
        }
    }

    pub(in crate::app) fn render_claude_launcher(&mut self, ctx: &Context) {
        let Some(mut state) = self.hub.claude_launcher.take() else {
            return;
        };
        let accents: Vec<Color32> = self
            .hub
            .accent_slots(&state.profiles)
            .iter()
            .map(|slot| hub_ui::accent_color(*slot))
            .collect();
        let rows: Vec<LauncherRow> = state
            .profiles
            .iter()
            .zip(accents)
            .map(|(name, accent)| LauncherRow {
                name: name.clone(),
                accent,
                email: self.profile_account_email(name),
                usage: self.profile_usage_label(name),
            })
            .collect();
        let workspace_name = self
            .board
            .active_workspace
            .and_then(|id| self.board.workspace(id))
            .map_or_else(|| "new workspace".to_owned(), |workspace| workspace.name.clone());

        let mut action = handle_launcher_keys(ctx, &mut state, rows.len());
        let outcome = hub_ui::show_modal(
            ctx,
            egui::Id::new("claude_launcher"),
            MODAL_WIDTH,
            ModalPlacement::TopFraction(0.18),
            None,
            |ui| {
                let from_ui = show_launcher_contents(ui, &mut state, &rows, &workspace_name);
                if matches!(action, LauncherAction::None) {
                    action = from_ui;
                }
            },
        );

        match action {
            LauncherAction::Launch(index) => {
                if let Some(row) = rows.get(index) {
                    self.launch_claude_profile(ctx, &row.name);
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
fn handle_launcher_keys(ctx: &Context, state: &mut ClaudeLauncherState, len: usize) -> LauncherAction {
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

fn show_launcher_contents(
    ui: &mut Ui,
    state: &mut ClaudeLauncherState,
    rows: &[LauncherRow],
    workspace_name: &str,
) -> LauncherAction {
    let width = ui.available_width();
    paint_header(ui, width, workspace_name);
    paint_separator(ui, width);
    let action = if rows.is_empty() {
        show_empty_body(ui, width)
    } else {
        let action = show_rows(ui, state, rows);
        paint_separator(ui, width);
        paint_footer(ui, width);
        action
    };
    state.selected = state.selected.min(rows.len().saturating_sub(1));
    action
}

fn paint_header(ui: &mut Ui, width: f32, workspace_name: &str) {
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
    let name = elided(
        painter,
        workspace_name,
        FontId::proportional(10.5),
        theme::FG_DIM(),
        160.0,
    );
    painter.galley(
        Pos2::new(right - name.size().x, rect.center().y + 1.0),
        name,
        theme::FG_DIM(),
    );
}

fn paint_separator(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER_SUBTLE());
}

fn paint_footer(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, FOOTER_HEIGHT), Sense::hover());
    let builder = UiBuilder::new()
        .max_rect(rect.shrink2(Vec2::new(16.0, 0.0)))
        .layout(Layout::left_to_right(Align::Center));
    ui.scope_builder(builder, |ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        hub_ui::kbd_hint(ui, "\u{2191}\u{2193}", "move");
        hub_ui::kbd_hint(ui, "Enter", "open");
        hub_ui::kbd_hint(ui, "1\u{2013}9", "pick");
        hub_ui::kbd_hint(ui, "Esc", "close");
    });
}

fn show_empty_body(ui: &mut Ui, width: f32) -> LauncherAction {
    let mut action = LauncherAction::None;
    ui.allocate_ui_with_layout(
        Vec2::new(width, EMPTY_BODY_HEIGHT),
        Layout::top_down(Align::Center),
        |ui| {
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
        },
    );
    action
}

fn show_rows(ui: &mut Ui, state: &mut ClaudeLauncherState, rows: &[LauncherRow]) -> LauncherAction {
    let mut action = LauncherAction::None;
    let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
    egui::ScrollArea::vertical()
        .max_height(LIST_MAX_HEIGHT + f32::from(LIST_MARGIN_Y) * 2.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(Margin::symmetric(LIST_MARGIN_X, LIST_MARGIN_Y))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (index, row) in rows.iter().enumerate() {
                        let (rect, response) =
                            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
                        response.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Button, true, format!("Open Claude session {}", row.name))
                        });
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            if pointer_moved {
                                state.selected = index;
                            }
                        }
                        if state.reveal_selected && state.selected == index {
                            ui.scroll_to_rect(rect, None);
                            state.reveal_selected = false;
                        }
                        if response.clicked() {
                            action = LauncherAction::Launch(index);
                        }
                        if ui.is_rect_visible(rect) {
                            let selected = state.selected == index;
                            let pressed = response.is_pointer_button_down_on();
                            paint_row(ui.painter(), rect, index, row, selected, pressed);
                            hub_ui::paint_focus_ring(ui.painter(), &response, rect, ROW_RADIUS);
                        }
                    }
                });
        });
    action
}

fn paint_row(painter: &Painter, rect: Rect, index: usize, row: &LauncherRow, selected: bool, pressed: bool) {
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
    let name = elided(painter, &row.name, FontId::proportional(13.0), theme::FG(), text_width);
    painter.galley(Pos2::new(text_left, rect.top() + 10.0), name, theme::FG());
    let (email_text, email_color) = row
        .email
        .as_deref()
        .map_or(("\u{2014}", theme::FG_DIM()), |email| (email, theme::FG_DIM()));
    let email = elided(painter, email_text, FontId::proportional(11.0), email_color, text_width);
    painter.galley(Pos2::new(text_left, rect.top() + 29.0), email, email_color);

    paint_usage_column(painter, rect, usage_left, row.usage.as_deref());
}

/// Right column: `5h` / `7d` lines. Without a usage snapshot both read `—`
/// and no bars are drawn.
fn paint_usage_column(painter: &Painter, rect: Rect, left: f32, usage: Option<&str>) {
    let right = rect.right() - ROW_PADDING_X;
    if let Some(summary) = usage {
        painter.text(
            Pos2::new(right, rect.center().y),
            Align2::RIGHT_CENTER,
            summary,
            FontId::monospace(10.5),
            theme::FG_SOFT(),
        );
        return;
    }
    for (offset, label) in [(-10.0_f32, "5h"), (10.0, "7d")] {
        let y = rect.center().y + offset;
        painter.text(
            Pos2::new(left, y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(8.5),
            theme::FG_DIM(),
        );
        painter.text(
            Pos2::new(left + USAGE_LABEL_WIDTH + USAGE_BAR_SIZE.x, y),
            Align2::RIGHT_CENTER,
            "\u{2014}",
            FontId::monospace(10.5),
            theme::FG_DIM(),
        );
    }
}

/// One-line galley truncated with an ellipsis at `max_width`.
fn elided(painter: &Painter, text: &str, font: FontId, color: Color32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(1.0));
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use horizon_core::PanelKind;

    use super::*;

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
            preset("Claude \u{b7} conta2", Some("conta2"), PanelKind::Claude),
        ];

        let found = find_profile_preset(&presets, "conta2").expect("derived preset");
        assert_eq!(found.name, "Claude \u{b7} conta2");
        assert!(find_profile_preset(&presets, "conta3").is_none());
    }
}
