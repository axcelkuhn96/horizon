//! Settings "Claude" tab: edit `claude_profiles` (spec: `docs/design/claude-hub-ui.md` §3).
//!
//! Credentials are only ever inspected for their state (`ok` / `expired` /
//! `missing`); the token itself is never read into a displayed or stored value.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{
    Align, Color32, Context, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextEdit, Ui, Vec2,
};
use horizon_core::claude_usage::{read_account, read_credentials, token_expired};
use horizon_core::{ClaudeProfile, dir_search, discover_claude_profiles, expand_config_dir};

use super::{SettingsEditor, SettingsTab, dim_label, section_card};
use crate::app::hub_ui::{self, IconKind};
use crate::dir_picker::{PickerEmptyState, PickerModalAction, PickerModalConfig, PickerModalState, split_path_display};
use crate::theme;

const DEBOUNCE_MS: u128 = 60;
const BROWSE_ROW_HEIGHT: f32 = 34.0;
/// Below this inner width a profile card wraps into two rows.
const COMPACT_WIDTH: f32 = 520.0;
const NAME_FIELD_WIDTH: f32 = 140.0;
const ACCENT_STRIPE_WIDTH: f32 = 3.0;

/// State of a profile's OAuth credentials. Carries no token data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialState {
    Ok,
    Expired,
    Missing,
}

impl CredentialState {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Expired => "expired",
            Self::Missing => "missing",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Ok => theme::PALETTE_GREEN(),
            Self::Expired => theme::PALETTE_YELLOW(),
            Self::Missing => theme::PALETTE_RED(),
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Self::Ok => "Signed in: credentials found and valid",
            Self::Expired => "Token expired: open this profile in a terminal to renew",
            Self::Missing => "No credentials found: run claude in a terminal with this profile to sign in",
        }
    }
}

