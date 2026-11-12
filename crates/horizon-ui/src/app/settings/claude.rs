//! Settings "Claude" tab: edit `claude_profiles` (spec: `docs/design/claude-hub-ui.md` §3).
//!
//! Credentials are only ever inspected for their state (`ok` / `expired` /
//! `missing`); the token itself is never read into a displayed or stored value.
//! The list logic and the cached credential facts live in [`profiles`]; the
//! directory browser modal in [`browse`].

mod browse;
mod profiles;

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use egui::{
    Align, Color32, Context, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextEdit, Ui, Vec2,
};
use horizon_core::{ClaudeProfile, SavedPrompt, discover_claude_profiles};

use self::browse::{BrowseState, render_browse};
use self::profiles::{
    CredentialState, InfoCache, IssueField, ProfileInfo, RowIssue, add_profile, merge_detected, move_profile,
    remove_profile, row_issue,
};
use super::{SettingsEditor, SettingsTab, dim_label, prompts_section, section_card};
use crate::app::hub_ui::{self, IconKind};
use crate::theme;

const ACCENT_STRIPE_WIDTH: f32 = 3.0;
/// Horizontal space the card itself takes: left/right padding plus stroke.
const CARD_CHROME_WIDTH: f32 = 15.0 + 12.0 + 2.0;
const ITEM_SPACING_X: f32 = 8.0;
const DOT_SIZE: f32 = 8.0;
const ICON_BUTTON_WIDTH: f32 = 26.0;
/// The name field takes this share of the free row width, within the bounds below.
const NAME_SHARE: f32 = 0.28;
const NAME_MIN_WIDTH: f32 = 88.0;
const NAME_MAX_WIDTH: f32 = 140.0;
/// The directory field must keep at least this share of the row, else the card wraps.
const DIR_MIN_SHARE: f32 = 0.4;

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

// -- Row geometry --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct RowLayout {
    /// Wrap into two rows (name above directory).
    compact: bool,
    /// Width of the name field in the single-row layout.
    name_width: f32,
}

/// Chooses between the single-row and the wrapped card layout for a row that
/// has `available` px, given the width of the Browse button and the (reserved)
/// credential pill. The single row is used while the directory field keeps at
/// least [`DIR_MIN_SHARE`] of the row.
fn row_layout(available: f32, browse_width: f32, pill_width: f32) -> RowLayout {
    let fixed =
        CARD_CHROME_WIDTH + DOT_SIZE + browse_width + pill_width + 3.0 * ICON_BUTTON_WIDTH + 7.0 * ITEM_SPACING_X;
    let free = available - fixed;
    let name_width = (free * NAME_SHARE).clamp(NAME_MIN_WIDTH, NAME_MAX_WIDTH);
    RowLayout {
        compact: free - name_width < DIR_MIN_SHARE * available,
        name_width,
    }
}

// -- Tab state ----------------------------------------------------------------

#[derive(Default)]
pub(super) struct ClaudeTabState {
    /// Credential facts, read in the background and re-read every 30 s; marked
    /// stale when the tab is (re)opened and on "Detect accounts".
    infos: InfoCache,
    last_pass: Option<u64>,
    accents: (Vec<String>, Vec<usize>),
    detect_rx: Option<mpsc::Receiver<Vec<ClaudeProfile>>>,
    detect_result: Option<String>,
    /// Profiles from this index on were just detected and get a highlight.
    highlight_from: Option<usize>,
    /// Row added by "+ Add profile": focus its name field and scroll to it.
    focus_new: Option<usize>,
    browse: Option<BrowseState>,
}

impl ClaudeTabState {
    fn accent_slots(&mut self, profiles: &[ClaudeProfile]) -> &[usize] {
        let unchanged = self.accents.0.len() == profiles.len()
            && self.accents.0.iter().zip(profiles).all(|(cached, p)| *cached == p.name);
        if !unchanged {
            let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
            self.accents = (
                profiles.iter().map(|p| p.name.clone()).collect(),
                hub_ui::assign_accent_indices(&names),
            );
        }
        &self.accents.1
    }
}

/// Switches the settings editor to the Claude tab (used by the launcher's
/// "Manage profiles" entry).
pub(crate) fn select_claude_tab(editor: &mut SettingsEditor) {
    super::switch_tab(editor, SettingsTab::Claude);
}

