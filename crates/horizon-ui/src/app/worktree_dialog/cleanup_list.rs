//! The "Clean up worktrees" list (spec section 11). Shows exactly which
//! directories would be removed, why each is a candidate, and what blocks the
//! rest. Rows that are not `safe()` can never be ticked; the destructive
//! button only acts on ticked, safe rows and the worker re-checks every one.
//! Only merged worktrees holding nothing ignored are ticked by default; a row
//! with ignored files (`.env`, `node_modules/`...) needs a second, explicit
//! click, and the worker refuses it unless that confirmation came along.

use std::path::{Path, PathBuf};

use egui::{
    Align, Align2, Color32, Context, CornerRadius, CursorIcon, FontId, Id, Layout, Pos2, Rect, RichText, ScrollArea,
    Sense, Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::git_overview::{Blocker, CleanupCandidate, CleanupReason, RemoveOutcome, RemoveStatus};

use super::{body_frame, checkbox_row, paint_checkbox, repo_display_name, short_display_path};
use crate::app::hub_ui::{self, ModalPlacement};
use crate::theme;

const MODAL_WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 40.0;
const VISIBLE_ROWS: f32 = 7.0;
const ROW_RADIUS: u8 = 8;
const MESSAGE_HEIGHT: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CleanupPhase {
    /// Waiting for the classification job with this id.
    Loading(u64),
    Ready,
    /// Waiting for the removal job with this id.
    Removing(u64),
}

pub(super) enum CleanupOutcome {
    Open,
    Submit,
    Close,
}

struct CleanupRow {
    candidate: CleanupCandidate,
    title: String,
    short_path: String,
    tooltip: String,
    checked: bool,
    /// First click on a row with ignored files: the second one confirms.
    armed: bool,
    /// Why the last removal attempt left this row in place.
    note: Option<String>,
}

impl CleanupRow {
    fn new(repo_root: &Path, candidate: CleanupCandidate) -> Self {
        let mut row = Self {
            title: candidate.branch.clone().unwrap_or_else(|| "(detached HEAD)".to_owned()),
            short_path: short_display_path(repo_root, &candidate.path),
            tooltip: String::new(),
            checked: false,
            armed: false,
            note: None,
            candidate,
        };
        row.refresh();
        row
    }

    /// Rebuilds the strings and the default tick after the candidate changed.
    fn refresh(&mut self) {
        let path = self.candidate.path.display();
        let mut tooltip = if self.candidate.safe() {
            format!("{path}\nNo uncommitted changes and unlocked.")
        } else {
            let reasons: Vec<&str> = self.candidate.blockers.iter().map(|b| b.explanation()).collect();
            format!("{path}\nBlocked: {}", reasons.join(" "))
        };
        match self.candidate.reason {
            CleanupReason::Fresh => tooltip.push_str(
                "\nIts branch is at the default branch's tip: no commits of its own yet (or merged by fast-forward).",
            ),
            CleanupReason::Prunable => tooltip.push_str("\nThe folder is already gone: only git's record is removed."),
            CleanupReason::Merged | CleanupReason::Orphan => {}
        }
        if self.candidate.ignored.any() {
            tooltip.push_str(&format!(
                "\nIgnored files are not changes, but removing the folder deletes them: {}.",
                self.candidate.ignored.describe()
            ));
        }
        self.tooltip = tooltip;
        self.checked = self.candidate.preselect();
        self.armed = false;
    }

    /// Ticks or clears the row. A row holding ignored files cannot be ticked here.
    fn set_checked(&mut self, checked: bool) {
        self.checked = checked && self.candidate.safe() && !self.candidate.ignored.any();
        self.armed = false;
    }

    /// A click on the row: clear when ticked; tick when plain; for a row with
    /// ignored files the first click only arms it and the second ticks it.
    fn toggle(&mut self) {
        if !self.candidate.safe() {
            return;
        }
        if self.checked {
            self.set_checked(false);
        } else if self.candidate.ignored.any() && !self.armed {
            self.armed = true;
        } else {
            self.checked = true;
            self.armed = false;
        }
    }
}

/// Rows for `candidates` (just `only` when set), safe ones first.
fn build_rows(repo_root: &Path, candidates: Vec<CleanupCandidate>, only: Option<&Path>) -> Vec<CleanupRow> {
    let mut rows: Vec<CleanupRow> = candidates
        .into_iter()
        .filter(|candidate| only.is_none_or(|path| candidate.path == path))
        .map(|candidate| CleanupRow::new(repo_root, candidate))
        .collect();
    rows.sort_by_key(|row| (!row.candidate.safe(), !row.candidate.preselect()));
    rows
}

/// Paths the destructive button acts on: ticked rows that are safe.
fn selected(rows: &[CleanupRow]) -> Vec<PathBuf> {
    rows.iter()
        .filter(|row| row.checked && row.candidate.safe())
        .map(|row| row.candidate.path.clone())
        .collect()
}

/// Ticked rows whose ignored files the user agreed to delete.
fn confirmed_ignored(rows: &[CleanupRow]) -> Vec<PathBuf> {
    rows.iter()
        .filter(|row| row.checked && row.candidate.safe() && row.candidate.ignored.any())
        .map(|row| row.candidate.path.clone())
        .collect()
}

fn confirm_label(count: usize) -> String {
    match count {
        0 => "Remove worktrees".to_owned(),
        1 => "Remove 1 worktree".to_owned(),
        n => format!("Remove {n} worktrees"),
    }
}

fn kept_phrase(blocker: Blocker) -> &'static str {
    match blocker {
        Blocker::Dirty => "became dirty",
        Blocker::Locked => "became locked",
        Blocker::ProtectedByWorkspace => "now in use",
        Blocker::UnreachableCommits => "has unreachable commits",
        Blocker::DefaultBranchUnknown => "default branch unknown",
        Blocker::Main => "is the main worktree",
        Blocker::NestedWorktree => "contains a worktree",
        Blocker::Submodules => "has submodules",
        Blocker::StillOnDisk => "still on disk",
        Blocker::ParentMissing => "folder unavailable",
    }
}

