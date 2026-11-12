//! The "New worktree + Claude" form (spec section 11): branch, base, path,
//! account and "also open as a workspace", with inline validation. Hub modals
//! render above egui popups, so every selector here is an inline chip.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Align, Align2, Context, FontId, Id, Key, Layout, Margin, RichText, Sense, Ui, Vec2};
use horizon_core::git_overview::{BranchInfo, CreateRequest, plan_worktree_path, validate_branch_name};
use horizon_core::{PresetConfig, WorkspaceId};

use super::{AccountChoice, body_frame, capitalized, checkbox_row, chip, repo_display_name, short_display_path};
use crate::app::hub_ui::{self, ModalPlacement};
use crate::theme;

const MODAL_WIDTH: f32 = 480.0;
const MAX_BRANCH_CHIPS: usize = 6;
const LABEL_WIDTH: f32 = 56.0;
const FIELD_HEIGHT: f32 = 28.0;
const ERROR_LINE_HEIGHT: f32 = 16.0;
/// Quiet time after the last keystroke before the path is planned and checked
/// (both stat the filesystem, so not on every key).
const SETTLE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Editing,
    /// Waiting for the worker job with this id.
    Working(u64),
}

pub(super) enum NewOutcome {
    Open,
    Submit,
    Close,
}

/// Everything the validation reads.
pub(super) struct FormInput<'a> {
    pub(super) branch: &'a str,
    pub(super) path: &'a str,
    /// `(branch, worktree path)` of every worktree that has a branch checked out.
    pub(super) occupied: &'a [(String, PathBuf)],
    pub(super) repo_root: &'a Path,
}

/// First problem with the form, as the message shown to the user.
pub(super) fn validate_form(input: &FormInput<'_>, exists: &dyn Fn(&Path) -> bool) -> Result<(), String> {
    let branch = input.branch.trim();
    if branch.is_empty() {
        return Err("Enter a branch name".to_owned());
    }
    validate_branch_name(branch).map_err(|error| capitalized(&error.to_string()))?;
    if let Some((_, path)) = input.occupied.iter().find(|(name, _)| name == branch) {
        return Err(format!("{branch} is already checked out in {}", path.display()));
    }
    let text = input.path.trim();
    if text.is_empty() {
        return Err("Enter a destination path".to_owned());
    }
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err("The path must be absolute".to_owned());
    }
    if path.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err("The path cannot contain '..'".to_owned());
    }
    if path.starts_with(input.repo_root.join(".git")) {
        return Err("The path cannot be inside .git".to_owned());
    }
    if exists(path) {
        return Err(format!("{} already exists", path.display()));
    }
    if path.parent().is_none_or(|parent| !exists(parent)) {
        return Err("The parent folder does not exist".to_owned());
    }
    Ok(())
}

pub(super) struct NewWorktreeDialog {
    workspace: WorkspaceId,
    workspace_name: String,
    repo_name: String,
    repo_root: PathBuf,
    workspace_cwd: Option<PathBuf>,
    occupied: Vec<(String, PathBuf)>,
    /// Id of the worker job listing the branches; `0` when none was started.
    pub(super) branches_job: u64,
    branches_loaded: bool,
    branches: Vec<String>,
    default_branch: Option<String>,
    branch: String,
    /// New branch starts from the default branch instead of `HEAD`.
    base_default: bool,
    path_text: String,
    path_custom: bool,
    editing_path: bool,
    accounts: Vec<AccountChoice>,
    account: usize,
    also_workspace: bool,
    also_touched: bool,
    check: Result<(), String>,
    /// Set by every edit; the path is planned and checked once it is quiet.
    settle_at: Option<Instant>,
    checked_for: Option<(String, String)>,
    pub(super) phase: Phase,
    pub(super) error: Option<String>,
}

impl NewWorktreeDialog {
    pub(super) fn new(
        workspace: WorkspaceId,
        workspace_name: String,
        repo_root: PathBuf,
        workspace_cwd: Option<PathBuf>,
        occupied: Vec<(String, PathBuf)>,
        accounts: Vec<AccountChoice>,
        account: usize,
    ) -> Self {
        Self {
            workspace,
            workspace_name,
            repo_name: repo_display_name(&repo_root),
            repo_root,
            workspace_cwd,
            occupied,
            branches_job: 0,
            branches_loaded: false,
            branches: Vec::new(),
            default_branch: None,
            branch: String::new(),
            base_default: false,
            path_text: String::new(),
            path_custom: false,
            editing_path: false,
            accounts,
            account,
            also_workspace: true,
            also_touched: false,
            check: Err("Enter a branch name".to_owned()),
            settle_at: None,
            checked_for: None,
            phase: Phase::Editing,
            error: None,
        }
    }

