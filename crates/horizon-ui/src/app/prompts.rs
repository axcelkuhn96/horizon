//! Saved prompts in the running app: the cached effective list (global prompts plus the ones of
//! the focused panel's workspace), the `Prompt: <name>` palette rows, and inserting one into a
//! terminal through the same path `link send` uses. Editing lives in `settings/prompts_section`.
//! Visual spec: `docs/design/claude-hub-ui.md` §14.

use std::time::{Duration, Instant};

use egui::{Align2, Context, Id, Margin, Order, Rect, Stroke, Vec2};
use horizon_core::{
    Board, Config, EffectivePrompt, PanelId, PromptScope, SavedPrompt, WorkspaceConfig, effective_prompts,
};

use super::HorizonApp;
use crate::command_palette::PromptEntry;
use crate::theme;

/// How long a refusal stays on screen.
const NOTICE_TTL: Duration = Duration::from_secs(4);
const NOTICE_EDGE: f32 = 12.0;
const NOTICE_WIDTH: f32 = 320.0;
const NOTICE_BAR_WIDTH: f32 = 3.0;
/// Longest slice of a prompt's first line kept as a palette search keyword.
const KEYWORD_CHARS: usize = 120;

/// Why a prompt was not inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// No focused panel with a terminal to type into.
    NoTerminal,
    /// Multi-line text and the terminal did not ask for bracketed paste: the lines would run
    /// one by one.
    MultilineWithoutPaste,
}

impl Refusal {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::NoTerminal => "Focus a terminal first",
            Self::MultilineWithoutPaste => "This terminal can't paste multi-line text",
        }
    }
}

/// Whether `text` may be typed into a terminal whose bracketed-paste state is `bracketed`.
/// Lines are never flattened: multi-line text needs bracketed paste, with or without Enter.
pub(super) fn check_insertable(text: &str, bracketed: bool) -> Result<(), Refusal> {
    if !bracketed && text.contains('\n') {
        return Err(Refusal::MultilineWithoutPaste);
    }
    Ok(())
}

/// The panel a prompt goes to: the focused one, when it has a terminal.
pub(super) fn target_panel(board: &Board) -> Option<PanelId> {
    let id = board.focused?;
    board.panel(id)?.terminal().map(|_| id)
}

/// Prompts of the template named like the workspace (case-insensitively), if there is one.
fn workspace_prompts<'a>(workspaces: &'a [WorkspaceConfig], name: Option<&str>) -> &'a [SavedPrompt] {
    name.and_then(|name| workspaces.iter().find(|w| w.name.eq_ignore_ascii_case(name)))
        .map_or(&[][..], |workspace| workspace.prompts.as_slice())
}

/// First line of `text`, cut to [`KEYWORD_CHARS`]: what the palette also matches on.
fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(KEYWORD_CHARS)
        .collect()
}

fn palette_entries(list: &[EffectivePrompt]) -> Vec<PromptEntry> {
    list.iter()
        .enumerate()
        .map(|(index, prompt)| PromptEntry {
            index,
            label: format!("Prompt: {}", prompt.name),
            detail: prompt.scope.label().to_owned(),
            workspace_scoped: prompt.scope == PromptScope::Workspace,
            keywords: vec![first_line(&prompt.text)],
        })
        .collect()
}

struct Notice {
    text: &'static str,
    until: Instant,
}

/// Prompt state owned by `HubState`.
#[derive(Default)]
pub(super) struct PromptsUi {
    /// The two lists the cache below was built from.
    global: Vec<SavedPrompt>,
    workspace: Vec<SavedPrompt>,
    effective: Vec<EffectivePrompt>,
    /// Palette rows for `effective`, same order.
    pub(super) entries: Vec<PromptEntry>,
    builds: u64,
    notice: Option<Notice>,
}

impl PromptsUi {
    /// Rebuilds the effective list and the palette rows only when one of the two source lists
    /// differs from what they were built from, so a frame without edits allocates nothing.
    pub(super) fn sync(&mut self, global: &[SavedPrompt], workspace: &[SavedPrompt]) {
        if self.global == global && self.workspace == workspace {
            return;
        }
        self.global = global.to_vec();
        self.workspace = workspace.to_vec();
        self.effective = effective_prompts(global, workspace);
        self.entries = palette_entries(&self.effective);
        self.builds += 1;
    }