/// `Removed 2, kept 1 (became dirty)`.
fn removal_summary(outcomes: &[RemoveOutcome]) -> String {
    let (mut removed, mut kept, mut failed) = (0_usize, 0_usize, 0_usize);
    let mut reasons: Vec<&'static str> = Vec::new();
    for outcome in outcomes {
        match &outcome.status {
            RemoveStatus::Removed { .. } => removed += 1,
            RemoveStatus::Kept(blockers) => {
                kept += 1;
                for blocker in blockers {
                    let phrase = kept_phrase(*blocker);
                    if !reasons.contains(&phrase) {
                        reasons.push(phrase);
                    }
                }
            }
            RemoveStatus::IgnoredNotConfirmed(_) => {
                kept += 1;
                if !reasons.contains(&"has ignored files") {
                    reasons.push("has ignored files");
                }
            }
            RemoveStatus::NotCandidate | RemoveStatus::Refused => kept += 1,
            RemoveStatus::Failed(_) => failed += 1,
        }
    }
    let mut text = format!("Removed {removed}");
    if kept > 0 {
        text.push_str(&format!(", kept {kept}"));
        if !reasons.is_empty() {
            text.push_str(&format!(" ({})", reasons.join(", ")));
        }
    }
    if failed > 0 {
        text.push_str(&format!(", {failed} failed"));
    }
    text
}

pub(super) struct CleanupDialog {
    workspace_name: String,
    repo_name: String,
    repo_root: PathBuf,
    /// Scoped to one worktree ("Remove worktree...").
    only: Option<PathBuf>,
    pub(super) phase: CleanupPhase,
    pub(super) error: Option<String>,
    summary: Option<String>,
    rows: Vec<CleanupRow>,
    delete_branches: bool,
}

impl CleanupDialog {
    pub(super) fn new(workspace_name: String, repo_root: PathBuf, only: Option<PathBuf>) -> Self {
        Self {
            workspace_name,
            repo_name: repo_display_name(&repo_root),
            repo_root,
            only,
            phase: CleanupPhase::Loading(0),
            error: None,
            summary: None,
            rows: Vec::new(),
            // Off: deleting a branch is a separate decision from removing its folder.
            delete_branches: false,
        }
    }

    pub(super) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub(super) fn delete_branches(&self) -> bool {
        self.delete_branches
    }