    pub(super) fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    pub(super) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub(super) fn also_workspace(&self) -> bool {
        self.also_workspace
    }

    /// The preset of the chosen account, if any Claude preset exists.
    pub(super) fn chosen_preset(&self, presets: &[PresetConfig]) -> Option<PresetConfig> {
        let account = self.accounts.get(self.account)?;
        presets.get(account.preset).cloned()
    }

    /// Sets the branch from a click or a hint (one event, so planned at once).
    pub(super) fn set_branch(&mut self, branch: String) {
        self.branch = branch;
        self.on_branch_changed();
        self.settle_now();
    }

    pub(super) fn apply_branches(&mut self, info: BranchInfo) {
        self.branches = info.local;
        self.default_branch = info.default_branch;
        self.branches_loaded = true;
        self.checked_for = None;
    }

    fn on_branch_changed(&mut self) {
        self.error = None;
        self.settle_at = Some(Instant::now());
    }

    /// Plans the path and validates once typing paused, without touching the
    /// filesystem on every keystroke.
    fn settle(&mut self, ctx: &Context) {
        let Some(since) = self.settle_at else {
            return;
        };
        let remaining = SETTLE_DELAY.saturating_sub(since.elapsed());
        if remaining.is_zero() {
            self.settle_now();
        } else {
            ctx.request_repaint_after(remaining);
        }
    }

    fn settle_now(&mut self) {
        self.settle_at = None;
        if !self.path_custom {
            self.plan_path();
        }
        self.refresh_check();
    }

    fn settling(&self) -> bool {
        self.settle_at.is_some()
    }

    fn plan_path(&mut self) {
        let branch = self.branch.trim();
        if branch.is_empty() {
            self.path_text.clear();
        } else {
            self.path_text = plan_worktree_path(&self.repo_root, branch, Path::exists)
                .display()
                .to_string();
        }
        self.sync_also_workspace();
    }

    /// A worktree outside the workspace folder gets its own workspace by default.
    fn sync_also_workspace(&mut self) {
        if !self.also_touched {
            let inside = self
                .workspace_cwd
                .as_deref()
                .is_some_and(|cwd| Path::new(self.path_text.trim()).starts_with(cwd));
            self.also_workspace = !inside;
        }
    }

    fn branch_exists(&self) -> bool {
        let branch = self.branch.trim();
        self.branches.iter().any(|name| name == branch)
    }

    /// Whether the "From" selector applies: a new branch and a known default.
    fn shows_base(&self) -> bool {
        self.branches_loaded
            && self.default_branch.is_some()
            && !self.branch.trim().is_empty()
            && !self.branch_exists()
            && validate_branch_name(self.branch.trim()).is_ok()
    }

    fn refresh_check(&mut self) {
        let unchanged = self
            .checked_for
            .as_ref()
            .is_some_and(|(branch, path)| *branch == self.branch && *path == self.path_text);
        if unchanged {
            return;
        }
        self.check = validate_form(
            &FormInput {
                branch: &self.branch,
                path: &self.path_text,
                occupied: &self.occupied,
                repo_root: &self.repo_root,
            },
            &|path| path.exists(),
        );
        self.checked_for = Some((self.branch.clone(), self.path_text.clone()));
    }

    /// The request for the worker; `None` while the form is invalid or busy.
    pub(super) fn take_request(&mut self) -> Option<CreateRequest> {
        if self.settling() {
            return None;
        }
        self.refresh_check();
        if self.check.is_err() || matches!(self.phase, Phase::Working(_)) {
            return None;
        }
        let base = if self.base_default && !self.branch_exists() {
            self.default_branch.as_deref().map(|name| {
                if self.branches.iter().any(|branch| branch == name) {
                    name.to_owned()
                } else {
                    format!("origin/{name}")
                }
            })
        } else {
            None
        };
        Some(CreateRequest {
            branch: self.branch.trim().to_owned(),
            base,
            path: PathBuf::from(self.path_text.trim()),
        })
    }

