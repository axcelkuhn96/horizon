//! Saved-prompts editor shared by Settings -> Claude (global prompts) and the workspace editor
//! in Settings -> Workspaces (per-workspace prompts). One card per prompt: name, delete button,
//! monospace text, byte counter and inline errors; no combo boxes, no modals.
//! Visual spec: `docs/design/claude-hub-ui.md` §14.

use std::hash::Hash;

use egui::{Align, FontId, Frame, Id, Layout, Margin, RichText, ScrollArea, Stroke, StrokeKind, TextEdit, Ui, Vec2};
use horizon_core::{MAX_PROMPT_TEXT_BYTES, PromptIssueKind, SavedPrompt, validate_prompts};

use crate::app::hub_ui::{self, IconKind};
use crate::theme;

const NAME_WIDTH: f32 = 160.0;
const FIELD_HEIGHT: f32 = 28.0;
const TEXT_FONT_SIZE: f32 = 11.5;
const TEXT_MIN_ROWS: usize = 3;
const TEXT_MAX_ROWS: usize = 8;
/// Pixel height of one monospace text row, with a little slack for the edit margin.
const TEXT_ROW_HEIGHT: f32 = 16.0;
const DEFAULT_NAME: &str = "New prompt";
/// How the saved prompts are used; shown above the list.
const HELP: &str = "Run \"Prompt: <name>\" from the command palette. Enter inserts the text into the focused \
terminal; Ctrl+Enter inserts it and presses Enter.";

/// What the editor shows under one prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RowMarks {
    pub(super) name: Option<PromptIssueKind>,
    pub(super) text: Option<PromptIssueKind>,
}

/// One entry per prompt: the first problem with its name and the first with its text.
pub(super) fn row_marks(prompts: &[SavedPrompt]) -> Vec<RowMarks> {
    let mut marks = vec![RowMarks::default(); prompts.len()];
    for issue in validate_prompts(prompts) {
        let Some(row) = marks.get_mut(issue.index) else {
            continue;
        };
        let slot = if issue.kind.is_name_issue() {
            &mut row.name
        } else {
            &mut row.text
        };
        slot.get_or_insert(issue.kind);
    }
    marks
}

/// Appends an empty prompt with a free default name; returns its index.
pub(super) fn add_prompt(prompts: &mut Vec<SavedPrompt>) -> usize {
    let taken = |name: &str| prompts.iter().any(|p| p.name.trim().eq_ignore_ascii_case(name));
    let mut name = DEFAULT_NAME.to_owned();
    let mut n = 2;
    while taken(&name) {
        name = format!("{DEFAULT_NAME} {n}");
        n += 1;
    }
    prompts.push(SavedPrompt {
        name,
        text: String::new(),
    });
    prompts.len() - 1
}

/// Removes prompt `index`; `false` when there is none.
pub(super) fn remove_prompt(prompts: &mut Vec<SavedPrompt>, index: usize) -> bool {
    if index >= prompts.len() {
        return false;
    }
    prompts.remove(index);
    true
}

/// `\r\n` and lone `\r` as `\n`: pasted Windows text must not trip the control-character rule.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Rows the text box shows for `text`: its line count, within the allowed range.
fn text_rows(text: &str) -> usize {
    text.lines().count().clamp(TEXT_MIN_ROWS, TEXT_MAX_ROWS)
}

/// Whether the byte counter must turn red.
const fn counter_over(bytes: usize) -> bool {
    bytes > MAX_PROMPT_TEXT_BYTES
}

fn error_label(ui: &mut Ui, message: &str) {
    ui.label(RichText::new(message).size(11.0).color(theme::PALETTE_RED()));
}

/// Inline message for `kind`: red for a real error, dim for an unfinished draft that is merely
/// skipped until it is filled in.
fn issue_label(ui: &mut Ui, kind: PromptIssueKind) {
    if kind.is_hard() {
        error_label(ui, kind.message());
    } else {
        ui.label(
            RichText::new(format!("{} \u{2014} skipped until filled in", kind.message()))
                .size(11.0)
                .color(theme::FG_DIM()),
        );
    }
}

fn outline(ui: &Ui, response: &egui::Response) {
    ui.painter().rect_stroke(
        response.rect,
        egui::CornerRadius::same(2),
        Stroke::new(1.0_f32, theme::PALETTE_RED()),
        StrokeKind::Inside,
    );
}

/// Draws the editor for `prompts` and returns `true` when anything changed. `id_salt` must be
/// unique per list on screen (global vs. each workspace).
pub(super) fn show(ui: &mut Ui, id_salt: impl Hash, prompts: &mut Vec<SavedPrompt>) -> bool {
    let base = Id::new(("saved_prompts", id_salt));
    let focus_key = base.with("focus_new");
    let focus_new: Option<usize> = ui.data_mut(|data| data.remove_temp(focus_key));
    let marks = row_marks(prompts);
    let mut changed = false;
    let mut remove = None;

    super::dim_label(ui, HELP);
    ui.add_space(6.0);
    if prompts.is_empty() {
        super::dim_label(ui, "No saved prompts yet.");
        ui.add_space(6.0);
    }
    for (index, prompt) in prompts.iter_mut().enumerate() {
        let marks = marks.get(index).copied().unwrap_or_default();
        ui.push_id((base, index), |ui| {
            let (row_changed, delete) = prompt_card(ui, prompt, marks, focus_new == Some(index));
            changed |= row_changed;
            if delete {
                remove = Some(index);
            }
        });
        ui.add_space(6.0);
    }
    if let Some(index) = remove {
        changed |= remove_prompt(prompts, index);
    }
    if hub_ui::secondary_button(ui, "+ Add prompt").clicked() {
        let index = add_prompt(prompts);
        ui.data_mut(|data| data.insert_temp(focus_key, index));
        ui.ctx().request_repaint();
        changed = true;
    }
    changed
}