    /// [`Self::sync`] for the workspace of the focused panel (else the active workspace).
    pub(super) fn refresh(&mut self, config: &Config, board: &Board) {
        let workspace = board
            .focused
            .and_then(|id| board.panel(id))
            .map(|panel| panel.workspace_id)
            .or(board.active_workspace)
            .and_then(|id| board.workspace(id));
        let local = workspace_prompts(&config.workspaces, workspace.map(|w| w.name.as_str()));
        self.sync(&config.prompts, local);
    }

    pub(super) fn notify(&mut self, text: &'static str, now: Instant) {
        self.notice = Some(Notice {
            text,
            until: now + NOTICE_TTL,
        });
    }
}

impl HorizonApp {
    /// Inserts the effective prompt `index` into the focused terminal, without Enter unless
    /// `enter`. Palette entry points call this.
    pub(in crate::app) fn insert_prompt_by_index(&mut self, ctx: &Context, index: usize, enter: bool) {
        let Some(panel) = target_panel(&self.board) else {
            self.hub.prompts.notify(Refusal::NoTerminal.message(), Instant::now());
            return;
        };
        self.hub.prompts.refresh(&self.template_config, &self.board);
        let Some(prompt) = self.hub.prompts.effective.get(index).cloned() else {
            return;
        };
        self.insert_prompt(ctx, panel, &prompt, enter);
    }

    /// Types `prompt` into `panel` as one paste (the link send path). With `enter` false the
    /// user reviews the text first; nothing is ever flattened to a single line.
    pub(in crate::app) fn insert_prompt(
        &mut self,
        ctx: &Context,
        panel: PanelId,
        prompt: &EffectivePrompt,
        enter: bool,
    ) {
        let bracketed = self.board.bracketed_paste_enabled(panel);
        if let Err(refusal) = check_insertable(&prompt.text, bracketed) {
            self.hub.prompts.notify(refusal.message(), Instant::now());
            return;
        }
        match self.board.deliver_text(panel, &prompt.text, enter) {
            Ok(()) => {
                if let Some(body) = crate::terminal_widget::terminal_focus_id(ctx, panel) {
                    ctx.memory_mut(|memory| memory.request_focus(body));
                }
            }
            Err(error) => {
                tracing::debug!(%error, "saved prompt was not delivered");
                self.hub
                    .prompts
                    .notify("Could not write to this terminal", Instant::now());
            }
        }
    }

    /// Paints the refusal notice, bottom-right, until it expires.
    pub(in crate::app) fn render_prompt_notice(&mut self, ctx: &Context) {
        let now = Instant::now();
        let Some(notice) = self.hub.prompts.notice.as_ref() else {
            return;
        };
        if now >= notice.until {
            self.hub.prompts.notice = None;
            return;
        }
        let (text, left) = (notice.text, notice.until - now);
        let shown = egui::Area::new(Id::new("saved_prompt_notice"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-NOTICE_EDGE, -NOTICE_EDGE))
            .order(Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(theme::PANEL_BG_ALT())
                    .stroke(Stroke::new(1.0_f32, theme::alpha(theme::FG_DIM(), 90)))
                    .corner_radius(8)
                    .inner_margin(Margin {
                        left: 14,
                        right: 12,
                        top: 8,
                        bottom: 8,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(NOTICE_WIDTH);
                        ui.label(egui::RichText::new(text).size(12.0).strong().color(theme::FG()));
                    })
                    .response
                    .rect
            });
        let card = shown.inner;
        let bar = Rect::from_min_max(card.min, egui::pos2(card.min.x + NOTICE_BAR_WIDTH, card.max.y));
        ctx.layer_painter(shown.response.layer_id).rect_filled(
            bar,
            egui::CornerRadius::same(2),
            theme::PALETTE_YELLOW(),
        );
        ctx.request_repaint_after(left);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::app::test_support::{command_options, test_app};
    use crate::command_registry::CommandId;

