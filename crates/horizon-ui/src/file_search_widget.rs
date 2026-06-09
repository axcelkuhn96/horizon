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

use egui::{Align, Frame, Layout, Margin, RichText, ScrollArea, Vec2};
use horizon_core::file_search::SearchOutcome;
use horizon_core::file_search_runner::SearchState;
use horizon_core::file_tree::FileTreeState;

use crate::file_tree_widget::file_type_icon;
use crate::theme;

/// Reserved height (px) for the search box header row inside the panel.
const INPUT_HEIGHT: f32 = 26.0;
/// Absolute safety cap on the characters of a matched line we will ever lay out,
/// regardless of how wide the panel is. The real limit is computed per-row from
/// the available pixel WIDTH (see [`chars_budget_for_width`]); this is only a
/// guard so a pathologically wide panel can't ask us to render a 100k-char line.
const MAX_LINE_CHARS: usize = 400;
/// Approximate width (px) of one monospace glyph at the 11px font used for match
/// lines. Used to convert the available pixel width of a row into a character
/// budget so lines elide to the panel width instead of a fixed char count.
const MONO_CHAR_WIDTH: f32 = 7.0;
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

/// One flattened, indexable row of the results list. The grouped
/// [`SearchOutcome`] (files, each with matches) is flattened into a single
/// `Vec<SearchRow>` so the results [`ScrollArea`] can be virtualized with
/// [`ScrollArea::show_rows`], which maps a visible index range to rows and only
/// lays out THOSE rows per frame. Without this, all ~1000 match rows were laid
/// out every frame, pegging the UI thread on large repos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRow<'a> {
    /// A "Results truncated (showing first N)" banner, shown once at the top
    /// when the engine hit its match cap.
    TruncationBanner { total: usize },
    /// A clickable file-path header introducing that file's matches.
    FileHeader { path: &'a Path },
    /// A single match line under the preceding [`SearchRow::FileHeader`].
    /// `path` is duplicated so clicking the row can reveal the file without
    /// scanning back for its header.
    Match {
        path: &'a Path,
        line_number: usize,
        line_text: &'a str,
        span: (usize, usize),
    },
}

/// Flatten a successful [`SearchOutcome`] into a single indexable row list:
/// an optional truncation banner, then, per file, a header row followed by one
/// row per match. Pure and side-effect free so it can be unit-tested; the order
/// mirrors the on-screen layout exactly so `show_rows` index ranges map back to
/// the right items.
#[must_use]
pub fn flatten_results(found: &SearchOutcome) -> Vec<SearchRow<'_>> {
    let total_matches: usize = found.results.iter().map(|r| r.matches.len()).sum();
    // Pre-size: optional banner + one header per file + one row per match.
    let mut rows = Vec::with_capacity(found.results.len() + total_matches + 1);
    if found.truncated {
        rows.push(SearchRow::TruncationBanner { total: total_matches });
    }
    for file in &found.results {
        rows.push(SearchRow::FileHeader { path: &file.path });
        for m in &file.matches {
            rows.push(SearchRow::Match {
                path: &file.path,
                line_number: m.line_number,
                line_text: &m.line_text,
                span: m.span,
            });
        }
    }
    rows
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

/// Converts the available pixel width of a match line into a character budget
/// (how many monospace glyphs fit), so lines are elided to the panel WIDTH
/// rather than a fixed char count. This is what keeps a long code line from
/// painting past the narrow Files panel and over the neighbouring terminal.
///
/// Pure and side-effect free so it can be unit-tested. `available_px` is the
/// horizontal room left for the text (panel width minus the gutter); the result
/// is clamped to `[1, MAX_LINE_CHARS]` so we always keep at least one glyph and
/// never exceed the absolute safety cap.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn chars_budget_for_width(available_px: f32) -> usize {
    if !available_px.is_finite() || available_px <= 0.0 {
        return 1;
    }
    // Leave one glyph of room for the trailing ellipsis. Clamp in f32 space to
    // [1, MAX_LINE_CHARS] (so the cast back to usize is always in range), then
    // round down to whole glyphs. `MAX_LINE_CHARS` is tiny, so the precision
    // loss on the bound is irrelevant.
    let glyphs = (available_px / MONO_CHAR_WIDTH - 1.0).clamp(1.0, MAX_LINE_CHARS as f32);
    glyphs.floor() as usize
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

    // Opaque background so the search panel doesn't bleed through to whatever
    // is rendered behind the explorer (other panels, terminals on the canvas).
    Frame::new()
        .fill(theme::PANEL_BG())
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            show_search_panel_inner(ui, state, panel_id, repaint_requested, &mut action);
        });

    action
}