    pub(super) fn confirmed_ignored_paths(&self) -> Vec<PathBuf> {
        if self.phase == CleanupPhase::Ready {
            confirmed_ignored(&self.rows)
        } else {
            Vec::new()
        }
    }

    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        if self.phase == CleanupPhase::Ready {
            selected(&self.rows)
        } else {
            Vec::new()
        }
    }

    pub(super) fn apply_classification(&mut self, result: Result<Vec<CleanupCandidate>, String>) {
        match result {
            Ok(candidates) => self.rows = build_rows(&self.repo_root, candidates, self.only.as_deref()),
            Err(message) => self.error = Some(message),
        }
        self.phase = CleanupPhase::Ready;
    }

    /// Folds the worker's outcomes into the list and returns the summary line.
    pub(super) fn apply_removal(&mut self, outcomes: &[RemoveOutcome]) -> String {
        for outcome in outcomes {
            let Some(index) = self.rows.iter().position(|row| row.candidate.path == outcome.path) else {
                continue;
            };
            match &outcome.status {
                RemoveStatus::Removed { .. } | RemoveStatus::NotCandidate | RemoveStatus::Refused => {
                    self.rows.remove(index);
                }
                RemoveStatus::IgnoredNotConfirmed(found) => {
                    let row = &mut self.rows[index];
                    row.candidate.ignored.clone_from(found);
                    row.refresh();
                    row.note = Some("Kept: it holds ignored files you did not confirm".to_owned());
                }
                RemoveStatus::Kept(blockers) => {
                    let row = &mut self.rows[index];
                    row.candidate.blockers.clone_from(blockers);
                    row.refresh();
                    row.note = blockers
                        .first()
                        .map(|blocker| format!("Kept: {}", kept_phrase(*blocker)));
                }
                RemoveStatus::Failed(message) => {
                    let row = &mut self.rows[index];
                    row.set_checked(false);
                    row.note = Some(message.clone());
                }
            }
        }
        let summary = removal_summary(outcomes);
        self.summary = Some(summary.clone());
        self.phase = CleanupPhase::Ready;
        summary
    }

    fn has_merged_rows(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.candidate.reason == CleanupReason::Merged)
    }
}

pub(super) fn show(ctx: &Context, dialog: &mut CleanupDialog) -> CleanupOutcome {
    let mut submit = false;
    let outcome = hub_ui::show_modal(
        ctx,
        Id::new("cleanup_worktrees_modal"),
        MODAL_WIDTH,
        ModalPlacement::TopFraction(0.14),
        None,
        |ui| {
            body_frame().show(ui, |ui| {
                submit = contents(ui, dialog);
            });
        },
    );
    if matches!(dialog.phase, CleanupPhase::Removing(_)) {
        // Removal in flight: the dialog stays until the worker answers.
        CleanupOutcome::Open
    } else if outcome.dismissed {
        CleanupOutcome::Close
    } else if submit {
        CleanupOutcome::Submit
    } else {
        CleanupOutcome::Open
    }
}

fn contents(ui: &mut Ui, dialog: &mut CleanupDialog) -> bool {
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 8.0);
    header(ui, dialog);
    let phase = dialog.phase;
    let ready = phase == CleanupPhase::Ready;
    match phase {
        CleanupPhase::Loading(_) => message_block(ui, "Scanning worktrees\u{2026}"),
        _ if dialog.rows.is_empty() => message_block(ui, empty_text(dialog.only.is_some())),
        _ => {
            ScrollArea::vertical()
                .max_height(ROW_HEIGHT * VISIBLE_ROWS)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for row in &mut dialog.rows {
                        paint_row(ui, row, ready);
                    }
                });
        }
    }
    if dialog.has_merged_rows() {
        let toggle = checkbox_row(ui, "also delete merged branches", dialog.delete_branches);
        if toggle.clicked() && ready {
            dialog.delete_branches = !dialog.delete_branches;
        }
    }
    status_line(ui, dialog);
    footer(ui, dialog)
}

fn empty_text(scoped: bool) -> &'static str {
    if scoped {
        "This worktree cannot be removed here: it has unmerged work, or it is the main worktree."
    } else {
        "Nothing to clean up: no merged, prunable or orphaned worktrees."
    }
}