pub(super) fn show(ui: &mut Ui, editor: &mut SettingsEditor) {
    let SettingsEditor {
        editing_config,
        claude_tab: tab,
        buffer,
        ..
    } = editor;
    let Some(config) = editing_config else {
        ui.label(
            RichText::new("Unable to parse current configuration")
                .color(theme::PALETTE_RED())
                .size(12.0),
        );
        return;
    };

    let pass = ui.ctx().cumulative_pass_nr();
    if !matches!(tab.last_pass, Some(last) if last + 1 >= pass) {
        tab.infos.invalidate();
    }
    tab.last_pass = Some(pass);

    let home = home_dir();
    let dirs: Vec<&str> = config.claude_profiles.iter().map(|p| p.config_dir.as_str()).collect();
    tab.infos.refresh(ui.ctx(), &dirs, &home);
    ui.ctx().request_repaint_after(profiles::INFO_TTL);

    let mut changed = poll_detect(tab, &mut config.claude_profiles, &home);
    changed |= render(ui, &mut config.claude_profiles, tab, &home);
    changed |= render_saved_prompts(ui, &mut config.prompts);
    if changed && let Ok(yaml) = config.to_yaml() {
        *buffer = yaml;
    }
}

fn poll_detect(tab: &mut ClaudeTabState, profiles: &mut Vec<ClaudeProfile>, home: &Path) -> bool {
    let Some(rx) = &tab.detect_rx else {
        return false;
    };
    let found = match rx.try_recv() {
        Ok(found) => found,
        Err(mpsc::TryRecvError::Empty) => return false,
        Err(mpsc::TryRecvError::Disconnected) => Vec::new(),
    };
    tab.detect_rx = None;
    tab.infos.invalidate();
    let added = merge_detected(profiles, found, home);
    tab.detect_result = Some(match added {
        0 => "No new accounts found".to_string(),
        1 => "Found 1 new profile".to_string(),
        n => format!("Found {n} new profiles"),
    });
    if added > 0 {
        tab.highlight_from = Some(profiles.len() - added);
    }
    added > 0
}

fn start_detect(tab: &mut ClaudeTabState, ctx: &Context, home: &Path) {
    let (tx, rx) = mpsc::channel();
    let home = home.to_path_buf();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(discover_claude_profiles(&home));
        ctx.request_repaint();
    });
    tab.detect_rx = Some(rx);
    tab.detect_result = None;
}

// -- Rendering ------------------------------------------------------------------

#[derive(Clone, Copy)]
enum RowAction {
    Up,
    Down,
    Remove,
    Browse,
}

fn render(ui: &mut Ui, profiles: &mut Vec<ClaudeProfile>, tab: &mut ClaudeTabState, home: &Path) -> bool {
    let mut changed = false;

    if tab.highlight_from.is_some()
        && ui.input(|i| {
            i.pointer.any_pressed()
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)))
        })
    {
        tab.highlight_from = None;
    }

    let detecting = tab.detect_rx.is_some();
    let mut want_detect = false;
    let mut want_add = false;

    // Title and actions stack instead of sharing a row: at the narrowest
    // settings panel the buttons would otherwise overlap the title.
    ui.label(
        RichText::new("Claude profiles")
            .color(theme::FG_SOFT())
            .size(13.0)
            .strong(),
    );
    if !profiles.is_empty() {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            want_detect = detect_button(ui, detecting);
            want_add = hub_ui::secondary_button(ui, "+ Add profile").clicked();
        });
    }
    ui.add_space(4.0);
    dim_label(
        ui,
        "Each profile is a CLAUDE_CONFIG_DIR. Credentials are never shown or modified.",
    );
    if let Some(result) = &tab.detect_result {
        ui.add_space(4.0);
        ui.label(RichText::new(result).color(theme::FG_DIM()).size(11.0));
    }
    ui.add_space(10.0);

    if profiles.is_empty() {
        let (detect, add) = render_empty_state(ui, detecting);
        want_detect |= detect;
        want_add |= add;
    } else {
        let (row_changed, action) = render_rows(ui, profiles, tab);
        changed |= row_changed;
        if let Some((index, action)) = action {
            changed |= apply_action(profiles, tab, index, action);
        }
    }

    if want_add {
        tab.focus_new = Some(add_profile(profiles));
        changed = true;
        ui.ctx().request_repaint();
    }
    if want_detect && !detecting {
        start_detect(tab, ui.ctx(), home);
    }
    changed |= render_browse(ui.ctx(), profiles, &mut tab.browse);
    changed
}

