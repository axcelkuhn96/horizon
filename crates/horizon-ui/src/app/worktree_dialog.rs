//! "New worktree + Claude" and "Clean up worktrees" dialogs.
//!
//! Visual spec: `docs/design/claude-hub-ui.md` section 11. All git work runs on
//! short-lived worker threads (`horizon_core::git_overview` worktree ops);
//! their results come back over a channel drained once per frame with a cap.
//! Nothing here touches git on the UI thread.

mod cleanup_list;
mod new_form;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{
    Align2, Area, Color32, Context, CornerRadius, FontId, Frame, Id, Margin, Order, Pos2, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType,
};
use horizon_core::git_overview::{
    BranchInfo, CleanupCandidate, Created, RemoveOutcome, WorktreeError, classify_cleanup, create_worktree,
    list_branches, remove_worktrees,
};
use horizon_core::{PanelKind, PresetConfig, Terminal, WorkspaceId};

use super::HorizonApp;
use super::claude_auto::{AUTO_NAME, MIN_PROFILES_FOR_AUTO};
use super::claude_launcher::{claude_profile_names, find_profile_preset};
use super::hub_ui;
use crate::theme;
use cleanup_list::{CleanupDialog, CleanupOutcome, CleanupPhase};
use new_form::{NewOutcome, NewWorktreeDialog, Phase};

/// Worker results applied per frame.
const MAX_EVENTS_PER_FRAME: usize = 8;
const NOTICE_TTL: Duration = Duration::from_secs(4);
const CHIP_HEIGHT: f32 = 26.0;
/// Prefix of the launcher presets derived from Claude profiles.
const CLAUDE_PROFILE_PREFIX: &str = "Claude \u{b7} ";

/// Result of one worker job; `job` ties it to the dialog that asked.
enum WorktreeEvent {
    Branches {
        job: u64,
        result: Result<BranchInfo, String>,
    },
    Created {
        job: u64,
        result: Result<Created, WorktreeError>,
    },
    Classified {
        job: u64,
        result: Result<Vec<CleanupCandidate>, String>,
    },
    Removed {
        job: u64,
        outcomes: Vec<RemoveOutcome>,
    },
    /// The worker panicked; whatever it was doing did not finish.
    Failed {
        job: u64,
        message: String,
    },
}

struct Notice {
    text: String,
    error: bool,
    until: Instant,
}

/// State of the worktree dialogs, owned by `HubState`.
#[derive(Default)]
pub(in crate::app) struct WorktreeUi {
    new: Option<NewWorktreeDialog>,
    cleanup: Option<CleanupDialog>,
    channel: Option<(Sender<WorktreeEvent>, Receiver<WorktreeEvent>)>,
    next_job: u64,
    notice: Option<Notice>,
}

impl WorktreeUi {
    /// Runs `work` on a worker thread and wakes the UI when it is done.
    /// Returns the job id its event will carry, or `None` when no thread could start.
    fn spawn_job(&mut self, ctx: &Context, work: impl FnOnce(u64) -> WorktreeEvent + Send + 'static) -> Option<u64> {
        self.next_job += 1;
        let job = self.next_job;
        let (sender, _) = self.channel.get_or_insert_with(mpsc::channel);
        let sender = sender.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("horizon-worktree".to_owned())
            .spawn(move || {
                // A panic still answers, so a dialog never waits for a result that cannot come.
                let event =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(job))).unwrap_or_else(|payload| {
                        WorktreeEvent::Failed {
                            job,
                            message: panic_message(payload.as_ref()),
                        }
                    });
                // A closed channel means the app is shutting down.
                let _ = sender.send(event);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => Some(job),
            Err(error) => {
                tracing::error!("failed to start the worktree worker: {error}");
                None
            }
        }
    }

    /// A job whose result a dialog is waiting for is in flight.
    fn busy(&self) -> bool {
        self.new
            .as_ref()
            .is_some_and(|dialog| matches!(dialog.phase, Phase::Working(_)))
            || self
                .cleanup
                .as_ref()
                .is_some_and(|dialog| matches!(dialog.phase, CleanupPhase::Removing(_)))
    }

    fn set_notice(&mut self, text: String, error: bool) {
        self.notice = Some(Notice {
            text,
            error,
            until: Instant::now() + NOTICE_TTL,
        });
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    match detail {
        Some(detail) => format!("The background task crashed: {detail}"),
        None => "The background task crashed.".to_owned(),
    }
}