/// Inner render: query input + separator + results scroll area. Extracted so
/// the outer [`show_search_panel`] can wrap everything in an opaque
/// [`egui::Frame`] without nesting the whole function.
fn show_search_panel_inner(
    ui: &mut egui::Ui,
    state: &mut FileTreeState,
    panel_id: u64,
    repaint_requested: &mut bool,
    action: &mut Option<SearchUiAction>,
) {
    // Clip ALL search-panel painting to the panel's own rect: combined with the
    // per-row width elision, a hard guarantee no glyph paints past the panel's
    // right edge and over the neighbouring terminal, however narrow the panel is.
    ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));

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
                *action = Some(SearchUiAction::Close);
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
    // The search panel now takes over the whole explorer body (the tree is not
    // rendered while searching), so results get the full remaining height and
    // scroll internally. Reserve a little room for the bottom separator.
    let results_max_h = (ui.available_height() - 8.0).max(60.0);

    // Decide what to render. For the `Done(Ok)` case we flatten into rows and
    // virtualize so only visible rows are laid out per frame (the fix for the
    // freeze on large result sets). Non-result states are a single line.
    match state.search.state() {
        SearchState::Idle => {
            results_scroll(results_max_h, panel_id).show(ui, |ui| {
                if !state.search.query.trim().is_empty() {
                    dim_line(ui, "Type to search\u{2026}");
                }
            });
        }
        SearchState::Searching => {
            *repaint_requested = true;
            results_scroll(results_max_h, panel_id).show(ui, |ui| {
                dim_line(ui, "Searching\u{2026}");
            });
        }
        SearchState::Done(outcome) => match &outcome.result {
            Err(err) => {
                results_scroll(results_max_h, panel_id).show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(err.to_string()).size(12.0).color(theme::PALETTE_RED()));
                    });
                });
            }
            Ok(found) if found.results.is_empty() => {
                results_scroll(results_max_h, panel_id).show(ui, |ui| {
                    dim_line(ui, "No results");
                });
            }
            Ok(found) => {
                let rows = flatten_results(found);
                results_scroll(results_max_h, panel_id).show_rows(ui, RESULT_ROW_HEIGHT, rows.len(), |ui, range| {
                    // Paint an opaque fill behind the visible viewport so
                    // results never bleed through to content underneath.
                    let vp = ui.max_rect();
                    ui.painter()
                        .rect_filled(vp, egui::CornerRadius::ZERO, theme::PANEL_BG());
                    for row in &rows[range] {
                        render_row(ui, row, action);
                    }
                });
            }
        },
    }

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
}

/// Builds the configured results [`ScrollArea`] (bounded height, no auto-shrink,
/// salted id). Shared by every result state so they scroll identically.
fn results_scroll(max_height: f32, panel_id: u64) -> ScrollArea {
    ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([false, false])
        .id_salt(("file_search_results", panel_id))
}