/// "Saved prompts" section at the end of the tab: prompts available in every workspace.
fn render_saved_prompts(ui: &mut Ui, prompts: &mut Vec<SavedPrompt>) -> bool {
    ui.add_space(16.0);
    ui.separator();
    ui.add_space(10.0);
    ui.label(
        RichText::new("Saved prompts")
            .color(theme::FG_SOFT())
            .size(13.0)
            .strong(),
    );
    ui.add_space(4.0);
    dim_label(
        ui,
        "Insert one into the focused terminal from the command palette (Prompt: name). Workspace prompts with the same name win.",
    );
    ui.add_space(10.0);
    prompts_section::show(ui, "global", prompts)
}

fn detect_button(ui: &mut Ui, detecting: bool) -> bool {
    if detecting {
        ui.add_enabled_ui(false, |ui| hub_ui::secondary_button(ui, "Detecting\u{2026}"));
        false
    } else {
        hub_ui::secondary_button(ui, "Detect accounts").clicked()
    }
}

fn render_empty_state(ui: &mut Ui, detecting: bool) -> (bool, bool) {
    let (mut detect, mut add) = (false, false);
    section_card(ui, |ui| {
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("No profiles configured. Detect accounts finds ~/.claude and ~/.claude-*.")
                    .color(theme::FG_DIM())
                    .size(12.0),
            );
        });
        ui.add_space(12.0);
        let detect_label = if detecting {
            "Detecting\u{2026}"
        } else {
            "Detect accounts"
        };
        let total = button_width(ui, detect_label) + button_width(ui, "+ Add profile") + ui.spacing().item_spacing.x;
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            detect = hub_ui::primary_button(ui, detect_label, !detecting).clicked();
            add = hub_ui::secondary_button(ui, "+ Add profile").clicked();
        });
        ui.add_space(8.0);
    });
    (detect, add)
}

fn button_width(ui: &Ui, label: &str) -> f32 {
    let text = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(12.0), Color32::WHITE)
        .size()
        .x;
    (text + 32.0).max(64.0)
}

/// Pill width reserved in every row (the widest label), so the fields to its
/// left line up whatever the credential state.
fn reserved_pill_width(ui: &Ui) -> f32 {
    CredentialState::ALL
        .iter()
        .map(|state| state.label())
        .chain([CHECKING_LABEL])
        .map(|label| hub_ui::pill_width(ui.painter(), label))
        .fold(0.0, f32::max)
}

const CHECKING_LABEL: &str = "checking";

struct CardContext<'a> {
    index: usize,
    count: usize,
    accent: Color32,
    /// `None` while the background read for this directory has not landed.
    info: Option<&'a ProfileInfo>,
    issue: Option<&'a RowIssue>,
    highlighted: bool,
    focus_name: bool,
    layout: RowLayout,
    pill_width: f32,
}

fn render_rows(
    ui: &mut Ui,
    profiles: &mut [ClaudeProfile],
    tab: &mut ClaudeTabState,
) -> (bool, Option<(usize, RowAction)>) {
    let mut changed = false;
    let mut action = None;
    let slots = tab.accent_slots(profiles).to_vec();
    let pill_width = reserved_pill_width(ui);
    let layout = row_layout(ui.available_width(), button_width(ui, "Browse"), pill_width);
    let highlight_from = tab.highlight_from;
    let focus_new = tab.focus_new.take();
    for index in 0..profiles.len() {
        let info = tab.infos.get(&profiles[index].config_dir).cloned();
        // Until the first read lands, do not warn about a directory we have not checked.
        let issue = row_issue(profiles, index, info.as_ref().is_none_or(|info| info.dir_exists));
        let context = CardContext {
            index,
            count: profiles.len(),
            accent: hub_ui::accent_color(slots.get(index).copied().unwrap_or(index)),
            info: info.as_ref(),
            issue: issue.as_ref(),
            highlighted: highlight_from.is_some_and(|from| index >= from),
            focus_name: focus_new == Some(index),
            layout,
            pill_width,
        };
        let (row_changed, row_action) = profile_card(ui, &mut profiles[index], &context);
        changed |= row_changed;
        if let Some(row_action) = row_action {
            action = Some((index, row_action));
        }
        if let Some(issue) = &issue {
            let color = if issue.blocking {
                theme::PALETTE_RED()
            } else {
                theme::PALETTE_YELLOW()
            };
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(RichText::new(&issue.message).color(color).size(11.0));
            });
        }
        ui.add_space(8.0);
    }
    (changed, action)
}

fn apply_action(profiles: &mut Vec<ClaudeProfile>, tab: &mut ClaudeTabState, index: usize, action: RowAction) -> bool {
    match action {
        RowAction::Up => {
            move_profile(profiles, index, index.saturating_sub(1));
            true
        }
        RowAction::Down => {
            move_profile(profiles, index, index + 1);
            true
        }
        RowAction::Remove => {
            tab.highlight_from = None;
            remove_profile(profiles, index)
        }
        RowAction::Browse => {
            let seed = profiles.get(index).map(|p| p.config_dir.clone()).unwrap_or_default();
            tab.browse = Some(BrowseState::new(index, &seed));
            false
        }
    }
}