/// Up to `cap` queued items, oldest first; the rest stay queued.
fn drain_capped<T>(receiver: &Receiver<T>, cap: usize) -> Vec<T> {
    std::iter::from_fn(|| receiver.try_recv().ok()).take(cap).collect()
}

/// One account the new worktree's Claude session can run under.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountChoice {
    /// Index into the preset list.
    preset: usize,
    label: String,
    /// Accent slot of the profile; `None` for a plain Claude preset.
    slot: Option<usize>,
    /// The "Auto" entry: the account is picked from the usage at create time, `preset` is unused.
    auto: bool,
}

/// Prepends the Auto choice when `profile_count` profiles make it worth offering.
fn with_auto_choice(mut accounts: Vec<AccountChoice>, profile_count: usize) -> Vec<AccountChoice> {
    if profile_count >= MIN_PROFILES_FOR_AUTO && !accounts.is_empty() {
        accounts.insert(
            0,
            AccountChoice {
                preset: 0,
                label: AUTO_NAME.to_owned(),
                slot: None,
                auto: true,
            },
        );
    }
    accounts
}

/// Derived `Claude · <profile>` presets, or the plain Claude presets when none exist.
fn account_choices(presets: &[PresetConfig]) -> Vec<AccountChoice> {
    let profiles: Vec<(usize, &str)> = presets
        .iter()
        .enumerate()
        .filter(|(_, preset)| preset.kind == PanelKind::Claude)
        .filter_map(|(index, preset)| {
            let alias = preset.alias.as_deref()?;
            (preset.name.strip_prefix(CLAUDE_PROFILE_PREFIX) == Some(alias)).then_some((index, alias))
        })
        .collect();
    if profiles.is_empty() {
        return presets
            .iter()
            .enumerate()
            .filter(|(_, preset)| preset.kind == PanelKind::Claude)
            .map(|(preset, config)| AccountChoice {
                preset,
                label: config.name.clone(),
                slot: None,
                auto: false,
            })
            .collect();
    }
    let names: Vec<&str> = profiles.iter().map(|(_, alias)| *alias).collect();
    let slots = hub_ui::assign_accent_indices(&names);
    profiles
        .iter()
        .zip(slots)
        .map(|((preset, alias), slot)| AccountChoice {
            preset: *preset,
            label: (*alias).to_owned(),
            slot: Some(slot),
            auto: false,
        })
        .collect()
}

/// `../name` for a sibling of the repository, the absolute path otherwise.
fn short_display_path(repo_root: &Path, path: &Path) -> String {
    if let Some(parent) = repo_root.parent()
        && let Ok(rest) = path.strip_prefix(parent)
    {
        return format!("../{}", rest.display());
    }
    path.display().to_string()
}

