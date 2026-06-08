//! Content-search ("search in files") panel for the File Explorer.
//!
//! Split out of [`crate::file_tree_widget`] (which is near the 1000-line cap)
//! so the search UI can grow independently. Renders a query input plus the
//! background [`SearchRunner`](horizon_core::file_search_runner::SearchRunner)'s
//! current state — a spinner line, an inline error, or results grouped by file
//! — above the tree. Clicking a result row reveals the file in the tree by
//! expanding its ancestor directories.
//!
//! The widget is stateless beyond what it borrows: the persistent search state
//! (query, runner, debounce bookkeeping) lives in
//! [`horizon_core::file_tree::SearchPanelState`], reached through the panel's
//! [`FileTreeState`]. This module only paints it and reports a [`SearchUiAction`].

use std::path::{Path, PathBuf};

use egui::{Align, Layout, Margin, RichText, ScrollArea, Vec2};
use horizon_core::file_search::{FileSearchResult, SearchOutcome};
use horizon_core::file_search_runner::SearchState;
use horizon_core::file_tree::FileTreeState;

use crate::file_tree_widget::file_type_icon;
use crate::theme;

/// Reserved height (px) for the search box header row inside the panel.
const INPUT_HEIGHT: f32 = 26.0;
/// Max characters of a matched line shown before elision. Long lines are
/// trimmed of leading indentation and clipped so results stay readable.
const MAX_LINE_CHARS: usize = 160;
/// Height of one results row (file header or match line).
const RESULT_ROW_HEIGHT: f32 = 20.0;

/// What the caller should do after rendering the search panel. Collected during
/// the (state-borrowing) render and applied by the caller afterwards.
pub enum SearchUiAction {
    /// User pressed Esc or the close button — close the search panel.
    Close,
    /// User clicked a match row — reveal this file in the tree (expand its
    /// ancestor directories and scroll it into view).
    Reveal(PathBuf),
}

/// A matched line prepared for display: an elided string plus the byte span of
/// the match relative to that string (when it survives elision).
#[derive(Debug, PartialEq, Eq)]
pub struct MatchLineDisplay {
    /// The (possibly trimmed/elided) line text.
    pub text: String,
    /// Byte span of the match within `text`, if the match is still visible
    /// after trimming. `None` when elision dropped the matched region.
    pub highlight: Option<(usize, usize)>,
}

/// Prepares a matched line for display: strips leading whitespace (tracking how
/// much was removed so the span stays correct) and truncates very long lines.
///
/// Pure and side-effect free so it can be unit-tested. The input `span` is a
/// byte range into `line_text` (guaranteed char-boundary by the search engine).
/// Returns the trimmed text and the span relative to it, or `None` highlight if
/// trimming/truncation moved the match out of view.
#[must_use]
pub fn format_match_line(line_text: &str, span: (usize, usize), max_chars: usize) -> MatchLineDisplay {
    // Drop leading whitespace; shift the span left by the dropped byte count.
    let trimmed = line_text.trim_start();
    let dropped = line_text.len() - trimmed.len();
    let (mut start, mut end) = span;
    // If the match started inside the dropped indentation, it has no valid
    // highlight position in the trimmed text.
    let highlight = if start >= dropped {
        start -= dropped;
        end = end.saturating_sub(dropped);
        Some((start, end))
    } else {
        None
    };

    // Truncate by characters (not bytes) so we never split a code point.
    if trimmed.chars().count() > max_chars {
        let cut: String = trimmed.chars().take(max_chars).collect();
        let cut_len = cut.len();
        let text = format!("{cut}\u{2026}");
        // Keep the highlight only if it lies fully within the kept prefix.
        let highlight = highlight.and_then(|(s, e)| (e <= cut_len).then_some((s, e)));
        return MatchLineDisplay { text, highlight };
    }

    MatchLineDisplay {
        text: trimmed.to_owned(),
        highlight,
    }
}