    fn prompt(name: &str, text: &str) -> SavedPrompt {
        SavedPrompt {
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn palette_entries_built_once_per_config_version() {
        let mut ui = PromptsUi::default();
        let global = [prompt("review", "Review this diff."), prompt("tests", "Write tests.")];
        let local = [prompt("deploy", "Ship it.")];

        ui.sync(&global, &local);
        ui.sync(&global, &local);
        ui.sync(&global, &local);

        assert_eq!(ui.builds, 1, "unchanged lists must not rebuild");
        let labels: Vec<_> = ui.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["Prompt: deploy", "Prompt: review", "Prompt: tests"]);
        assert!(ui.entries[0].workspace_scoped && !ui.entries[1].workspace_scoped);
        assert_eq!(ui.entries[0].detail, "workspace");
        assert_eq!(ui.entries[1].detail, "global");

        ui.sync(&global, &[]);
        assert_eq!(ui.builds, 2, "a changed list rebuilds once");
        assert_eq!(ui.entries.len(), 2);
    }

    #[test]
    fn an_empty_config_builds_nothing() {
        let mut ui = PromptsUi::default();

        ui.sync(&[], &[]);

        assert_eq!(ui.builds, 0);
        assert!(ui.entries.is_empty());
    }

    #[test]
    fn multiline_without_bracketed_paste_is_refused() {
        assert_eq!(check_insertable("one\ntwo", false), Err(Refusal::MultilineWithoutPaste));
        assert_eq!(check_insertable("one\ntwo", true), Ok(()));
        assert_eq!(check_insertable("one line", false), Ok(()));
        assert_eq!(
            Refusal::MultilineWithoutPaste.message(),
            "This terminal can't paste multi-line text"
        );
        assert_eq!(Refusal::NoTerminal.message(), "Focus a terminal first");
    }

    #[test]
    fn workspace_prompts_are_found_by_name_ignoring_case() {
        let workspaces = [WorkspaceConfig {
            name: "Alpha".to_owned(),
            color: None,
            cwd: None,
            position: None,
            terminals: Vec::new(),
            sidebar_collapsed: false,
            prompts: vec![prompt("p", "t")],
        }];

        assert_eq!(workspace_prompts(&workspaces, Some("alpha")).len(), 1);
        assert!(workspace_prompts(&workspaces, Some("beta")).is_empty());
        assert!(workspace_prompts(&workspaces, None).is_empty());
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// A command panel that enables bracketed paste, goes raw and appends its input to `file`.
    fn sink_panel(app: &mut HorizonApp, name: &str, file: &Path) -> PanelId {
        let workspace = app.board.ensure_workspace();
        let script = format!(
            "printf '\\033[?2004h'; stty raw -echo; echo ready; exec cat > '{}'",
            file.display()
        );
        let id = app
            .board
            .create_panel(command_options(name, &script), workspace)
            .expect("panel should spawn");
        let ready = wait_until(|| {
            let panel = app.board.panel_mut(id).expect("panel");
            let _ = panel.process_output();
            panel
                .terminal()
                .is_some_and(|terminal| terminal.last_lines_text(24).contains("ready"))
        });
        assert!(ready, "sink panel never became ready");
        id
    }

    fn file_equals(path: &Path, expected: &str) -> bool {
        wait_until(|| std::fs::read(path).is_ok_and(|bytes| bytes == expected.as_bytes()))
    }

    #[test]
    fn insert_defaults_to_no_enter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("in");
        let mut app = test_app();
        app.template_config.prompts = vec![prompt("review", "Review this.\nBe terse.")];
        let id = sink_panel(&mut app, "sink", &file);
        app.board.focus(id);
        let ctx = Context::default();

        app.execute_command(&ctx, &CommandId::InsertPrompt(0));

        assert!(
            file_equals(&file, "\u{1b}[200~Review this.\nBe terse.\u{1b}[201~"),
            "{:?}",
            std::fs::read(&file)
        );
    }

    #[test]
    fn insert_and_send_presses_enter_after_the_paste() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("in");
        let mut app = test_app();
        app.template_config.prompts = vec![prompt("review", "Review this.")];
        let id = sink_panel(&mut app, "sink", &file);
        app.board.focus(id);
        let ctx = Context::default();

        app.execute_command(&ctx, &CommandId::InsertPromptAndSend(0));

        assert!(
            file_equals(&file, "\u{1b}[200~Review this.\u{1b}[201~\r"),
            "{:?}",
            std::fs::read(&file)
        );
    }

    #[test]
    fn target_panel_prefers_focused_terminal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = test_app();
        assert_eq!(target_panel(&app.board), None, "nothing focused");
        let first = sink_panel(&mut app, "first", &dir.path().join("a"));
        let second = sink_panel(&mut app, "second", &dir.path().join("b"));

        app.board.focus(first);
        assert_eq!(target_panel(&app.board), Some(first));
        app.board.focus(second);
        assert_eq!(target_panel(&app.board), Some(second));
    }

    #[test]
    fn inserting_without_a_focused_terminal_raises_the_notice() {
        let mut app = test_app();
        app.template_config.prompts = vec![prompt("review", "Review this.")];
        let ctx = Context::default();

        app.execute_command(&ctx, &CommandId::InsertPrompt(0));

        let notice = app.hub.prompts.notice.as_ref().expect("a notice is shown");
        assert_eq!(notice.text, "Focus a terminal first");
    }
}