    fn create_label(&self) -> &'static str {
        if matches!(self.phase, Phase::Working(_)) {
            "Creating \u{b7}\u{b7}\u{b7}"
        } else if self.accounts.is_empty() {
            "Create worktree"
        } else {
            "Create + open Claude"
        }
    }

    /// Names of existing, free branches matching what is typed.
    fn branch_suggestions(&self) -> Vec<&str> {
        let typed = self.branch.trim().to_lowercase();
        self.branches
            .iter()
            .filter(|name| !self.occupied.iter().any(|(taken, _)| taken == *name))
            .filter(|name| name.to_lowercase().contains(&typed) && **name != self.branch.trim())
            .take(MAX_BRANCH_CHIPS)
            .map(String::as_str)
            .collect()
    }
}

pub(super) fn show(ctx: &Context, dialog: &mut NewWorktreeDialog) -> NewOutcome {
    dialog.settle(ctx);
    if !dialog.settling() {
        dialog.refresh_check();
    }
    let branch_id = Id::new("new_worktree_branch");
    let path_id = Id::new("new_worktree_path");
    let mut submit = false;
    let outcome = hub_ui::show_modal(
        ctx,
        Id::new("new_worktree_modal"),
        MODAL_WIDTH,
        ModalPlacement::TopFraction(0.16),
        Some(branch_id),
        |ui| {
            body_frame().show(ui, |ui| {
                submit = contents(ui, dialog, branch_id, path_id);
            });
        },
    );
    if matches!(dialog.phase, Phase::Working(_)) {
        // The dialog stays until the worker answers; Esc and the scrim do nothing.
        NewOutcome::Open
    } else if outcome.dismissed {
        NewOutcome::Close
    } else if submit {
        NewOutcome::Submit
    } else {
        NewOutcome::Open
    }
}

fn field_row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(LABEL_WIDTH, FIELD_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(11.0),
            theme::FG_DIM(),
        );
        add(ui)
    })
    .inner
}

/// Draws the form; returns `true` when the user asked to create.
fn contents(ui: &mut Ui, dialog: &mut NewWorktreeDialog, branch_id: Id, path_id: Id) -> bool {
    let working = matches!(dialog.phase, Phase::Working(_));
    let mut submit = false;
    ui.spacing_mut().item_spacing = Vec2::new(6.0, 8.0);

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("New worktree + Claude")
                .size(14.0)
                .strong()
                .color(theme::FG()),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let scope = if dialog.workspace_name.is_empty() {
                dialog.repo_name.as_str()
            } else {
                dialog.workspace_name.as_str()
            };
            ui.label(RichText::new(scope).size(10.5).color(theme::FG_DIM()));
        });
    });
    ui.add_space(2.0);

    ui.add_enabled_ui(!working, |ui| {
        submit |= branch_field(ui, dialog, branch_id);
        suggestions(ui, dialog);
        base_selector(ui, dialog);
        path_field(ui, dialog, path_id);
        account_selector(ui, dialog);
        let toggle = checkbox_row(ui, "also open as a workspace", dialog.also_workspace);
        if toggle.clicked() {
            dialog.also_workspace = !dialog.also_workspace;
            dialog.also_touched = true;
        }
    });

    error_line(ui, dialog);
    submit |= footer(ui, dialog);
    submit && dialog.check.is_ok() && !working && !dialog.settling()
}

fn branch_field(ui: &mut Ui, dialog: &mut NewWorktreeDialog, branch_id: Id) -> bool {
    let response = field_row(ui, "Branch", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut dialog.branch)
                .id(branch_id)
                .hint_text("feat/new-thing")
                .font(FontId::proportional(12.0))
                .margin(Margin::symmetric(8, 6))
                .char_limit(200)
                .desired_width(ui.available_width())
                .min_size(Vec2::new(0.0, FIELD_HEIGHT)),
        )
    });
    if response.changed() {
        dialog.on_branch_changed();
    }
    response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter))
}

/// Existing branches matching the text, as clickable chips.
fn suggestions(ui: &mut Ui, dialog: &mut NewWorktreeDialog) {
    let names: Vec<String> = dialog.branch_suggestions().into_iter().map(str::to_owned).collect();
    if names.is_empty() {
        return;
    }
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.add_space(LABEL_WIDTH);
        ui.label(RichText::new("existing").size(10.5).color(theme::FG_DIM()));
        for name in &names {
            if chip(ui, name, false, None).clicked() {
                picked = Some(name.clone());
            }
        }
    });
    if let Some(name) = picked {
        dialog.set_branch(name);
    }
}