/// Renders the content-search panel for `state` and returns an action for the
/// caller to apply (it cannot mutate the tree here without re-borrowing).
///
/// `panel_id` salts egui ids so multiple explorer panels don't collide.
/// `repaint_requested` is set to `true` while a search is in flight so the
/// caller can keep animating until results arrive.
pub fn show_search_panel(
    ui: &mut egui::Ui,
    state: &mut FileTreeState,
    panel_id: u64,
    repaint_requested: &mut bool,
) -> Option<SearchUiAction> {
    let mut action: Option<SearchUiAction> = None;

    // --- Query input row -------------------------------------------------
    ui.add_space(4.0);
    ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), INPUT_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(8.0);
            // Magnifier glyph (nf-fa-search) as a non-interactive prefix.
            ui.label(RichText::new("\u{f002}").size(12.0).color(theme::FG_DIM()));
            ui.add_space(6.0);

            let edit = egui::TextEdit::singleline(&mut state.search.query)
                .font(egui::FontId::proportional(13.0))
                .text_color(theme::FG())
                .frame(false)
                .desired_width(ui.available_width() - 24.0)
                .hint_text(
                    RichText::new("Search in files\u{2026}")
                        .color(theme::FG_DIM())
                        .size(12.0),
                )
                .margin(Margin::ZERO);
            let response = ui.add(edit);

            // Grab keyboard focus the frame after the panel was opened.
            if state.search.focus_requested {
                response.request_focus();
                state.search.focus_requested = false;
            }
            if response.changed() {
                state.search.mark_edited();
            }
            // Esc closes the panel (only honored while the input has focus so it
            // doesn't swallow a global Esc).
            if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                action = Some(SearchUiAction::Close);
            }
        },
    );

    // Separator under the input.
    let sep_y = ui.cursor().min.y + 2.0;
    ui.painter().line_segment(
        [
            egui::Pos2::new(ui.max_rect().min.x, sep_y),
            egui::Pos2::new(ui.max_rect().max.x, sep_y),
        ],
        egui::Stroke::new(1.0, theme::BORDER_SUBTLE()),
    );
    ui.add_space(4.0);

    // --- Results ---------------------------------------------------------
    // Bound the results to roughly the top third of the panel so the tree below
    // stays visible; it scrolls internally.
    let results_max_h = (ui.available_height() * 0.45).clamp(60.0, 320.0);
    ScrollArea::vertical()
        .max_height(results_max_h)
        .auto_shrink([false, false])
        .id_salt(("file_search_results", panel_id))
        .show(ui, |ui| match state.search.state() {
            SearchState::Idle => {
                if !state.search.query.trim().is_empty() {
                    dim_line(ui, "Type to search\u{2026}");
                }
            }
            SearchState::Searching => {
                *repaint_requested = true;
                dim_line(ui, "Searching\u{2026}");
            }
            SearchState::Done(outcome) => match &outcome.result {
                Err(err) => {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(err.to_string()).size(12.0).color(theme::PALETTE_RED()));
                    });
                }
                Ok(found) => render_results(ui, found, &mut action),
            },
        });

    ui.add_space(4.0);
    // Bottom separator between the search panel and the tree.
    let bot_y = ui.cursor().min.y;
    ui.painter().line_segment(
        [
            egui::Pos2::new(ui.max_rect().min.x, bot_y),
            egui::Pos2::new(ui.max_rect().max.x, bot_y),
        ],
        egui::Stroke::new(1.0, theme::BORDER_SUBTLE()),
    );
    ui.add_space(2.0);

    action
}

/// Renders the grouped `Ok` results: a truncation banner (if any), then each
/// file as a header row followed by its match rows.
fn render_results(ui: &mut egui::Ui, found: &SearchOutcome, action: &mut Option<SearchUiAction>) {
    if found.results.is_empty() {
        dim_line(ui, "No results");
        return;
    }
    if found.truncated {
        let total: usize = found.results.iter().map(|r| r.matches.len()).sum();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("Results truncated (showing first {total})"))
                    .size(11.0)
                    .color(theme::PALETTE_YELLOW()),
            );
        });
        ui.add_space(2.0);
    }
    for file in &found.results {
        render_file_group(ui, file, action);
    }
}

/// Renders one file's group: a clickable path header, then its match rows.
fn render_file_group(ui: &mut egui::Ui, file: &FileSearchResult, action: &mut Option<SearchUiAction>) {
    // File header row (path + icon). Clicking reveals it in the tree.
    let header = ui.allocate_response(
        Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT),
        egui::Sense::click(),
    );
    let header_rect = header.rect;
    if header.hovered() {
        ui.painter()
            .rect_filled(header_rect, egui::CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }
    let painter = ui.painter();
    let mid_y = header_rect.center().y;
    let icon = file_type_icon(&file.path, false);
    painter.text(
        egui::Pos2::new(header_rect.min.x + 12.0, mid_y),
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(12.0),
        theme::FG_SOFT(),
    );
    painter.text(
        egui::Pos2::new(header_rect.min.x + 28.0, mid_y),
        egui::Align2::LEFT_CENTER,
        display_path(&file.path),
        egui::FontId::proportional(12.0),
        theme::FG_SOFT(),
    );
    if header.clicked() && action.is_none() {
        *action = Some(SearchUiAction::Reveal(file.path.clone()));
    }

    // Match rows.
    for m in &file.matches {
        let display = format_match_line(&m.line_text, m.span, MAX_LINE_CHARS);
        let row = ui.allocate_response(
            Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT),
            egui::Sense::click(),
        );
        let row_rect = row.rect;
        if row.hovered() {
            ui.painter()
                .rect_filled(row_rect, egui::CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
        }
        let painter = ui.painter();
        let mid_y = row_rect.center().y;
        // Line number gutter.
        painter.text(
            egui::Pos2::new(row_rect.min.x + 28.0, mid_y),
            egui::Align2::LEFT_CENTER,
            format!("{}", m.line_number),
            egui::FontId::monospace(11.0),
            theme::FG_DIM(),
        );
        // Line text (highlighting the matched span when visible).
        let text_x = row_rect.min.x + 64.0;
        paint_match_text(painter, egui::Pos2::new(text_x, mid_y), &display);
        if row.clicked() && action.is_none() {
            *action = Some(SearchUiAction::Reveal(file.path.clone()));
        }
    }
}