fn header(ui: &mut Ui, dialog: &CleanupDialog) {
    ui.horizontal(|ui| {
        let title = if dialog.only.is_some() {
            "Remove worktree"
        } else {
            "Clean up worktrees"
        };
        ui.label(RichText::new(title).size(14.0).strong().color(theme::FG()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let scope = if dialog.workspace_name.is_empty() {
                dialog.repo_name.as_str()
            } else {
                dialog.workspace_name.as_str()
            };
            ui.label(RichText::new(scope).size(10.5).color(theme::FG_DIM()));
        });
    });
    ui.add(
        egui::Label::new(
            RichText::new(
                "Clean = no uncommitted changes. Ignored files (.env, node_modules...) are not changes: \
                 they are listed and deleted with the folder, so those rows need a second click.",
            )
            .size(10.5)
            .color(theme::FG_DIM()),
        )
        .wrap(),
    );
    ui.add_space(2.0);
}

fn message_block(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 72.0), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(11.5),
        theme::FG_DIM(),
    );
}

fn reason_color(reason: CleanupReason) -> Color32 {
    match reason {
        CleanupReason::Merged => theme::PALETTE_GREEN(),
        CleanupReason::Prunable => theme::FG_DIM(),
        CleanupReason::Orphan | CleanupReason::Fresh => theme::PALETTE_YELLOW(),
    }
}

fn blocker_color(blocker: Blocker) -> Color32 {
    match blocker {
        Blocker::Dirty | Blocker::Submodules => theme::PALETTE_YELLOW(),
        Blocker::UnreachableCommits => theme::PALETTE_RED(),
        Blocker::ProtectedByWorkspace => theme::ACCENT(),
        _ => theme::FG_DIM(),
    }
}

/// One 40 px row: checkbox, branch and path, reason pill, blocker pills.
fn paint_row(ui: &mut Ui, row: &mut CleanupRow, interactive: bool) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    let safe = row.candidate.safe();
    let enabled = safe && interactive;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, row.checked, &row.title));
    if enabled && response.clicked() {
        row.toggle();
    }
    let response = response.on_hover_text(row.tooltip.as_str());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let fade = |color: Color32| if safe { color } else { theme::alpha(color, 140) };
    let painter = ui.painter();
    if enabled && response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(ROW_RADIUS), theme::PANEL_BG_ALT());
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let boxed = Rect::from_center_size(Pos2::new(rect.left() + 18.0, rect.center().y), Vec2::splat(14.0));
    paint_checkbox(painter, boxed, row.checked, enabled);

    // Pills, right to left: blockers last in reading order, reason first.
    let mut right = rect.right() - 10.0;
    let mut pills: Vec<(String, Color32)> = vec![(
        row.candidate.reason.label().to_owned(),
        reason_color(row.candidate.reason),
    )];
    if row.candidate.ignored.any() {
        let text = if row.candidate.ignored.count == 0 {
            "ignored files?".to_owned()
        } else {
            format!("{} ignored", row.candidate.ignored.count)
        };
        pills.push((text, theme::PALETTE_YELLOW()));
    }
    pills.extend(
        row.candidate
            .blockers
            .iter()
            .map(|blocker| (blocker.label().to_owned(), blocker_color(*blocker))),
    );
    for (text, color) in pills.iter().rev() {
        let width = hub_ui::pill_width(painter, text);
        let pill_rect = hub_ui::pill_rect(Pos2::new(right - width, rect.center().y - 9.0), width);
        hub_ui::pill(painter, pill_rect, text, fade(*color));
        right = pill_rect.min.x - 6.0;
    }

    let text_x = rect.left() + 38.0;
    let max_width = (right - text_x - 6.0).max(40.0);
    let title = hub_ui::elided(painter, &row.title, FontId::proportional(12.0), max_width);
    painter.galley(Pos2::new(text_x, rect.top() + 5.0), title, fade(theme::FG()));
    let warning;
    let (second, color) = match &row.note {
        Some(note) => (note.as_str(), theme::PALETTE_RED()),
        None if row.armed => {
            warning = format!("Click again to also delete {}", row.candidate.ignored.describe());
            (warning.as_str(), theme::PALETTE_RED())
        }
        None if row.checked && row.candidate.ignored.any() => {
            warning = format!("Will also delete {}", row.candidate.ignored.describe());
            (warning.as_str(), theme::PALETTE_YELLOW())
        }
        None => (row.short_path.as_str(), fade(theme::FG_DIM())),
    };
    let second = hub_ui::elided(painter, second, FontId::monospace(10.5), max_width);
    painter.galley(Pos2::new(text_x, rect.top() + 22.0), second, color);
    hub_ui::paint_focus_ring(painter, &response, rect, ROW_RADIUS);
}