/// Credential state of `config_dir` at `now_ms` (Unix milliseconds).
/// Unreadable, empty or malformed credential files all count as `Missing`.
pub(crate) fn credential_state(config_dir: &Path, now_ms: i64) -> CredentialState {
    match read_credentials(config_dir) {
        Ok(credentials) if token_expired(credentials.expires_at_ms, now_ms) => CredentialState::Expired,
        Ok(_) => CredentialState::Ok,
        Err(_) => CredentialState::Missing,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

// -- Pure list editing ------------------------------------------------------

fn unique_profile_name(profiles: &[ClaudeProfile]) -> String {
    let taken = |name: &str| profiles.iter().any(|p| p.name.trim().eq_ignore_ascii_case(name));
    let base = "profile";
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_string())
}

/// Appends an empty profile with a unique name and returns its index.
fn add_profile(profiles: &mut Vec<ClaudeProfile>) -> usize {
    profiles.push(ClaudeProfile {
        name: unique_profile_name(profiles),
        config_dir: String::new(),
    });
    profiles.len() - 1
}

fn remove_profile(profiles: &mut Vec<ClaudeProfile>, index: usize) -> bool {
    if index < profiles.len() {
        profiles.remove(index);
        true
    } else {
        false
    }
}

/// Moves the profile at `from` so it ends up at `to`; out-of-range is a no-op.
fn move_profile(profiles: &mut Vec<ClaudeProfile>, from: usize, to: usize) {
    if from == to || from >= profiles.len() || to >= profiles.len() {
        return;
    }
    let profile = profiles.remove(from);
    profiles.insert(to, profile);
}

/// Appends detected profiles whose name (case-insensitive) and directory are
/// both unseen; returns how many were added.
fn merge_detected(existing: &mut Vec<ClaudeProfile>, detected: Vec<ClaudeProfile>, home: &Path) -> usize {
    let mut added = 0;
    for profile in detected {
        let name = profile.name.trim().to_lowercase();
        let dir = expand_config_dir(profile.config_dir.trim(), home);
        let known = existing
            .iter()
            .any(|p| p.name.trim().to_lowercase() == name || expand_config_dir(p.config_dir.trim(), home) == dir);
        if !known {
            existing.push(profile);
            added += 1;
        }
    }
    added
}

// -- Validation -------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssueField {
    Name,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowIssue {
    field: IssueField,
    /// Errors block saving; warnings (missing directory) do not.
    blocking: bool,
    message: String,
}

/// First validation problem of the profile at `index`. Mirrors
/// `validate_claude_profiles` (which gates saving) with per-row granularity.
fn row_issue(profiles: &[ClaudeProfile], index: usize, dir_exists: bool) -> Option<RowIssue> {
    let profile = profiles.get(index)?;
    let name = profile.name.trim();
    let issue = |field, blocking, message: &str| {
        Some(RowIssue {
            field,
            blocking,
            message: message.to_string(),
        })
    };
    if name.is_empty() {
        return issue(IssueField::Name, true, "Name must not be empty");
    }
    let lower = name.to_lowercase();
    if profiles[..index].iter().any(|p| p.name.trim().to_lowercase() == lower) {
        return issue(IssueField::Name, true, "Another profile already uses this name");
    }
    if profile.config_dir.trim().is_empty() {
        return issue(IssueField::Dir, true, "Config directory must not be empty");
    }
    if !dir_exists {
        return issue(IssueField::Dir, false, "Directory does not exist yet");
    }
    None
}

// -- Per-profile cached facts -------------------------------------------------

struct ProfileInfo {
    credentials: CredentialState,
    dir_exists: bool,
    /// `"Name · email"` of the signed-in account, when Claude Code recorded one.
    account: Option<String>,
}

fn inspect_profile(raw_dir: &str, home: &Path, now_ms: i64) -> ProfileInfo {
    let raw_dir = raw_dir.trim();
    if raw_dir.is_empty() {
        return ProfileInfo {
            credentials: CredentialState::Missing,
            dir_exists: false,
            account: None,
        };
    }
    let dir = expand_config_dir(raw_dir, home);
    let dir_exists = dir.is_dir();
    let (name, email) = if dir_exists {
        read_account(&dir, home)
    } else {
        (None, None)
    };
    let account = match (name, email) {
        (Some(name), Some(email)) => Some(format!("{name} \u{b7} {email}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    };
    ProfileInfo {
        credentials: credential_state(&dir, now_ms),
        dir_exists,
        account,
    }
}

// -- Tab state ----------------------------------------------------------------

#[derive(Default)]
pub(super) struct ClaudeTabState {
    /// Credential facts keyed by the raw `config_dir` string; refreshed when the
    /// tab is (re)opened and on "Detect accounts", filled lazily on a miss.
    infos: HashMap<String, ProfileInfo>,
    last_pass: Option<u64>,
    accents: (Vec<String>, Vec<usize>),
    detect_rx: Option<mpsc::Receiver<Vec<ClaudeProfile>>>,
    detect_result: Option<String>,
    /// Profiles from this index on were just detected and get a highlight.
    highlight_from: Option<usize>,
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

    fn info(&mut self, raw_dir: &str, home: &Path) -> &ProfileInfo {
        self.infos
            .entry(raw_dir.to_string())
            .or_insert_with(|| inspect_profile(raw_dir, home, now_ms()))
    }
}

/// Switches the settings editor to the Claude tab (used by the launcher's
/// "Manage profiles" entry).
#[allow(dead_code, reason = "called by the Claude launcher (Task 5)")]
pub(crate) fn select_claude_tab(editor: &mut SettingsEditor) {
    editor.active_tab = SettingsTab::Claude;
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
        tab.infos.clear();
    }
    tab.last_pass = Some(pass);

    let home = home_dir();
    let mut changed = poll_detect(tab, &mut config.claude_profiles, &home);
    changed |= render(ui, &mut config.claude_profiles, tab, &home);
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
    tab.infos.clear();
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

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Claude profiles")
                .color(theme::FG_SOFT())
                .size(13.0)
                .strong(),
        );
        if !profiles.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                want_add = hub_ui::secondary_button(ui, "+ Add profile").clicked();
                want_detect = detect_button(ui, detecting);
            });
        }
    });
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
        let (row_changed, action) = render_rows(ui, profiles, tab, home);
        changed |= row_changed;
        if let Some((index, action)) = action {
            changed |= apply_action(profiles, tab, index, action);
        }
    }

    if want_add {
        add_profile(profiles);
        changed = true;
    }
    if want_detect && !detecting {
        start_detect(tab, ui.ctx(), home);
    }
    changed |= render_browse(ui.ctx(), profiles, tab);
    changed
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

struct CardContext<'a> {
    index: usize,
    count: usize,
    accent: Color32,
    info: &'a ProfileInfo,
    issue: Option<&'a RowIssue>,
    highlighted: bool,
    compact: bool,
}