/// Paints a match line at `pos` (left-center), bolding the matched span if one
/// survived elision. Splits the string into before / match / after segments.
fn paint_match_text(painter: &egui::Painter, pos: egui::Pos2, display: &MatchLineDisplay) {
    let font = egui::FontId::monospace(11.0);
    let Some((s, e)) = display.highlight else {
        painter.text(pos, egui::Align2::LEFT_CENTER, &display.text, font, theme::FG());
        return;
    };
    // Defensive: if the span is somehow out of range, paint plainly.
    if s > e || e > display.text.len() || !display.text.is_char_boundary(s) || !display.text.is_char_boundary(e) {
        painter.text(pos, egui::Align2::LEFT_CENTER, &display.text, font, theme::FG());
        return;
    }
    let before = &display.text[..s];
    let matched = &display.text[s..e];
    let after = &display.text[e..];
    let mut x = pos.x;
    let g_before = painter.text(
        egui::Pos2::new(x, pos.y),
        egui::Align2::LEFT_CENTER,
        before,
        font.clone(),
        theme::FG_DIM(),
    );
    x = g_before.max.x;
    let g_match = painter.text(
        egui::Pos2::new(x, pos.y),
        egui::Align2::LEFT_CENTER,
        matched,
        font.clone(),
        theme::ACCENT(),
    );
    x = g_match.max.x;
    painter.text(egui::Pos2::new(x, pos.y), egui::Align2::LEFT_CENTER, after, font, theme::FG());
}

/// A dim, indented one-line status message (e.g. "Searching…").
fn dim_line(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(text).size(12.0).color(theme::FG_DIM()));
    });
}

/// A compact display string for a result path: the file name plus its parent
/// directory name, keeping rows short without losing context.
fn display_path(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match path.parent().and_then(Path::file_name).and_then(|n| n.to_str()) {
        Some(parent) if !parent.is_empty() => format!("{parent}/{name}"),
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_match_line_trims_indentation_and_shifts_span() {
        // 4-space indent; match "needle" at bytes 4..10 of the original.
        let line = "    needle here";
        let out = format_match_line(line, (4, 10), MAX_LINE_CHARS);
        assert_eq!(out.text, "needle here");
        // Span shifts left by the 4 dropped bytes.
        assert_eq!(out.highlight, Some((0, 6)));
    }

    #[test]
    fn format_match_line_no_indent_keeps_span() {
        let line = "let x = needle;";
        let out = format_match_line(line, (8, 14), MAX_LINE_CHARS);
        assert_eq!(out.text, "let x = needle;");
        assert_eq!(out.highlight, Some((8, 14)));
        // The highlighted slice is exactly the match.
        let (s, e) = out.highlight.expect("highlight");
        assert_eq!(&out.text[s..e], "needle");
    }

    #[test]
    fn format_match_line_match_inside_dropped_indent_has_no_highlight() {
        // Match lies entirely within the leading whitespace (a tab character).
        let line = "\tcode";
        let out = format_match_line(line, (0, 1), MAX_LINE_CHARS);
        assert_eq!(out.text, "code");
        assert_eq!(out.highlight, None);
    }

    #[test]
    fn format_match_line_truncates_long_lines_with_ellipsis() {
        let line = "x".repeat(500);
        let out = format_match_line(&line, (0, 1), 10);
        assert_eq!(out.text.chars().count(), 11); // 10 kept + ellipsis
        assert!(out.text.ends_with('\u{2026}'));
        // Match at the very start survives.
        assert_eq!(out.highlight, Some((0, 1)));
    }

    #[test]
    fn format_match_line_drops_highlight_past_truncation() {
        // Match near the end of a long line is cut away by truncation.
        let mut line = "a".repeat(50);
        line.push_str("needle");
        let out = format_match_line(&line, (50, 56), 10);
        assert!(out.text.ends_with('\u{2026}'));
        assert_eq!(out.highlight, None);
    }

    #[test]
    fn display_path_includes_parent_dir() {
        let p = Path::new("/repo/src/main.rs");
        assert_eq!(display_path(p), "src/main.rs");
    }

    #[test]
    fn display_path_bare_file_has_no_parent() {
        let p = Path::new("main.rs");
        assert_eq!(display_path(p), "main.rs");
    }
}