fn repo_display_name(repo_root: &Path) -> String {
    repo_root.file_name().map_or_else(
        || repo_root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

// ---------------------------------------------------------------------------
// Entry points and event handling
// ---------------------------------------------------------------------------

impl HorizonApp {
    /// Directories in use: workspace and terminal working directories. A worktree
    /// containing one is never removed.
    fn worktree_protected_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self
            .board
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.cwd.clone())
            .collect();
        paths.extend(self.board.panels.iter().filter_map(|panel| panel.launch_cwd.clone()));
        // A shell that `cd`-ed into a worktree is using it, whatever it was launched with.
        paths.extend(
            self.board
                .panels
                .iter()
                .filter_map(|panel| panel.terminal().and_then(Terminal::current_cwd)),
        );
        paths.sort();
        paths.dedup();
        paths
    }

    /// Opens "New worktree + Claude" for the repository of `workspace`.
    pub(in crate::app) fn open_new_worktree(
        &mut self,
        ctx: &Context,
        workspace: WorkspaceId,
        branch_hint: Option<String>,
    ) {
        if self.hub.worktree_ui.busy() {
            self.hub
                .worktree_ui
                .set_notice("Wait for the current worktree operation to finish.".to_owned(), true);
            return;
        }
        let Some(overview) = self.hub.git.overview_for(workspace) else {
            return;
        };
        let repo_root = overview.repo_root.clone();
        let occupied: Vec<(String, PathBuf)> = overview
            .worktrees
            .iter()
            .filter_map(|worktree| Some((worktree.branch.clone()?, worktree.path.clone())))
            .collect();
        let (workspace_name, workspace_cwd) = self
            .board
            .workspace(workspace)
            .map(|found| (found.name.clone(), found.cwd.clone()))
            .unwrap_or_default();
        let accounts = with_auto_choice(
            account_choices(&self.presets),
            claude_profile_names(&self.presets).len(),
        );
        // Without a remembered account the first real profile is preselected, not Auto.
        let account = self
            .hub
            .last_launched_profile
            .as_deref()
            .and_then(|last| accounts.iter().position(|choice| !choice.auto && choice.label == last))
            .unwrap_or_else(|| usize::from(accounts.first().is_some_and(|choice| choice.auto)));
        let mut dialog = NewWorktreeDialog::new(
            workspace,
            workspace_name,
            repo_root.clone(),
            workspace_cwd,
            occupied,
            accounts,
            account,
        );
        if let Some(hint) = branch_hint {
            dialog.set_branch(hint);
        }
        let ui = &mut self.hub.worktree_ui;
        let job = ui.spawn_job(ctx, move |job| WorktreeEvent::Branches {
            job,
            result: list_branches(&repo_root).map_err(|error| error.to_string()),
        });
        dialog.branches_job = job.unwrap_or(0);
        ui.cleanup = None;
        ui.new = Some(dialog);
        ctx.request_repaint();
    }

    /// Opens "Clean up worktrees" for the repository of `workspace`; with `only`,
    /// scoped to that one worktree ("Remove worktree...").
    pub(in crate::app) fn open_worktree_cleanup(
        &mut self,
        ctx: &Context,
        workspace: WorkspaceId,
        only: Option<PathBuf>,
    ) {
        if self.hub.worktree_ui.busy() {
            self.hub
                .worktree_ui
                .set_notice("Wait for the current worktree operation to finish.".to_owned(), true);
            return;
        }
        let Some(overview) = self.hub.git.overview_for(workspace) else {
            return;
        };
        let repo_root = overview.repo_root.clone();
        let workspace_name = self
            .board
            .workspace(workspace)
            .map(|found| found.name.clone())
            .unwrap_or_default();
        let protected = self.worktree_protected_paths();
        let mut dialog = CleanupDialog::new(workspace_name, repo_root.clone(), only);
        let ui = &mut self.hub.worktree_ui;
        match ui.spawn_job(ctx, move |job| WorktreeEvent::Classified {
            job,
            result: classify_cleanup(&repo_root, &protected).map_err(|error| error.to_string()),
        }) {
            Some(job) => dialog.phase = CleanupPhase::Loading(job),
            None => dialog.error = Some("Could not start the background worker.".to_owned()),
        }
        ui.new = None;
        ui.cleanup = Some(dialog);
        ctx.request_repaint();
    }

    /// Applies worker results (capped per frame).
    pub(in crate::app) fn poll_worktree_events(&mut self, ctx: &Context) {
        let Some((_, receiver)) = &self.hub.worktree_ui.channel else {
            return;
        };
        let events = drain_capped(receiver, MAX_EVENTS_PER_FRAME);
        let capped = events.len() == MAX_EVENTS_PER_FRAME;
        for event in events {
            self.apply_worktree_event(ctx, event);
        }
        if capped {
            ctx.request_repaint();
        }
    }

    fn apply_worktree_event(&mut self, ctx: &Context, event: WorktreeEvent) {
        match event {
            WorktreeEvent::Branches { job, result } => {
                if let Some(dialog) = self.hub.worktree_ui.new.as_mut().filter(|d| d.branches_job == job) {
                    match result {
                        Ok(info) => dialog.apply_branches(info),
                        Err(message) => tracing::warn!("could not list branches: {message}"),
                    }
                }
            }
            WorktreeEvent::Created { job, result } => self.finish_create(ctx, job, result),
            WorktreeEvent::Classified { job, result } => {
                if let Some(dialog) = self.hub.worktree_ui.cleanup.as_mut()
                    && dialog.phase == CleanupPhase::Loading(job)
                {
                    dialog.apply_classification(result);
                }
            }
            WorktreeEvent::Removed { job, outcomes } => self.finish_removal(job, &outcomes),
            WorktreeEvent::Failed { job, message } => self.fail_job(ctx, job, &message),
        }
    }

    /// A worker panicked: put the dialog waiting for `job` back in a usable state.
    fn fail_job(&mut self, ctx: &Context, job: u64, message: &str) {
        let ui = &mut self.hub.worktree_ui;
        if let Some(dialog) = ui.new.as_mut()
            && dialog.phase == Phase::Working(job)
        {
            // The worktree may or may not exist: a refresh shows the truth.
            dialog.phase = Phase::Editing;
            dialog.error = Some(message.to_owned());
            self.hub.git.refresh_now();
            return;
        }
        let mut reload = None;
        if let Some(dialog) = ui.cleanup.as_mut() {
            match dialog.phase {
                CleanupPhase::Loading(waiting) if waiting == job => {
                    dialog.phase = CleanupPhase::Ready;
                    dialog.error = Some(message.to_owned());
                }
                CleanupPhase::Removing(waiting) if waiting == job => {
                    // Some worktrees may already be gone: scan again.
                    dialog.error = Some(format!("{message} Check the list below."));
                    reload = Some(dialog.repo_root().to_path_buf());
                }
                _ => {}
            }
        }
        if let Some(repo_root) = reload {
            self.reload_cleanup(ctx, repo_root);
        }
        self.hub.git.refresh_now();
    }

    fn reload_cleanup(&mut self, ctx: &Context, repo_root: PathBuf) {
        let protected = self.worktree_protected_paths();
        let ui = &mut self.hub.worktree_ui;
        let job = ui.spawn_job(ctx, move |job| WorktreeEvent::Classified {
            job,
            result: classify_cleanup(&repo_root, &protected).map_err(|error| error.to_string()),
        });
        if let Some(dialog) = ui.cleanup.as_mut() {
            dialog.phase = job.map_or(CleanupPhase::Ready, CleanupPhase::Loading);
        }
    }

    fn submit_new_worktree(&mut self, ctx: &Context) {
        let Some(dialog) = self.hub.worktree_ui.new.as_mut() else {
            return;
        };
        let Some(request) = dialog.take_request() else {
            return;
        };
        let repo_root = dialog.repo_root().to_path_buf();
        let ui = &mut self.hub.worktree_ui;
        let job = ui.spawn_job(ctx, move |job| WorktreeEvent::Created {
            job,
            result: create_worktree(&repo_root, &request),
        });
        if let Some(dialog) = ui.new.as_mut() {
            match job {
                Some(job) => dialog.phase = Phase::Working(job),
                None => dialog.error = Some("Could not start the background worker.".to_owned()),
            }
        }
    }

    fn finish_create(&mut self, ctx: &Context, job: u64, result: Result<Created, WorktreeError>) {
        let matches_dialog = self
            .hub
            .worktree_ui
            .new
            .as_ref()
            .is_some_and(|dialog| dialog.phase == Phase::Working(job));
        if !matches_dialog {
            return;
        }
        match result {
            Err(error) => {
                if let Some(dialog) = self.hub.worktree_ui.new.as_mut() {
                    dialog.phase = Phase::Editing;
                    dialog.error = Some(error.to_string());
                }
            }
            Ok(created) => {
                let Some(dialog) = self.hub.worktree_ui.new.take() else {
                    return;
                };
                self.hub.git.refresh_now();
                let (message, failed) = self.open_claude_in_new_worktree(ctx, &dialog, &created);
                self.hub.worktree_ui.set_notice(message, failed);
            }
        }
    }

    /// Opens the Claude session of the account picked in `dialog` in the new
    /// worktree, in a new workspace when asked. Returns the notice text and
    /// whether it reports a problem.
    fn open_claude_in_new_worktree(
        &mut self,
        ctx: &Context,
        dialog: &NewWorktreeDialog,
        created: &Created,
    ) -> (String, bool) {
        let preset = if dialog.auto_chosen() {
            // Auto is resolved now, from the usage as it is at this moment.
            let picked = self
                .current_auto_pick()
                .and_then(|pick| find_profile_preset(&self.presets, &pick.name).cloned());
            let Some(preset) = picked else {
                return (
                    format!(
                        "Created worktree {}, but no Claude account is available for Auto",
                        created.branch
                    ),
                    true,
                );
            };
            preset
        } else if let Some(preset) = dialog.chosen_preset(&self.presets) {
            preset
        } else {
            return (
                format!("Created worktree {} (no Claude preset configured)", created.branch),
                false,
            );
        };
        let mut options = preset.to_panel_options();
        options.cwd = Some(created.path.clone());
        let workspace = if dialog.also_workspace() {
            let workspace = self.create_workspace_visible(ctx, &created.branch);
            if let Some(found) = self.board.workspace_mut(workspace) {
                found.cwd = Some(created.path.clone());
            }
            Some(workspace)
        } else if self.board.workspace(dialog.workspace()).is_some() {
            Some(dialog.workspace())
        } else {
            self.board.active_workspace
        };
        let Some(workspace) = workspace else {
            return (format!("Created worktree {}", created.branch), false);
        };
        let opened = self.open_panel_in_worktree(workspace, options);
        if dialog.also_workspace() {
            self.board.focus_workspace(workspace);
            self.fit_workspace_visible(ctx, workspace);
        }
        if !opened {
            return (
                format!("Created worktree {}, but Claude could not be started", created.branch),
                true,
            );
        }
        if let Some(alias) = &preset.alias {
            self.hub.last_launched_profile = Some(alias.clone());
        }
        (format!("Created worktree {} and opened Claude", created.branch), false)
    }

    fn submit_cleanup(&mut self, ctx: &Context) {
        let protected = self.worktree_protected_paths();
        let Some(dialog) = self.hub.worktree_ui.cleanup.as_mut() else {
            return;
        };
        let items = dialog.selected_paths();
        if items.is_empty() {
            return;
        }
        let repo_root = dialog.repo_root().to_path_buf();
        let delete_branches = dialog.delete_branches();
        let confirmed_ignored = dialog.confirmed_ignored_paths();
        let ui = &mut self.hub.worktree_ui;
        let job = ui.spawn_job(ctx, move |job| WorktreeEvent::Removed {
            job,
            outcomes: remove_worktrees(&repo_root, &items, delete_branches, &protected, &confirmed_ignored),
        });
        if let Some(dialog) = ui.cleanup.as_mut() {
            match job {
                Some(job) => dialog.phase = CleanupPhase::Removing(job),
                None => dialog.error = Some("Could not start the background worker.".to_owned()),
            }
        }
    }

    fn finish_removal(&mut self, job: u64, outcomes: &[RemoveOutcome]) {
        let Some(dialog) = self.hub.worktree_ui.cleanup.as_mut() else {
            return;
        };
        if dialog.phase != CleanupPhase::Removing(job) {
            return;
        }
        let summary = dialog.apply_removal(outcomes);
        let failed = outcomes
            .iter()
            .any(|outcome| matches!(outcome.status, horizon_core::git_overview::RemoveStatus::Failed(_)));
        self.hub.git.refresh_now();
        self.hub.worktree_ui.set_notice(summary, failed);
    }

    /// Draws the open dialogs and the notice; called once per frame, last.
    pub(in crate::app) fn render_worktree_dialogs(&mut self, ctx: &Context) {
        if let Some(mut dialog) = self.hub.worktree_ui.new.take() {
            match new_form::show(ctx, &mut dialog) {
                NewOutcome::Open => self.hub.worktree_ui.new = Some(dialog),
                NewOutcome::Submit => {
                    self.hub.worktree_ui.new = Some(dialog);
                    self.submit_new_worktree(ctx);
                }
                NewOutcome::Close => {}
            }
        }
        if let Some(mut dialog) = self.hub.worktree_ui.cleanup.take() {
            match cleanup_list::show(ctx, &mut dialog) {
                CleanupOutcome::Open => self.hub.worktree_ui.cleanup = Some(dialog),
                CleanupOutcome::Submit => {
                    self.hub.worktree_ui.cleanup = Some(dialog);
                    self.submit_cleanup(ctx);
                }
                CleanupOutcome::Close => {}
            }
        }
        render_notice(ctx, &mut self.hub.worktree_ui.notice);
    }
}