fn profile_card(ui: &mut Ui, profile: &mut ClaudeProfile, card: &CardContext<'_>) -> (bool, Option<RowAction>) {
    let mut changed = false;
    let mut action = None;
    let mut fill = theme::blend(theme::PANEL_BG_ALT(), theme::PANEL_BG(), 0.5);
    if card.highlighted {
        fill = theme::blend(fill, theme::ACCENT(), 28.0 / 255.0);
    }
    let response = Frame::default()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, theme::BORDER_SUBTLE()))
        .corner_radius(10)
        .inner_margin(Margin {
            left: 15,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(ITEM_SPACING_X, 6.0);
            if card.layout.compact {
                compact_rows(ui, profile, card, &mut changed, &mut action);
            } else {
                wide_row(ui, profile, card, &mut changed, &mut action);
            }
            if let Some(account) = card.info.and_then(|info| info.account.as_ref()) {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(RichText::new(account).color(theme::FG_DIM()).size(10.5));
                });
            }
        })
        .response;
    let rect = response.rect;
    ui.painter().rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.min.x + 1.0, rect.min.y + 6.0),
            Pos2::new(rect.min.x + 1.0 + ACCENT_STRIPE_WIDTH, rect.max.y - 6.0),
        ),
        CornerRadius::same(2),
        card.accent,
    );
    (changed, action)
}

fn wide_row(
    ui: &mut Ui,
    profile: &mut ClaudeProfile,
    card: &CardContext<'_>,
    changed: &mut bool,
    action: &mut Option<RowAction>,
) {
    ui.horizontal(|ui| {
        accent_dot(ui, card.accent);
        *changed |= name_edit(ui, &mut profile.name, card.layout.name_width, card);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            controls(ui, card, action);
            if hub_ui::secondary_button(ui, "Browse").clicked() {
                *action = Some(RowAction::Browse);
            }
            *changed |= dir_edit(ui, &mut profile.config_dir, card.issue);
        });
    });
}

fn compact_rows(
    ui: &mut Ui,
    profile: &mut ClaudeProfile,
    card: &CardContext<'_>,
    changed: &mut bool,
    action: &mut Option<RowAction>,
) {
    ui.horizontal(|ui| {
        accent_dot(ui, card.accent);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            controls(ui, card, action);
            let width = ui.available_width();
            *changed |= name_edit(ui, &mut profile.name, width, card);
        });
    });
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if hub_ui::secondary_button(ui, "Browse").clicked() {
                *action = Some(RowAction::Browse);
            }
            *changed |= dir_edit(ui, &mut profile.config_dir, card.issue);
        });
    });
}

/// Remove, down, up and the credential pill, added for a right-to-left layout.
fn controls(ui: &mut Ui, card: &CardContext<'_>, action: &mut Option<RowAction>) {
    if hub_ui::icon_button(ui, IconKind::Remove, "Remove profile").clicked() {
        *action = Some(RowAction::Remove);
    }
    let last = card.index + 1 >= card.count;
    if hub_ui::icon_button_enabled(ui, IconKind::Down, "Move down", !last).clicked() {
        *action = Some(RowAction::Down);
    }
    if hub_ui::icon_button_enabled(ui, IconKind::Up, "Move up", card.index > 0).clicked() {
        *action = Some(RowAction::Up);
    }
    credential_pill(ui, card.info.map(|info| info.credentials), card.pill_width);
}

/// Paints the pill right-aligned in a slot of `reserved_width`, so rows keep
/// their columns whichever label they show.
fn credential_pill(ui: &mut Ui, state: Option<CredentialState>, reserved_width: f32) {
    let (label, color, tooltip) = match state {
        Some(state) => (state.label(), state.color(), state.tooltip()),
        None => (CHECKING_LABEL, theme::FG_DIM(), "Reading credentials\u{2026}"),
    };
    let slot = hub_ui::pill_rect(Pos2::ZERO, reserved_width).size();
    let (rect, response) = ui.allocate_exact_size(slot, Sense::hover());
    let width = hub_ui::pill_width(ui.painter(), label).min(rect.width());
    let pill = hub_ui::pill_rect(Pos2::new(rect.right() - width, rect.top()), width);
    hub_ui::pill(ui.painter(), pill, label, color);
    response.on_hover_ui(|ui| {
        ui.label(RichText::new(tooltip).size(10.5));
    });
}