fn prompt_card(ui: &mut Ui, prompt: &mut SavedPrompt, marks: RowMarks, focus_name: bool) -> (bool, bool) {
    let mut changed = false;
    let mut delete = false;
    Frame::default()
        .fill(theme::PANEL_BG_ALT())
        .stroke(Stroke::new(1.0_f32, theme::BORDER_SUBTLE()))
        .corner_radius(6)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);

            ui.horizontal(|ui| {
                let name = ui.add_sized(
                    [NAME_WIDTH, FIELD_HEIGHT],
                    TextEdit::singleline(&mut prompt.name)
                        .hint_text("Name")
                        .margin(Margin::symmetric(8, 6)),
                );
                if focus_name {
                    name.request_focus();
                    name.scroll_to_me(Some(Align::Center));
                }
                changed |= name.changed();
                if marks.name.is_some_and(PromptIssueKind::is_hard) {
                    outline(ui, &name);
                }
                delete = hub_ui::icon_button(ui, IconKind::Remove, "Delete prompt").clicked();
            });
            if let Some(kind) = marks.name {
                issue_label(ui, kind);
            }

            let rows = text_rows(&prompt.text);
            let text = ScrollArea::vertical()
                .max_height(rows_to_height(rows))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::multiline(&mut prompt.text)
                            .font(FontId::monospace(TEXT_FONT_SIZE))
                            .hint_text("Prompt text")
                            .desired_rows(rows)
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(8, 6)),
                    )
                })
                .inner;
            changed |= text.changed();
            if prompt.text.contains('\r') {
                prompt.text = normalize_newlines(&prompt.text);
                changed = true;
            }
            if marks.text.is_some_and(PromptIssueKind::is_hard) {
                outline(ui, &text);
            }

            ui.horizontal(|ui| {
                if let Some(kind) = marks.text {
                    issue_label(ui, kind);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let bytes = prompt.text.len();
                    let color = if counter_over(bytes) {
                        theme::PALETTE_RED()
                    } else {
                        theme::FG_DIM()
                    };
                    ui.label(
                        RichText::new(format!("{bytes} / {MAX_PROMPT_TEXT_BYTES}"))
                            .size(10.0)
                            .color(color),
                    );
                });
            });
        });
    (changed, delete)
}

fn rows_to_height(rows: usize) -> f32 {
    let rows = u16::try_from(rows).map_or(0.0, f32::from);
    rows * TEXT_ROW_HEIGHT + 12.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(name: &str, text: &str) -> SavedPrompt {
        SavedPrompt {
            name: name.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn settings_prompt_editor_add_remove_and_issue_markers() {
        let mut prompts = Vec::new();

        let first = add_prompt(&mut prompts);
        let second = add_prompt(&mut prompts);

        assert_eq!((first, second), (0, 1));
        assert_eq!(prompts[0].name, "New prompt");
        assert_eq!(prompts[1].name, "New prompt 2");

        prompts[0].text = "Review this.".to_owned();
        prompts[1].name = "new PROMPT".to_owned();
        prompts[1].text = "x".repeat(MAX_PROMPT_TEXT_BYTES + 1);
        let marks = row_marks(&prompts);
        assert_eq!(marks[0], RowMarks::default());
        assert_eq!(marks[1].name, Some(PromptIssueKind::DuplicateName));
        assert_eq!(marks[1].text, Some(PromptIssueKind::TextTooLong));
        assert_eq!(marks[1].name.map(PromptIssueKind::message), Some("Name already used"));

        assert!(remove_prompt(&mut prompts, 1));
        assert!(!remove_prompt(&mut prompts, 5));
        assert_eq!(prompts.len(), 1);
        assert_eq!(row_marks(&prompts), [RowMarks::default()]);
    }

    #[test]
    fn a_fresh_prompt_only_lacks_text() {
        let mut prompts = vec![prompt("a", "t")];
        add_prompt(&mut prompts);

        let marks = row_marks(&prompts);

        assert_eq!(marks[1].name, None);
        assert_eq!(marks[1].text, Some(PromptIssueKind::EmptyText));
    }

    #[test]
    fn pasted_line_endings_become_plain_newlines() {
        assert_eq!(normalize_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize_newlines("no returns"), "no returns");
        assert!(validate_prompts(&[prompt("n", &normalize_newlines("a\r\nb"))]).is_empty());
    }

    #[test]
    fn drafts_are_hinted_but_not_hard_errors() {
        let marks = row_marks(&[prompt("", "x"), prompt("n", "")]);

        assert_eq!(marks[0].name, Some(PromptIssueKind::EmptyName));
        assert_eq!(marks[1].text, Some(PromptIssueKind::EmptyText));
        assert!(!PromptIssueKind::EmptyName.is_hard() && !PromptIssueKind::EmptyText.is_hard());
        assert!(HELP.contains("Ctrl+Enter"));
    }

    #[test]
    fn text_box_grows_with_the_text_within_bounds() {
        assert_eq!(text_rows(""), TEXT_MIN_ROWS);
        assert_eq!(text_rows("a\nb\nc\nd\ne"), 5);
        assert_eq!(text_rows(&"x\n".repeat(50)), TEXT_MAX_ROWS);
    }

    #[test]
    fn counter_turns_red_only_over_the_cap() {
        assert!(!counter_over(MAX_PROMPT_TEXT_BYTES));
        assert!(counter_over(MAX_PROMPT_TEXT_BYTES + 1));
    }
}