fn base_selector(ui: &mut Ui, dialog: &mut NewWorktreeDialog) {
    if !dialog.shows_base() {
        return;
    }
    let default = dialog.default_branch.clone().unwrap_or_default();
    let mut from_default = dialog.base_default;
    field_row(ui, "From", |ui| {
        if chip(ui, "HEAD", !from_default, None)
            .on_hover_text("Start the new branch from the current commit of the main checkout")
            .clicked()
        {
            from_default = false;
        }
        if chip(ui, &default, from_default, None)
            .on_hover_text("Start the new branch from the default branch")
            .clicked()
        {
            from_default = true;
        }
    });
    dialog.base_default = from_default;
}

fn path_field(ui: &mut Ui, dialog: &mut NewWorktreeDialog, path_id: Id) {
    field_row(ui, "Path", |ui| {
        if dialog.editing_path {
            let response = ui.add(
                egui::TextEdit::singleline(&mut dialog.path_text)
                    .id(path_id)
                    .font(FontId::monospace(11.0))
                    .margin(Margin::symmetric(8, 6))
                    .desired_width(ui.available_width())
                    .min_size(Vec2::new(0.0, FIELD_HEIGHT)),
            );
            if response.changed() {
                dialog.path_custom = true;
                dialog.error = None;
                dialog.settle_at = Some(Instant::now());
                dialog.sync_also_workspace();
            }
            if response.lost_focus() {
                dialog.editing_path = false;
            }
        } else {
            let short = short_display_path(&dialog.repo_root, Path::new(dialog.path_text.trim()));
            let max_width = (ui.available_width() - 40.0).max(40.0);
            let galley = hub_ui::elided(ui.painter(), &short, FontId::monospace(10.5), max_width);
            let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
            ui.painter().galley(rect.min, galley, theme::FG_DIM());
            response.on_hover_text(dialog.path_text.as_str());
            let edit =
                ui.add(egui::Label::new(RichText::new("edit").size(11.0).color(theme::ACCENT())).sense(Sense::click()));
            if edit.clicked() {
                dialog.editing_path = true;
                ui.memory_mut(|memory| memory.request_focus(path_id));
            }
            if edit.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    });
}

fn account_selector(ui: &mut Ui, dialog: &mut NewWorktreeDialog) {
    if dialog.accounts.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(LABEL_WIDTH);
            ui.label(
                RichText::new("No Claude preset configured: only the worktree is created.")
                    .size(10.5)
                    .color(theme::FG_DIM()),
            );
        });
        return;
    }
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(LABEL_WIDTH - 6.0, CHIP_ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            rect.left_center(),
            Align2::LEFT_CENTER,
            "Account",
            FontId::proportional(11.0),
            theme::FG_DIM(),
        );
        for (index, account) in dialog.accounts.iter().enumerate() {
            let dot = account.slot.map_or_else(theme::ACCENT, hub_ui::accent_color);
            if chip(ui, &account.label, index == dialog.account, Some(dot)).clicked() {
                picked = Some(index);
            }
        }
    });
    if let Some(index) = picked {
        dialog.account = index;
    }
}

const CHIP_ROW_HEIGHT: f32 = 26.0;

/// Fixed-height line so showing or hiding an error never moves the footer.
fn error_line(ui: &mut Ui, dialog: &NewWorktreeDialog) {
    let message = dialog.error.as_deref().or_else(|| {
        if dialog.branch.trim().is_empty() || dialog.settling() {
            None
        } else {
            dialog.check.as_ref().err().map(String::as_str)
        }
    });
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ERROR_LINE_HEIGHT), Sense::hover());
    let Some(message) = message else {
        return;
    };
    let galley = hub_ui::elided(ui.painter(), message, FontId::proportional(11.0), rect.width());
    ui.painter().galley(rect.min, galley, theme::PALETTE_RED());
    response.on_hover_text(message);
}