/// Fixed-height line for the removal summary or an error.
fn status_line(ui: &mut Ui, dialog: &CleanupDialog) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), MESSAGE_HEIGHT), Sense::hover());
    let (text, color) = match (&dialog.error, &dialog.summary) {
        (Some(error), _) => (error.as_str(), theme::PALETTE_RED()),
        (None, Some(summary)) => (summary.as_str(), theme::FG_SOFT()),
        (None, None) => return,
    };
    let galley = hub_ui::elided(ui.painter(), text, FontId::proportional(11.0), rect.width());
    ui.painter().galley(rect.min, galley, color);
}

/// Footer with the Esc hint and the destructive button; `true` when clicked.
fn footer(ui: &mut Ui, dialog: &CleanupDialog) -> bool {
    let count = dialog.selected_paths().len();
    let removing = matches!(dialog.phase, CleanupPhase::Removing(_));
    let label = if removing {
        "Removing \u{b7}\u{b7}\u{b7}".to_owned()
    } else {
        confirm_label(count)
    };
    let mut clicked = false;
    ui.horizontal(|ui| {
        hub_ui::kbd_hint(ui, "Esc", if removing { "wait" } else { "close" });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            clicked = danger_button(ui, &label, count > 0 && !removing).clicked();
        });
    });
    clicked
}