fn render_notice(ctx: &Context, slot: &mut Option<Notice>) {
    let Some(notice) = slot else {
        return;
    };
    let Some(remaining) = notice
        .until
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
    else {
        *slot = None;
        return;
    };
    ctx.request_repaint_after(remaining);
    let color = if notice.error {
        theme::PALETTE_YELLOW()
    } else {
        theme::PALETTE_GREEN()
    };
    let max_width = (ctx.content_rect().width() - 48.0).clamp(1.0, 520.0);
    Area::new(Id::new("worktree_notice"))
        .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
        .order(Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_max_width(max_width);
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&notice.text).size(12.0).color(color)).wrap());
            });
        });
}

// ---------------------------------------------------------------------------
// Shared widgets
// ---------------------------------------------------------------------------

/// Selectable chip (optional leading dot). Used for branches, base and accounts.
fn chip(ui: &mut Ui, label: &str, selected: bool, dot: Option<Color32>) -> egui::Response {
    let font = FontId::proportional(11.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let lead = if dot.is_some() { 14.0 } else { 0.0 };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(text_width + 20.0 + lead, CHIP_HEIGHT), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    if ui.is_rect_visible(rect) {
        let (fill, stroke) = match (selected, response.hovered()) {
            (true, _) => (
                theme::blend(theme::PANEL_BG_ALT(), theme::ACCENT(), 0.22),
                Stroke::new(1.0_f32, theme::alpha(theme::ACCENT(), 170)),
            ),
            (false, true) => (theme::PANEL_BG_ALT(), Stroke::new(1.0_f32, theme::BORDER_STRONG())),
            (false, false) => (theme::PANEL_BG_ALT(), Stroke::new(1.0_f32, theme::BORDER_SUBTLE())),
        };
        let painter = ui.painter();
        painter.rect(rect, CornerRadius::same(7), fill, stroke, StrokeKind::Inside);
        let mut x = rect.left() + 10.0;
        if let Some(color) = dot {
            painter.circle_filled(Pos2::new(x + 3.0, rect.center().y), 3.5, color);
            x += lead;
        }
        painter.text(
            Pos2::new(x, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            if selected { theme::FG() } else { theme::FG_SOFT() },
        );
        hub_ui::paint_focus_ring(painter, &response, rect, 7);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// 14x14 checkbox painted at `rect`; `enabled == false` dims it.
fn paint_checkbox(painter: &egui::Painter, rect: Rect, checked: bool, enabled: bool) {
    let radius = CornerRadius::same(4);
    if checked {
        let fill = if enabled {
            theme::ACCENT()
        } else {
            theme::alpha(theme::ACCENT(), 90)
        };
        painter.rect_filled(rect, radius, fill);
        let c = rect.center();
        let stroke = Stroke::new(1.6_f32, theme::BG());
        painter.line_segment([c + Vec2::new(-3.5, 0.5), c + Vec2::new(-1.0, 3.0)], stroke);
        painter.line_segment([c + Vec2::new(-1.0, 3.0), c + Vec2::new(3.8, -2.8)], stroke);
    } else {
        let color = if enabled {
            theme::BORDER_STRONG()
        } else {
            theme::alpha(theme::BORDER_STRONG(), 110)
        };
        painter.rect_stroke(rect, radius, Stroke::new(1.2_f32, color), StrokeKind::Inside);
    }
}

/// Labelled checkbox row; returns the response (click toggles in the caller).
fn checkbox_row(ui: &mut Ui, label: &str, checked: bool) -> egui::Response {
    let font = FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(text_width + 14.0 + 8.0 + 4.0, 24.0), Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, checked, label));
    if ui.is_rect_visible(rect) {
        let boxed = Rect::from_center_size(Pos2::new(rect.left() + 7.0, rect.center().y), Vec2::splat(14.0));
        let painter = ui.painter();
        paint_checkbox(painter, boxed, checked, true);
        painter.text(
            Pos2::new(rect.left() + 22.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            theme::FG_SOFT(),
        );
        hub_ui::paint_focus_ring(painter, &response, rect.expand(2.0), 4);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Modal body padding.
fn body_frame() -> Frame {
    Frame::new().inner_margin(Margin::same(16))
}

/// First letter upper-cased, for error messages that start lowercase.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    use horizon_core::{PanelKind, PanelResume, PresetConfig};

    use super::{AccountChoice, account_choices, capitalized, drain_capped, short_display_path, with_auto_choice};

    fn preset(name: &str, alias: Option<&str>, kind: PanelKind) -> PresetConfig {
        PresetConfig {
            name: name.to_owned(),
            alias: alias.map(str::to_owned),
            kind,
            command: None,
            args: Vec::new(),
            resume: PanelResume::Fresh,
            ssh_connection: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn worktree_result_drain_respects_cap() {
        let (sender, receiver) = mpsc::channel();
        for value in 0..12 {
            sender.send(value).expect("send");
        }

        assert_eq!(drain_capped(&receiver, 8), (0..8).collect::<Vec<_>>());
        assert_eq!(drain_capped(&receiver, 8), vec![8, 9, 10, 11]);
        assert!(drain_capped(&receiver, 8).is_empty());
    }

    #[test]
    fn accounts_prefer_derived_profiles_then_plain_claude_presets() {
        let presets = vec![
            preset("Shell", None, PanelKind::Shell),
            preset("Claude Code", None, PanelKind::Claude),
            preset("Claude \u{b7} conta1", Some("conta1"), PanelKind::Claude),
            preset("Claude \u{b7} conta2", Some("conta2"), PanelKind::Claude),
        ];
        let accounts = account_choices(&presets);
        assert_eq!(accounts.iter().map(|a| a.preset).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(accounts[0].label, "conta1");
        assert!(accounts.iter().all(|a| a.slot.is_some()));

        let plain = account_choices(&presets[..2]);
        assert_eq!(
            plain,
            [AccountChoice {
                preset: 1,
                label: "Claude Code".to_owned(),
                slot: None,
                auto: false
            }]
        );
        assert!(account_choices(&presets[..1]).is_empty());
    }

    #[test]
    fn auto_is_offered_first_from_two_profiles_and_never_without_accounts() {
        let presets = vec![
            preset("Claude \u{b7} conta1", Some("conta1"), PanelKind::Claude),
            preset("Claude \u{b7} conta2", Some("conta2"), PanelKind::Claude),
        ];

        let two = with_auto_choice(account_choices(&presets), 2);
        assert_eq!(
            two.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(),
            ["Auto", "conta1", "conta2"]
        );
        assert!(two[0].auto && two[1..].iter().all(|a| !a.auto));

        let one = with_auto_choice(account_choices(&presets[..1]), 1);
        assert_eq!(one.len(), 1);
        assert!(with_auto_choice(Vec::new(), 2).is_empty());
    }

    #[test]
    fn display_path_is_relative_to_the_repository_parent() {
        let repo = Path::new("/work/api");
        assert_eq!(short_display_path(repo, Path::new("/work/api-fix")), "../api-fix");
        assert_eq!(short_display_path(repo, Path::new("/elsewhere/x")), "/elsewhere/x");
        assert_eq!(PathBuf::from("/a").display().to_string(), "/a");
    }

    #[test]
    fn capitalized_handles_empty_and_ascii() {
        assert_eq!(capitalized(""), "");
        assert_eq!(capitalized("branch name is empty"), "Branch name is empty");
    }
}