/// Footer with the Esc hint and the primary button; `true` when it was clicked.
fn footer(ui: &mut Ui, dialog: &NewWorktreeDialog) -> bool {
    let working = matches!(dialog.phase, Phase::Working(_));
    let enabled = dialog.check.is_ok() && !working && !dialog.settling();
    let mut clicked = false;
    ui.horizontal(|ui| {
        hub_ui::kbd_hint(ui, "Esc", if working { "wait" } else { "close" });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let button = hub_ui::primary_button(ui, dialog.create_label(), enabled);
            if enabled {
                clicked = button.clicked();
            } else if let (false, Err(reason)) = (dialog.settling(), &dialog.check) {
                button.on_hover_text(reason.as_str());
            }
        });
    });
    clicked
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use horizon_core::WorkspaceId;
    use horizon_core::git_overview::BranchInfo;

    use super::{FormInput, NewWorktreeDialog, validate_form};

    fn check(branch: &str, path: &str, existing: &[&str]) -> Result<(), String> {
        let occupied = vec![("taken".to_owned(), PathBuf::from("/work/api-taken"))];
        let existing: Vec<PathBuf> = existing.iter().map(PathBuf::from).collect();
        validate_form(
            &FormInput {
                branch,
                path,
                occupied: &occupied,
                repo_root: Path::new("/work/api"),
            },
            &|candidate| existing.iter().any(|path| path == candidate),
        )
    }

    #[test]
    fn new_worktree_form_validation_messages() {
        let work = &["/work"];
        assert_eq!(check("", "/work/api-x", work), Err("Enter a branch name".to_owned()));
        assert_eq!(
            check("a b", "/work/api-x", work),
            Err("Not a valid git branch name".to_owned())
        );
        assert_eq!(
            check("-x", "/work/api-x", work),
            Err("Branch name cannot start with '-'".to_owned())
        );
        assert_eq!(
            check("taken", "/work/api-taken-2", work),
            Err("taken is already checked out in /work/api-taken".to_owned())
        );
        assert_eq!(check("feat/x", "", work), Err("Enter a destination path".to_owned()));
        assert_eq!(
            check("feat/x", "../api-x", work),
            Err("The path must be absolute".to_owned())
        );
        assert_eq!(
            check("feat/x", "/work/../etc/x", work),
            Err("The path cannot contain '..'".to_owned())
        );
        assert_eq!(
            check("feat/x", "/work/api/.git/wt", &["/work", "/work/api/.git"]),
            Err("The path cannot be inside .git".to_owned())
        );
        assert_eq!(
            check("feat/x", "/work/api-x", &["/work", "/work/api-x"]),
            Err("/work/api-x already exists".to_owned())
        );
        assert_eq!(
            check("feat/x", "/missing/api-x", work),
            Err("The parent folder does not exist".to_owned())
        );
        assert_eq!(check("  feat/x  ", "/work/api-x", work), Ok(()));
    }

    fn dialog() -> NewWorktreeDialog {
        NewWorktreeDialog::new(
            WorkspaceId(1),
            "api".to_owned(),
            PathBuf::from("/work/api"),
            Some(PathBuf::from("/work/api")),
            Vec::new(),
            Vec::new(),
            0,
        )
    }

    #[test]
    fn typing_a_branch_plans_a_sibling_path_until_the_path_is_edited() {
        let mut form = dialog();

        form.set_branch("feat/new-thing".to_owned());
        assert_eq!(form.path_text, "/work/api-feat-new-thing");
        assert!(form.also_workspace, "outside the workspace folder -> own workspace");

        form.path_custom = true;
        form.set_branch("other".to_owned());
        assert_eq!(form.path_text, "/work/api-feat-new-thing", "an edited path is kept");
    }

    #[test]
    fn typing_waits_for_a_pause_before_touching_the_filesystem() {
        let mut form = dialog();
        form.set_branch("feat/a".to_owned());
        assert_eq!(form.path_text, "/work/api-feat-a");

        // A keystroke only marks the form as settling: nothing is planned or checked yet.
        form.branch = "feat/ab".to_owned();
        form.on_branch_changed();
        assert!(form.settling());
        assert_eq!(form.path_text, "/work/api-feat-a", "the plan waits for the pause");
        assert!(form.take_request().is_none(), "no request while settling");

        form.settle_now();
        assert!(!form.settling());
        assert_eq!(form.path_text, "/work/api-feat-ab");
    }

    #[test]
    fn request_uses_the_default_branch_only_for_new_branches() {
        let mut form = dialog();
        form.apply_branches(BranchInfo {
            local: vec!["main".to_owned(), "fix/login".to_owned()],
            default_branch: Some("main".to_owned()),
            current: Some("main".to_owned()),
        });
        form.base_default = true;
        form.path_custom = true;
        // The directory does not exist, so only the parent check can fail: point at a real parent.
        form.path_text = std::env::temp_dir()
            .join("horizon-wt-form-test-x")
            .display()
            .to_string();

        form.branch = "brand/new".to_owned();
        let request = form.take_request().expect("valid request");
        assert_eq!(request.base.as_deref(), Some("main"));

        form.branch = "fix/login".to_owned();
        let request = form.take_request().expect("valid request");
        assert_eq!(request.base, None, "an existing branch ignores the base");
    }
}