/// Destructive button: `PALETTE_RED` fill, `BG` text, 30 px high.
fn danger_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    const RADIUS: u8 = 8;
    let font = FontId::proportional(12.0);
    let enabled = enabled && ui.is_enabled();
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new((text_width + 32.0).max(64.0), 30.0), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let (fill, text) = if !enabled {
            (theme::alpha(theme::PALETTE_RED(), 70), theme::alpha(theme::BG(), 160))
        } else if response.is_pointer_button_down_on() {
            (theme::blend(theme::PALETTE_RED(), theme::BG(), 0.18), theme::BG())
        } else if response.hovered() {
            (theme::blend(theme::PALETTE_RED(), theme::FG(), 0.12), theme::BG())
        } else {
            (theme::PALETTE_RED(), theme::BG())
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(RADIUS), fill);
        painter.text(rect.center(), Align2::CENTER_CENTER, label, font, text);
        hub_ui::paint_focus_ring(painter, &response, rect, RADIUS);
    }
    response
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use horizon_core::git_overview::{
        Blocker, CleanupCandidate, CleanupReason, IgnoredFiles, RemoveOutcome, RemoveStatus,
    };

    use super::{CleanupDialog, build_rows, confirm_label, confirmed_ignored, removal_summary, selected};

    fn candidate(path: &str, reason: CleanupReason, blockers: &[Blocker]) -> CleanupCandidate {
        CleanupCandidate {
            path: PathBuf::from(path),
            branch: Some(path.rsplit('/').next().unwrap_or(path).to_owned()),
            reason,
            blockers: blockers.to_vec(),
            ignored: IgnoredFiles::default(),
        }
    }

    fn with_ignored(mut candidate: CleanupCandidate) -> CleanupCandidate {
        candidate.ignored = IgnoredFiles {
            count: 2,
            examples: vec![".env".to_owned(), "node_modules/".to_owned()],
            capped: false,
            unreadable: false,
        };
        candidate
    }

    fn rows() -> Vec<super::CleanupRow> {
        build_rows(
            Path::new("/work/api"),
            vec![
                candidate("/work/api-dirty", CleanupReason::Merged, &[Blocker::Dirty]),
                candidate("/work/api-fix", CleanupReason::Merged, &[]),
                candidate("/work/api-old", CleanupReason::Prunable, &[]),
                candidate("/work/api-pin", CleanupReason::Merged, &[Blocker::Locked]),
            ],
            None,
        )
    }

    #[test]
    fn cleanup_selection_excludes_blocked_rows() {
        let mut rows = rows();
        // Safe rows come first; only the merged one is ticked by default (a prunable
        // record needs a deliberate tick); blocked ones are never ticked.
        assert_eq!(selected(&rows), [PathBuf::from("/work/api-fix")]);
        // A blocked row can never be ticked, whatever the caller asks.
        for row in &mut rows {
            row.set_checked(true);
        }
        assert_eq!(selected(&rows).len(), 2);
        assert!(rows.iter().filter(|row| !row.candidate.safe()).all(|row| !row.checked));
        // Even a forced tick on a blocked row is ignored when selecting.
        rows[3].checked = true;
        assert_eq!(selected(&rows).len(), 2);
    }

    #[test]
    fn rows_with_ignored_files_or_no_commits_are_not_preticked_and_need_two_clicks() {
        let mut rows = build_rows(
            Path::new("/work/api"),
            vec![
                with_ignored(candidate("/work/api-env", CleanupReason::Merged, &[])),
                candidate("/work/api-new", CleanupReason::Fresh, &[]),
                candidate("/work/api-done", CleanupReason::Merged, &[]),
            ],
            None,
        );
        assert_eq!(selected(&rows), [PathBuf::from("/work/api-done")]);
        assert!(confirmed_ignored(&rows).is_empty());

        let env = rows
            .iter_mut()
            .find(|row| row.candidate.path == Path::new("/work/api-env"))
            .expect("row");
        env.set_checked(true);
        assert!(
            !env.checked,
            "ignored data cannot be ticked without the confirmation click"
        );
        env.toggle();
        assert!(env.armed && !env.checked, "first click only arms the row");
        env.toggle();
        assert!(env.checked && !env.armed, "second click confirms");
        assert_eq!(confirmed_ignored(&rows), [PathBuf::from("/work/api-env")]);

        let fresh = rows
            .iter_mut()
            .find(|row| row.candidate.path == Path::new("/work/api-new"))
            .expect("row");
        fresh.toggle();
        assert!(fresh.checked, "a plain row ticks with one click");
        fresh.toggle();
        assert!(!fresh.checked);
    }

    #[test]
    fn deleting_merged_branches_is_off_by_default() {
        let dialog = CleanupDialog::new(String::new(), PathBuf::from("/work/api"), None);
        assert!(!dialog.delete_branches());
    }

    #[test]
    fn unconfirmed_ignored_outcome_unticks_the_row() {
        let mut dialog = CleanupDialog::new(String::new(), PathBuf::from("/work/api"), None);
        dialog.apply_classification(Ok(vec![candidate("/work/api-a", CleanupReason::Merged, &[])]));
        assert_eq!(dialog.selected_paths(), [PathBuf::from("/work/api-a")]);

        let found = IgnoredFiles {
            count: 1,
            examples: vec![".env".to_owned()],
            capped: false,
            unreadable: false,
        };
        let summary = dialog.apply_removal(&[RemoveOutcome {
            path: PathBuf::from("/work/api-a"),
            status: RemoveStatus::IgnoredNotConfirmed(found),
        }]);

        assert_eq!(summary, "Removed 0, kept 1 (has ignored files)");
        assert!(dialog.selected_paths().is_empty());
    }

    #[test]
    fn cleanup_can_be_scoped_to_one_worktree() {
        let scoped = build_rows(
            Path::new("/work/api"),
            vec![
                candidate("/work/api-a", CleanupReason::Merged, &[]),
                candidate("/work/api-b", CleanupReason::Merged, &[]),
            ],
            Some(Path::new("/work/api-b")),
        );
        assert_eq!(selected(&scoped), [PathBuf::from("/work/api-b")]);
    }

    #[test]
    fn cleanup_confirm_label_counts_selected() {
        assert_eq!(confirm_label(0), "Remove worktrees");
        assert_eq!(confirm_label(1), "Remove 1 worktree");
        assert_eq!(confirm_label(2), "Remove 2 worktrees");
    }

    #[test]
    fn removal_summary_reports_removed_kept_and_failed() {
        let outcome = |status| RemoveOutcome {
            path: PathBuf::from("/x"),
            status,
        };
        let outcomes = vec![
            outcome(RemoveStatus::Removed { branch_deleted: true }),
            outcome(RemoveStatus::Removed { branch_deleted: false }),
            outcome(RemoveStatus::Kept(vec![Blocker::Dirty])),
            outcome(RemoveStatus::Failed("boom".to_owned())),
        ];
        assert_eq!(removal_summary(&outcomes), "Removed 2, kept 1 (became dirty), 1 failed");
        assert_eq!(removal_summary(&outcomes[..2]), "Removed 2");
    }
}