/// Renders a single flattened [`SearchRow`] at the current cursor. Called only
/// for rows in the visible viewport range (virtualized), so per-frame cost is
/// bounded by what's on screen, not by the total result count.
fn render_row(ui: &mut egui::Ui, row: &SearchRow<'_>, action: &mut Option<SearchUiAction>) {
    match *row {
        SearchRow::TruncationBanner { total } => {
            let resp = ui.allocate_response(Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT), egui::Sense::hover());
            ui.painter().text(
                egui::Pos2::new(resp.rect.min.x + 8.0, resp.rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!("Results truncated (showing first {total})"),
                egui::FontId::proportional(11.0),
                theme::PALETTE_YELLOW(),
            );
        }
        SearchRow::FileHeader { path } => render_file_header(ui, path, action),
        SearchRow::Match {
            path,
            line_number,
            line_text,
            span,
        } => render_match_row(ui, path, line_number, line_text, span, action),
    }
}

/// Renders a clickable file-path header row (icon + compact path).
fn render_file_header(ui: &mut egui::Ui, path: &Path, action: &mut Option<SearchUiAction>) {
    let header = ui.allocate_response(Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT), egui::Sense::click());
    let header_rect = header.rect;
    if header.hovered() {
        ui.painter()
            .rect_filled(header_rect, egui::CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }
    let painter = ui.painter();
    let mid_y = header_rect.center().y;
    let icon = file_type_icon(path, false);
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
        display_path(path),
        egui::FontId::proportional(12.0),
        theme::FG_SOFT(),
    );
    if header.clicked() && action.is_none() {
        *action = Some(SearchUiAction::Reveal(path.to_path_buf()));
    }
}