fn accent_dot(ui: &mut Ui, accent: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, accent);
}

fn name_edit(ui: &mut Ui, name: &mut String, width: f32, card: &CardContext<'_>) -> bool {
    let response = ui.add(
        TextEdit::singleline(name)
            .desired_width(width)
            .font(FontId::proportional(12.0))
            .hint_text("name"),
    );
    if card.focus_name {
        response.request_focus();
        response.scroll_to_me(Some(Align::Center));
    }
    mark_invalid(ui, &response, card.issue, IssueField::Name);
    response.changed()
}

fn dir_edit(ui: &mut Ui, dir: &mut String, issue: Option<&RowIssue>) -> bool {
    let response = ui.add(
        TextEdit::singleline(dir)
            .desired_width(ui.available_width())
            .font(FontId::monospace(11.0))
            .hint_text("~/.claude-name"),
    );
    mark_invalid(ui, &response, issue, IssueField::Dir);
    response.changed()
}

fn mark_invalid(ui: &Ui, response: &egui::Response, issue: Option<&RowIssue>, field: IssueField) {
    if let Some(issue) = issue.filter(|issue| issue.field == field) {
        let color = if issue.blocking {
            theme::PALETTE_RED()
        } else {
            theme::PALETTE_YELLOW()
        };
        ui.painter().rect_stroke(
            response.rect,
            CornerRadius::same(2),
            Stroke::new(1.0_f32, color),
            StrokeKind::Inside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::SettingsStatus;
    use super::*;

    const BROWSE: f32 = 64.0;
    const PILL: f32 = 55.0;

    #[test]
    fn select_claude_tab_switches_the_active_tab() {
        let mut editor = SettingsEditor {
            buffer: String::new(),
            original: String::new(),
            status: SettingsStatus::None,
            active_tab: SettingsTab::General,
            editing_config: None,
            claude_tab: ClaudeTabState::default(),
            workspaces_tab: super::super::workspaces::WorkspacesTabState::default(),
        };
        select_claude_tab(&mut editor);
        assert!(editor.active_tab == SettingsTab::Claude);
    }

    #[test]
    fn select_claude_tab_from_yaml_reparses_the_unsaved_buffer() {
        let yaml = horizon_core::Config::default().to_yaml().expect("yaml");
        let mut editor = SettingsEditor {
            buffer: yaml,
            original: String::new(),
            status: SettingsStatus::None,
            active_tab: SettingsTab::Yaml,
            editing_config: None,
            claude_tab: ClaudeTabState::default(),
            workspaces_tab: super::super::workspaces::WorkspacesTabState::default(),
        };
        select_claude_tab(&mut editor);
        assert!(editor.active_tab == SettingsTab::Claude);
        assert!(editor.editing_config.is_some());
    }

    #[test]
    fn narrow_panels_wrap_the_card() {
        for available in [262.0, 300.0, 420.0, 560.0] {
            assert!(row_layout(available, BROWSE, PILL).compact, "{available}");
        }
    }

    #[test]
    fn wide_panels_keep_one_row_with_a_roomy_dir_field() {
        for available in [660.0, 800.0, 1200.0] {
            let layout = row_layout(available, BROWSE, PILL);
            assert!(!layout.compact, "{available}");
            assert!((NAME_MIN_WIDTH..=NAME_MAX_WIDTH).contains(&layout.name_width));
            let fixed = CARD_CHROME_WIDTH + DOT_SIZE + BROWSE + PILL + 3.0 * ICON_BUTTON_WIDTH + 7.0 * ITEM_SPACING_X;
            let dir = available - fixed - layout.name_width;
            assert!(dir >= DIR_MIN_SHARE * available - 0.5, "{available}: dir {dir}");
        }
    }

    #[test]
    fn layout_switches_once_and_never_flaps_back() {
        let mut was_wide = false;
        for available in (200_u16..1400).map(f32::from) {
            let wide = !row_layout(available, BROWSE, PILL).compact;
            assert!(wide || !was_wide, "wrapped again at {available}");
            was_wide |= wide;
        }
        assert!(was_wide);
    }

    #[test]
    fn name_field_grows_with_the_row_within_bounds() {
        let narrow = row_layout(680.0, BROWSE, PILL).name_width;
        let wide = row_layout(2000.0, BROWSE, PILL).name_width;
        assert!(narrow <= wide);
        assert!((wide - NAME_MAX_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn degenerate_widths_do_not_panic() {
        for available in [0.0, -5.0, f32::MAX] {
            let _ = row_layout(available, BROWSE, PILL);
        }
    }
}