fn render_rows(
    ui: &mut Ui,
    profiles: &mut [ClaudeProfile],
    tab: &mut ClaudeTabState,
    home: &Path,
) -> (bool, Option<(usize, RowAction)>) {
    let mut changed = false;
    let mut action = None;
    let slots = tab.accent_slots(profiles).to_vec();
    let compact = ui.available_width() < COMPACT_WIDTH;
    let highlight_from = tab.highlight_from;
    for index in 0..profiles.len() {
        let dir = profiles[index].config_dir.clone();
        let (credentials, dir_exists, account) = {
            let info = tab.info(&dir, home);
            (info.credentials, info.dir_exists, info.account.clone())
        };
        let info = ProfileInfo {
            credentials,
            dir_exists,
            account,
        };
        let issue = row_issue(profiles, index, info.dir_exists);
        let context = CardContext {
            index,
            count: profiles.len(),
            accent: hub_ui::accent_color(slots.get(index).copied().unwrap_or(index)),
            info: &info,
            issue: issue.as_ref(),
            highlighted: highlight_from.is_some_and(|from| index >= from),
            compact,
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
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            if card.compact {
                compact_rows(ui, profile, card, &mut changed, &mut action);
            } else {
                wide_row(ui, profile, card, &mut changed, &mut action);
            }
            if let Some(account) = &card.info.account {
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
        *changed |= name_edit(ui, &mut profile.name, NAME_FIELD_WIDTH, card.issue);
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
            *changed |= name_edit(ui, &mut profile.name, width, card.issue);
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
    credential_pill(ui, card.info.credentials);
}

fn credential_pill(ui: &mut Ui, state: CredentialState) {
    let width = hub_ui::pill_width(ui.painter(), state.label());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    hub_ui::pill(ui.painter(), rect, state.label(), state.color());
    response.on_hover_ui(|ui| {
        ui.label(RichText::new(state.tooltip()).size(10.5));
    });
}

fn accent_dot(ui: &mut Ui, accent: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, accent);
}

fn name_edit(ui: &mut Ui, name: &mut String, width: f32, issue: Option<&RowIssue>) -> bool {
    let response = ui.add(
        TextEdit::singleline(name)
            .desired_width(width)
            .font(FontId::proportional(12.0))
            .hint_text("name"),
    );
    mark_invalid(ui, &response, issue, IssueField::Name);
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

// -- Directory browser ----------------------------------------------------------

/// Local directory search modal that fills one profile's `config_dir`.
struct BrowseState {
    target: usize,
    modal: PickerModalState,
    results: Vec<PathBuf>,
    search_rx: Option<mpsc::Receiver<Vec<PathBuf>>>,
    last_query_sent: String,
    last_query_time: Instant,
    loaded: bool,
}

enum BrowseOutcome {
    Open,
    Cancelled,
    Picked(PathBuf),
}

impl BrowseState {
    fn new(target: usize, current_dir: &str) -> Self {
        let expanded = dir_search::expand_tilde(current_dir.trim());
        let query = if !current_dir.trim().is_empty() && expanded.is_dir() {
            format!("{}/", dir_search::abbreviate_home(&expanded).trim_end_matches('/'))
        } else {
            "~/".to_string()
        };
        Self {
            target,
            search_rx: Some(dir_search::spawn_lookup(query.clone())),
            modal: PickerModalState::new(query.clone()),
            results: Vec::new(),
            last_query_sent: query,
            last_query_time: Instant::now(),
            loaded: false,
        }
    }

    fn update_search(&mut self) {
        if let Some(rx) = &self.search_rx {
            match rx.try_recv() {
                Ok(results) => {
                    self.results = results;
                    self.modal.clamp_selected(self.results.len());
                    self.search_rx = None;
                    self.loaded = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.search_rx = None,
            }
        }
        if self.modal.query() != self.last_query_sent && self.last_query_time.elapsed().as_millis() >= DEBOUNCE_MS {
            self.last_query_sent = self.modal.query().to_string();
            self.last_query_time = Instant::now();
            self.search_rx = Some(dir_search::spawn_lookup(self.last_query_sent.clone()));
        }
    }

    fn show(&mut self, ctx: &Context) -> BrowseOutcome {
        self.update_search();
        let message = if self.loaded {
            "No directories found"
        } else {
            "Searching..."
        };
        let action = self.modal.show(
            ctx,
            &PickerModalConfig {
                id_source: "claude_profile_dir_picker",
                heading: "Select Claude config directory",
                hint_text: "Type a path or search...",
                status_text: None,
                empty_state: PickerEmptyState {
                    message,
                    color: theme::FG_DIM(),
                },
                footer_action_label: None,
            },
            &self.results,
            |ui, width, index, path, selected| browse_row(ui, width, index, path, selected),
        );
        match action {
            PickerModalAction::None | PickerModalAction::FooterAction => BrowseOutcome::Open,
            PickerModalAction::Cancelled => BrowseOutcome::Cancelled,
            PickerModalAction::ClickedRow(index) => self
                .results
                .get(index)
                .map_or(BrowseOutcome::Open, |path| BrowseOutcome::Picked(path.clone())),
            PickerModalAction::CompleteSelection => {
                if let Some(path) = self.results.get(self.modal.selected_index()) {
                    self.modal
                        .set_query(format!("{}/", dir_search::abbreviate_home(path).trim_end_matches('/')));
                    self.last_query_time = Instant::now();
                }
                BrowseOutcome::Open
            }
            PickerModalAction::Submit => self.submit(),
        }
    }

    fn submit(&self) -> BrowseOutcome {
        let typed = dir_search::expand_tilde(self.modal.query().trim());
        if !self.modal.query().trim().is_empty() && typed.is_dir() {
            return BrowseOutcome::Picked(typed);
        }
        self.results
            .get(self.modal.selected_index())
            .map_or(BrowseOutcome::Open, |path| BrowseOutcome::Picked(path.clone()))
    }
}

fn browse_row(ui: &mut Ui, width: f32, index: usize, path: &Path, selected: bool) -> bool {
    let display = dir_search::abbreviate_home(path);
    let row = ui.allocate_space(Vec2::new(width, BROWSE_ROW_HEIGHT)).1;
    let response = ui.interact(row, ui.make_persistent_id(("claude_dir_row", index)), Sense::click());
    let painter = ui.painter_at(row);
    if selected {
        painter.rect_filled(
            row,
            CornerRadius::same(8),
            theme::alpha(theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.28), 200),
        );
    } else if response.hovered() {
        painter.rect_filled(row, CornerRadius::same(8), theme::alpha(theme::PANEL_BG_ALT(), 160));
    }
    let (dir_part, name_part) = split_path_display(&display);
    let font = FontId::monospace(12.5);
    let y = row.center().y;
    let name_color = if selected { theme::FG() } else { theme::FG_SOFT() };
    let mut x = row.min.x + 14.0;
    if !dir_part.is_empty() {
        x = painter
            .text(
                Pos2::new(x, y),
                egui::Align2::LEFT_CENTER,
                &dir_part,
                font.clone(),
                theme::FG_DIM(),
            )
            .max
            .x;
    }
    painter.text(Pos2::new(x, y), egui::Align2::LEFT_CENTER, &name_part, font, name_color);
    response.clicked()
}

fn render_browse(ctx: &Context, profiles: &mut [ClaudeProfile], tab: &mut ClaudeTabState) -> bool {
    let Some(browse) = tab.browse.as_mut() else {
        return false;
    };
    match browse.show(ctx) {
        BrowseOutcome::Open => false,
        BrowseOutcome::Cancelled => {
            tab.browse = None;
            false
        }
        BrowseOutcome::Picked(path) => {
            let target = browse.target;
            tab.browse = None;
            profiles.get_mut(target).is_some_and(|profile| {
                profile.config_dir = dir_search::abbreviate_home(&path);
                true
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000;

    fn profile(name: &str, dir: &str) -> ClaudeProfile {
        ClaudeProfile {
            name: name.to_string(),
            config_dir: dir.to_string(),
        }
    }

    fn write_credentials(dir: &Path, body: &str) {
        std::fs::write(dir.join(".credentials.json"), body).unwrap();
    }

    fn creds(expires_at: i64) -> String {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"sk-secret-token","expiresAt":{expires_at}}}}}"#)
    }

    #[test]
    fn credential_state_ok_expired_missing() {
        let ok = tempfile::tempdir().unwrap();
        write_credentials(ok.path(), &creds(NOW + 60_001));
        assert_eq!(credential_state(ok.path(), NOW), CredentialState::Ok);

        let expired = tempfile::tempdir().unwrap();
        write_credentials(expired.path(), &creds(NOW + 60_000));
        assert_eq!(credential_state(expired.path(), NOW), CredentialState::Expired);

        let past = tempfile::tempdir().unwrap();
        write_credentials(past.path(), &creds(NOW - 5_000));
        assert_eq!(credential_state(past.path(), NOW), CredentialState::Expired);

        let missing = tempfile::tempdir().unwrap();
        assert_eq!(credential_state(missing.path(), NOW), CredentialState::Missing);
        assert_eq!(
            credential_state(&missing.path().join("nonexistent"), NOW),
            CredentialState::Missing
        );

        let malformed = tempfile::tempdir().unwrap();
        write_credentials(malformed.path(), "{ not json");
        assert_eq!(credential_state(malformed.path(), NOW), CredentialState::Missing);

        let empty = tempfile::tempdir().unwrap();
        write_credentials(empty.path(), "");
        assert_eq!(credential_state(empty.path(), NOW), CredentialState::Missing);
    }

    #[test]
    fn credential_state_never_exposes_token() {
        let dir = tempfile::tempdir().unwrap();
        write_credentials(dir.path(), &creds(NOW + 600_000));
        let state = credential_state(dir.path(), NOW);
        let rendered = format!("{state:?} {} {}", state.label(), state.tooltip());
        assert!(!rendered.contains("sk-secret-token"));
        // The cached per-profile facts hold no token either.
        let info = inspect_profile(dir.path().to_str().unwrap(), Path::new("/nonexistent-home"), NOW);
        assert_eq!(info.credentials, CredentialState::Ok);
        assert!(info.account.is_none());
    }

    #[test]
    fn inspect_profile_with_empty_dir_does_not_touch_the_filesystem() {
        let info = inspect_profile("  ", Path::new("/nonexistent-home"), NOW);
        assert_eq!(info.credentials, CredentialState::Missing);
        assert!(!info.dir_exists);
    }

    #[test]
    fn merge_detected_skips_known() {
        let home = Path::new("/home/me");
        let mut existing = vec![profile("Work", "/home/me/.claude-work/"), profile("x", "~/.claude-x")];
        let detected = vec![
            profile("work", "~/.claude-w2"),
            profile("other", "~/.claude-work"),
            profile("claude", "~/.claude"),
            profile("x2", "/home/me/.claude-x"),
        ];
        assert_eq!(merge_detected(&mut existing, detected, home), 1);
        assert_eq!(existing.len(), 3);
        assert_eq!(existing[2], profile("claude", "~/.claude"));
        assert_eq!(
            merge_detected(&mut existing, vec![profile("claude", "~/.claude")], home),
            0
        );
    }

    #[test]
    fn move_profile_reorders() {
        let mut list = vec![profile("a", "1"), profile("b", "2"), profile("c", "3")];
        move_profile(&mut list, 0, 1);
        assert_eq!(names(&list), ["b", "a", "c"]);
        move_profile(&mut list, 2, 0);
        assert_eq!(names(&list), ["c", "b", "a"]);
        move_profile(&mut list, 1, 1);
        move_profile(&mut list, 0, 3);
        move_profile(&mut list, 5, 0);
        assert_eq!(names(&list), ["c", "b", "a"]);
    }

    #[test]
    fn add_and_remove_profile() {
        let mut list = vec![profile("profile", "~/.claude-a")];
        assert_eq!(add_profile(&mut list), 1);
        assert_eq!(list[1].name, "profile 2");
        assert!(list[1].config_dir.is_empty());
        assert_eq!(add_profile(&mut list), 2);
        assert_eq!(list[2].name, "profile 3");
        assert!(remove_profile(&mut list, 0));
        assert!(!remove_profile(&mut list, 9));
        assert_eq!(names(&list), ["profile 2", "profile 3"]);
    }

    #[test]
    fn row_issue_covers_name_dir_and_warning() {
        let list = vec![
            profile("a", "~/.claude-a"),
            profile(" A ", "~/.claude-b"),
            profile("", "~/.claude-c"),
            profile("d", "  "),
            profile("e", "~/.claude-e"),
        ];
        assert_eq!(row_issue(&list, 0, true), None);
        let dup = row_issue(&list, 1, true).unwrap();
        assert_eq!((dup.field, dup.blocking), (IssueField::Name, true));
        assert_eq!(row_issue(&list, 2, true).unwrap().field, IssueField::Name);
        let empty_dir = row_issue(&list, 3, true).unwrap();
        assert_eq!((empty_dir.field, empty_dir.blocking), (IssueField::Dir, true));
        let missing_dir = row_issue(&list, 4, false).unwrap();
        assert_eq!((missing_dir.field, missing_dir.blocking), (IssueField::Dir, false));
        assert_eq!(row_issue(&list, 9, true), None);
    }

    #[test]
    fn blocking_row_issues_agree_with_core_validation() {
        let cases = [
            vec![profile("a", "~/.claude-a"), profile("b", "~/.claude-b")],
            vec![profile("a", "~/x"), profile("A", "~/y")],
            vec![profile(" ", "~/x")],
            vec![profile("a", "")],
        ];
        for list in cases {
            let blocking = (0..list.len()).any(|i| row_issue(&list, i, true).is_some_and(|issue| issue.blocking));
            assert_eq!(blocking, horizon_core::validate_claude_profiles(&list).is_err());
        }
    }

    fn names(list: &[ClaudeProfile]) -> Vec<&str> {
        list.iter().map(|p| p.name.as_str()).collect()
    }
}