/// Renders a single match row (line-number gutter + highlighted line). Clicking
/// reveals the owning file in the tree.
fn render_match_row(
    ui: &mut egui::Ui,
    path: &Path,
    line_number: usize,
    line_text: &str,
    span: (usize, usize),
    action: &mut Option<SearchUiAction>,
) {
    let row = ui.allocate_response(Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT), egui::Sense::click());
    let row_rect = row.rect;
    // Text starts after the line-number gutter; elide to the pixel width that
    // remains up to the row's right edge so the line can never paint past the
    // panel (and over the neighbouring terminal).
    let text_x = row_rect.min.x + 64.0;
    let avail_px = (row_rect.max.x - text_x).max(0.0);
    let budget = chars_budget_for_width(avail_px);
    let display = format_match_line(line_text, span, budget);
    if row.hovered() {
        ui.painter()
            .rect_filled(row_rect, egui::CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }
    // Clip all painting for this row to the row rect so no glyph spills past the
    // panel's right edge even if width estimation is off by a glyph.
    let painter = ui.painter().with_clip_rect(row_rect);
    let mid_y = row_rect.center().y;
    // Line number gutter.
    painter.text(
        egui::Pos2::new(row_rect.min.x + 28.0, mid_y),
        egui::Align2::LEFT_CENTER,
        format!("{line_number}"),
        egui::FontId::monospace(11.0),
        theme::FG_DIM(),
    );
    // Line text (highlighting the matched span when visible).
    paint_match_text(&painter, egui::Pos2::new(text_x, mid_y), &display);
    if row.clicked() && action.is_none() {
        *action = Some(SearchUiAction::Reveal(path.to_path_buf()));
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
    painter.text(
        egui::Pos2::new(x, pos.y),
        egui::Align2::LEFT_CENTER,
        after,
        font,
        theme::FG(),
    );
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
    use horizon_core::file_search::{FileSearchResult, SearchMatch};

    fn file(path: &str, matches: Vec<(usize, &str, (usize, usize))>) -> FileSearchResult {
        FileSearchResult {
            path: PathBuf::from(path),
            matches: matches
                .into_iter()
                .map(|(line_number, line_text, span)| SearchMatch {
                    line_number,
                    line_text: line_text.to_owned(),
                    span,
                })
                .collect(),
        }
    }

    #[test]
    fn flatten_results_empty_outcome_is_no_rows() {
        let outcome = SearchOutcome {
            results: Vec::new(),
            truncated: false,
        };
        assert!(flatten_results(&outcome).is_empty());
    }

    #[test]
    fn flatten_results_header_then_matches_in_order() {
        let outcome = SearchOutcome {
            results: vec![
                file("a.rs", vec![(1, "one", (0, 1)), (2, "two", (0, 1))]),
                file("b.rs", vec![(3, "three", (0, 1))]),
            ],
            truncated: false,
        };
        let rows = flatten_results(&outcome);
        // 2 headers + 3 matches = 5 rows, no banner.
        assert_eq!(rows.len(), 5);
        assert!(matches!(rows[0], SearchRow::FileHeader { path } if path == Path::new("a.rs")));
        assert!(matches!(rows[1], SearchRow::Match { line_number: 1, .. }));
        assert!(matches!(rows[2], SearchRow::Match { line_number: 2, .. }));
        assert!(matches!(rows[3], SearchRow::FileHeader { path } if path == Path::new("b.rs")));
        assert!(matches!(rows[4], SearchRow::Match { line_number: 3, .. }));
    }

    #[test]
    fn flatten_results_truncation_banner_is_first_row_with_total() {
        let outcome = SearchOutcome {
            results: vec![file("a.rs", vec![(1, "x", (0, 1)), (2, "y", (0, 1))])],
            truncated: true,
        };
        let rows = flatten_results(&outcome);
        // banner + 1 header + 2 matches = 4 rows.
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], SearchRow::TruncationBanner { total: 2 }));
        assert!(matches!(rows[1], SearchRow::FileHeader { .. }));
    }

    #[test]
    fn flatten_results_match_carries_owning_path() {
        let outcome = SearchOutcome {
            results: vec![file("dir/a.rs", vec![(7, "hit", (0, 3))])],
            truncated: false,
        };
        let rows = flatten_results(&outcome);
        match rows[1] {
            SearchRow::Match {
                path,
                line_number,
                line_text,
                span,
            } => {
                assert_eq!(path, Path::new("dir/a.rs"));
                assert_eq!(line_number, 7);
                assert_eq!(line_text, "hit");
                assert_eq!(span, (0, 3));
            }
            ref other => panic!("expected Match, got {other:?}"),
        }
    }

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
    fn chars_budget_scales_with_width_and_is_clamped() {
        // Zero / non-positive / non-finite widths keep at least one glyph.
        assert_eq!(chars_budget_for_width(0.0), 1);
        assert_eq!(chars_budget_for_width(-50.0), 1);
        assert_eq!(chars_budget_for_width(f32::NAN), 1);
        // A wider budget fits more glyphs than a narrower one.
        let narrow = chars_budget_for_width(70.0);
        let wide = chars_budget_for_width(700.0);
        assert!(wide > narrow, "wider panel must allow more chars: {wide} > {narrow}");
        // Never exceeds the absolute safety cap, even for an enormous width.
        assert!(chars_budget_for_width(1.0e6) <= MAX_LINE_CHARS);
    }

    #[test]
    fn width_based_elision_drops_highlight_when_match_is_off_screen() {
        // A long line whose match sits far to the right. With a small pixel
        // budget (a narrow panel) the line must elide and the now-off-screen
        // highlight must be dropped — i.e. nothing to paint past the panel edge.
        let mut line = "a".repeat(80);
        line.push_str("needle");
        let budget = chars_budget_for_width(70.0); // ~9 glyphs
        assert!(budget < 80, "test assumes the match is past the budget");
        let out = format_match_line(&line, (80, 86), budget);
        assert!(out.text.chars().count() <= budget + 1); // kept glyphs + ellipsis
        assert!(out.text.ends_with('\u{2026}'));
        assert_eq!(out.highlight, None);
    }

    #[test]
    fn width_based_elision_keeps_highlight_when_match_fits() {
        // The match is near the start, so a generous width budget keeps it.
        let mut line = "needle".to_owned();
        line.push_str(&"x".repeat(500));
        let budget = chars_budget_for_width(700.0);
        let out = format_match_line(&line, (0, 6), budget);
        assert_eq!(out.highlight, Some((0, 6)));
        let (s, e) = out.highlight.expect("highlight");
        assert_eq!(&out.text[s..e], "needle");
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
