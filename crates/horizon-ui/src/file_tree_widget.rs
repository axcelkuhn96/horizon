//! `FileExplorer` panel rendering helpers.
//!
//! Provides [`file_type_icon`] (path -> Symbols Nerd Font glyph) and
//! [`FileExplorerView`], the egui widget that renders the lazy, gitignore-aware
//! project file tree with live git-status decorations. Mirrors the structure of
//! `git_changes_widget::GitChangesView` (`new(panel)` + `show(ui, is_focused)`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Vec2};
use horizon_core::file_tree::{
    ChangedTreeNode, FileNode, FileTreeState, RowHit, changed_file_tree, dir_contains_changes, status_for_path,
};
use horizon_core::{FileStatus, GitStatus, Panel};

use crate::scroll_forward::{forward_scroll_to_scroll_area, scroll_viewport_height};
use crate::theme;

const HEADER_HEIGHT: f32 = 28.0;
/// Height reserved below the scroll area for the footer when `code_missing`.
/// Covers the single ~10pt label line plus its 4px trailing space.
const FOOTER_HEIGHT: f32 = 22.0;
const ROW_HEIGHT: f32 = 22.0;
const ROW_FONT_SIZE: f32 = 12.0;
const ICON_FONT_SIZE: f32 = 13.0;
/// Horizontal shift per nesting level. Bumped to 16.0 (from 14.0) so deeply
/// nested children read as an obvious tree (2026-06-07, user request).
const INDENT_PER_DEPTH: f32 = 16.0;
const BASE_INDENT: f32 = 8.0;
/// Right-edge space reserved for the status letter so long names truncate
/// with `…` instead of running underneath it.
const LETTER_RESERVE: f32 = 26.0;
/// Right-edge padding for rows without a status letter.
const PLAIN_RESERVE: f32 = 10.0;
/// Fixed width of the icon column. The icon is painted centered (and clipped)
/// inside this column at an explicit x, so glyphs whose paint extent exceeds
/// their font advance (Nerd Font / emoji fallback) can never bleed over the
/// filename that follows.
const ICON_COL_WIDTH: f32 = 18.0;
/// Fixed width of the chevron (expand/collapse arrow) column. Constant for
/// every row — files have no chevron but reserve the same gap so their icon and
/// name align with directory rows.
const CHEVRON_COL_WIDTH: f32 = 12.0;
/// Gap between the icon column and the name text.
const ICON_NAME_GAP: f32 = 4.0;
/// Font size of the chevron arrows.
const CHEVRON_FONT_SIZE: f32 = 9.0;

/// Returns a Nerd Font glyph (Private Use Area) for a path. `is_dir` selects a
/// folder glyph. Unknown extensions fall back to a generic file glyph.
///
/// The returned glyph resolves against the Symbols Nerd Font registered in the
/// egui fallback stacks. Never panics.
#[must_use]
pub(crate) fn file_type_icon(path: &Path, is_dir: bool) -> &'static str {
    if is_dir {
        return "\u{f07b}"; // nf-fa-folder
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    // Special-cased filenames take precedence over extension matching.
    match name {
        ".gitignore" | ".gitattributes" => return "\u{e702}", // nf-dev-git
        "Cargo.toml" | "Cargo.lock" => return "\u{e7a8}",     // nf-dev-rust
        "Dockerfile" => return "\u{f308}",                    // nf-linux-docker
        _ => {}
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "rs" => "\u{e7a8}",                                            // rust
        "toml" | "yaml" | "yml" => "\u{e615}",                         // settings/seti
        "lock" => "\u{f023}",                                          // lock
        "md" | "markdown" => "\u{f48a}",                               // markdown
        "json" => "\u{e60b}",                                          // json
        "js" => "\u{e74e}",                                            // js
        "ts" => "\u{e628}",                                            // ts
        "tsx" | "jsx" => "\u{e7ba}",                                   // react
        "py" => "\u{e606}",                                            // python
        "sh" | "bash" | "zsh" => "\u{f489}",                           // terminal
        "html" | "htm" => "\u{e736}",                                  // html5
        "css" | "scss" | "sass" => "\u{e749}",                         // css3
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "\u{f1c5}", // image
        "txt" => "\u{f0f6}",                                           // file-text
        _ => "\u{f15b}",                                               // generic file
    }
}

/// Maps a git [`FileStatus`] to its single-letter badge and decoration color.
///
/// Returns `None` for paths with no pending change (rendered with the neutral
/// foreground color and no status letter).
#[must_use]
fn status_decoration(status: Option<FileStatus>) -> Option<(&'static str, Color32)> {
    match status {
        Some(FileStatus::Untracked) => Some(("U", theme::PALETTE_GREEN())),
        Some(FileStatus::Added) => Some(("A", theme::PALETTE_GREEN())),
        Some(FileStatus::Modified) => Some(("M", theme::PALETTE_YELLOW())),
        Some(FileStatus::Renamed) => Some(("R", theme::PALETTE_YELLOW())),
        Some(FileStatus::Deleted) => Some(("D", theme::PALETTE_RED())),
        None => None,
    }
}

/// Spawn `program <path>` detached. Returns `false` if the program could not be
/// launched (e.g. not on PATH). Never panics; never inherits our stdio.
///
/// Task 8 hardens and adds dedicated tests for this launcher.
fn try_launch_editor(program: &str, path: &Path) -> bool {
    Command::new(program)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Open a file in VS Code. Sets `code_missing` when the `code` binary is absent.
///
/// Minimal implementation so double-click works now; Task 8 hardens + tests it.
pub(crate) fn open_in_vscode(path: &Path, code_missing: &mut bool) {
    if try_launch_editor("code", path) {
        *code_missing = false;
    } else {
        *code_missing = true;
        tracing::warn!("failed to launch `code` for file open");
    }
}

/// Open `path` in VS Code, jumping to `line` when known. Fire-and-forget; logs
/// a warning if `code` could not be launched. Used by the terminal Ctrl+click
/// path (no `code_missing` UI sink there).
///
/// Defense-in-depth against argv flag smuggling: a path beginning with `-`
/// could be interpreted by `code` as a CLI flag, so it is refused outright
/// (the path itself is not logged). The no-line case uses a `--` argv
/// terminator so the path can never be parsed as an option. The `--goto` case
/// keeps `path:line` as the option's single value (inserting `--` there would
/// break it); the leading-`-` guard already makes that value safe.
pub(crate) fn open_path_in_vscode(path: &std::path::Path, line: Option<u32>) {
    let path_str = path.to_string_lossy();
    if path_str.starts_with('-') {
        tracing::warn!("refusing to open path beginning with '-'");
        return;
    }

    let mut command = Command::new("code");
    match line {
        Some(ln) => {
            command.arg("--goto").arg(format!("{}:{ln}", path.display()));
        }
        None => {
            command.arg("--").arg(path);
        }
    }
    let launched = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok();
    if !launched {
        tracing::warn!("failed to launch `code` for terminal file open");
    }
}

/// Renders the lazy project file tree for a `PanelKind::FileExplorer` panel.
pub struct FileExplorerView<'a> {
    panel: &'a mut Panel,
}

/// A deferred action collected during the (status-borrowing) tree render and
/// applied afterwards, when `state` can be mutably re-borrowed.
enum TreeAction {
    /// Lazily scan and expand the directory at this path.
    Expand(std::path::PathBuf),
    /// Re-hide (collapse) the directory at this path.
    Collapse(std::path::PathBuf),
    /// Open the file at this path in VS Code.
    Open(std::path::PathBuf),
    /// Expand a directory of the filtered (uncommitted) tree.
    ExpandChanged(std::path::PathBuf),
    /// Collapse a directory of the filtered (uncommitted) tree.
    CollapseChanged(std::path::PathBuf),
}

impl<'a> FileExplorerView<'a> {
    pub fn new(panel: &'a mut Panel) -> Self {
        Self { panel }
    }

    /// Renders the file explorer panel. Returns `true` if the pointer is over
    /// the panel (for focus tracking), mirroring `GitChangesView`.
    pub fn show(&mut self, ui: &mut egui::Ui, is_focused: bool) -> bool {
        let clicked = ui.rect_contains_pointer(ui.max_rect());

        let panel_id = self.panel.id.0;
        let Some(state) = self.panel.content.file_explorer_mut() else {
            return clicked;
        };

        if !state.loaded {
            state.reload_root();
        }

        // Auto-refresh the git-status snapshot (throttled while visible, plus an
        // immediate refresh on focus-regain). The shared GitWatcher only fires on
        // `.git/index` mtime changes, so plain working-tree edits would otherwise
        // leave the changed/green decorations stale until a commit.
        state.maybe_refresh_git_status(is_focused);

        let mut refresh = false;
        // Own the toggle value before the immutable borrows below; the header
        // flips it in place and we persist it back into `state` afterwards.
        let mut show_only = state.show_only_changes;
        render_header(ui, state, &mut refresh, &mut show_only);
        state.set_show_only_changes(show_only);

        // Content-search panel (Ctrl+Shift+F). When active it takes over the
        // entire explorer body: we render ONLY the search panel (a dedicated,
        // opaque full-panel surface) and skip the tree ScrollArea below, so the
        // two views never overlap. Its background runner is pumped every frame
        // and finished results (or an in-flight spinner) are painted by the
        // widget. The reveal action is applied after the panel render so we can
        // mutate the tree; the tree returns the moment search is closed.
        if state.search.active {
            let mut repaint = false;
            state.tick_search();
            let search_action = crate::file_search_widget::show_search_panel(ui, state, panel_id, &mut repaint);
            if repaint {
                ui.ctx().request_repaint();
            }
            match search_action {
                Some(crate::file_search_widget::SearchUiAction::Close) => state.close_search(),
                Some(crate::file_search_widget::SearchUiAction::Reveal(path)) => {
                    reveal_in_tree(&mut state.roots, &state.root, &path);
                }
                Some(crate::file_search_widget::SearchUiAction::Open(path)) => {
                    open_in_vscode(&path, &mut state.code_missing);
                }
                None => {}
            }
            // No stale row hit-map while searching: drops resolve to the root.
            state.row_hits = Vec::new();
            return clicked;
        }

        // Clone the status Arc out before the recursive render so the immutable
        // borrow of `state.git_status` does not conflict with the mutations we
        // apply afterwards (mirrors GitChangesView cloning `viewer.status`).
        let status = state.git_status.clone();
        let mut action: Option<TreeAction> = None;
        // Row selection collected this frame (the single source of truth for
        // which row is selected); applied to `state.selected` after the render.
        let mut selection: Option<(PathBuf, bool)> = None;
        // Screen-space hit boxes for the visible rows, rebuilt every frame so the
        // OS-file-drop handler can resolve the folder under the cursor. Only the
        // normal (on-disk) tree records hits; the filtered changes-only view is
        // not a drop target.
        let mut row_hits: Vec<RowHit> = Vec::new();

        // Bound the scroll area to the panel body so a tall tree clips and
        // scrolls inside the panel instead of painting over neighbouring
        // panels. Reserve footer space only when the footer is shown.
        let footer_h = if state.code_missing { FOOTER_HEIGHT } else { 0.0 };
        let max_h = scroll_viewport_height(ui.max_rect().bottom(), ui.cursor().top(), footer_h);

        // Snapshot the selected path before the immutable borrow in render_nodes.
        let selected_path: Option<PathBuf> = state.selected.as_ref().map(|(p, _)| p.clone());

        let scroll_output = ScrollArea::vertical()
            .max_height(max_h)
            .auto_shrink([false, false])
            .id_salt(("file_explorer_tree", panel_id))
            .show(ui, |ui| {
                ui.add_space(2.0);
                if show_only {
                    render_changes_only(ui, status.as_deref(), &state.changed_expanded, &mut action);
                } else {
                    let mut sink = RenderSink {
                        action: &mut action,
                        selection: &mut selection,
                        row_hits: &mut row_hits,
                    };
                    render_nodes(
                        ui,
                        &state.roots,
                        0,
                        status.as_deref(),
                        &mut sink,
                        selected_path.as_deref(),
                    );
                }
                ui.add_space(4.0);
            });
        // The panel `Area` is `interactable(false)`, so egui never registers the
        // pointer as hovering the scroll area; forward the wheel delta manually
        // (and consume it so the canvas pan handler ignores the same gesture).
        forward_scroll_to_scroll_area(
            ui,
            scroll_output.id,
            scroll_output.inner_rect,
            scroll_output.content_size.y,
        );

        render_footer(ui, state.code_missing);

        // Publish this frame's screen-space row hit-map for the OS-file-drop
        // handler (empty in changes-only mode, so drops resolve to the root).
        state.row_hits = row_hits;

        if refresh {
            state.reload_root();
        }

        // Apply the row selection collected this frame (single source of truth).
        // Done before reload-aware actions below; a refresh above already cleared
        // any stale selection, and a fresh click re-sets it here.
        if let Some((path, is_dir)) = selection {
            state.select_row(path, is_dir);
        }

        match action {
            Some(TreeAction::Expand(path)) => expand_node(&mut state.roots, &path),
            Some(TreeAction::Collapse(path)) => collapse_node(&mut state.roots, &path),
            Some(TreeAction::Open(path)) => open_in_vscode(&path, &mut state.code_missing),
            Some(TreeAction::ExpandChanged(path)) => state.expand_changed(path),
            Some(TreeAction::CollapseChanged(path)) => state.collapse_changed(&path),
            None => {}
        }

        clicked
    }
}

fn render_header(ui: &mut egui::Ui, state: &FileTreeState, refresh: &mut bool, show_only: &mut bool) {
    let header_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), HEADER_HEIGHT));
    ui.allocate_rect(header_rect, egui::Sense::hover());

    let root_name = state
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| state.root.to_str().unwrap_or("."));

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(header_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new(root_name)
                    .font(FontId::proportional(12.0))
                    .color(theme::FG())
                    .strong(),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(10.0);
                let refresh_btn =
                    ui.add(egui::Button::new(RichText::new("\u{27f3}").size(14.0).color(theme::FG_DIM())).frame(false));
                if refresh_btn.on_hover_text("Refresh").clicked() {
                    *refresh = true;
                }

                ui.add_space(8.0);
                // Funnel toggle: filters the view to only uncommitted files.
                // Accent when active, dim when off (nf-fa-filter).
                let filter_color = if *show_only { theme::ACCENT() } else { theme::FG_DIM() };
                let filter_btn =
                    ui.add(egui::Button::new(RichText::new("\u{f0b0}").size(13.0).color(filter_color)).frame(false));
                if filter_btn
                    .on_hover_text("Mostrar s\u{f3} n\u{e3}o-commitados")
                    .clicked()
                {
                    *show_only = !*show_only;
                }
            });
        },
    );

    let sep_y = header_rect.max.y;
    ui.painter().line_segment(
        [
            egui::Pos2::new(header_rect.min.x, sep_y),
            egui::Pos2::new(header_rect.max.x, sep_y),
        ],
        egui::Stroke::new(1.0, theme::BORDER_SUBTLE()),
    );
}

/// Per-frame output sinks collected while rendering the normal (on-disk) tree.
/// Bundling these mutable borrows keeps the recursive render functions to a sane
/// argument count and groups the three things one row can produce.
struct RenderSink<'a> {
    /// The primary action (expand/collapse/open) for the first interacted row.
    action: &'a mut Option<TreeAction>,
    /// The row selected this frame (single source of truth for selection).
    selection: &'a mut Option<(PathBuf, bool)>,
    /// Screen-space hit boxes for the OS-file-drop handler.
    row_hits: &'a mut Vec<RowHit>,
}

fn render_nodes(
    ui: &mut egui::Ui,
    nodes: &[FileNode],
    depth: usize,
    status: Option<&GitStatus>,
    sink: &mut RenderSink<'_>,
    selected: Option<&Path>,
) {
    for node in nodes {
        render_node(ui, node, depth, status, sink, selected);
    }
}

fn render_node(
    ui: &mut egui::Ui,
    node: &FileNode,
    depth: usize,
    status: Option<&GitStatus>,
    sink: &mut RenderSink<'_>,
    selected: Option<&Path>,
) {
    let is_dir = node.is_dir;
    let is_open = node.children.is_some();
    let is_selected = selected.is_some_and(|s| s == node.path.as_path());

    let response = render_row(ui, node, depth, status, sink.row_hits, is_selected);

    // Selection is the SINGLE source of truth here: any single click selects the
    // row (file or folder), recorded independently of the primary action below.
    // This is the only place selection is set, so folder selection works on the
    // same click that also expands/collapses — no reliance on the action arms.
    if response.clicked {
        *sink.selection = Some((node.path.clone(), is_dir));
    }

    // Primary action: toggle folder (single click) or open file (double click).
    // Orthogonal to selection — it never touches `*sink.selection`.
    if response.primary_action {
        *sink.action = Some(if is_dir {
            if is_open {
                TreeAction::Collapse(node.path.clone())
            } else {
                TreeAction::Expand(node.path.clone())
            }
        } else {
            TreeAction::Open(node.path.clone())
        });
    }

    if let Some(children) = &node.children {
        render_nodes(ui, children, depth + 1, status, sink, selected);
    }
}

/// The painted appearance of one tree row, resolved by the caller and handed to
/// [`paint_tree_row`]. Keeping all geometry in one painter pass (instead of
/// nested `ui.label` scopes) makes row layout robust: every element is placed at
/// an explicit x, so no glyph-advance / cursor-rewind quirk can stack the name
/// under the icon.
struct RowVisual<'a> {
    depth: usize,
    /// `Some(is_open)` for directories (draws a chevron), `None` for files.
    chevron: Option<bool>,
    icon: &'a str,
    icon_color: Color32,
    name: &'a str,
    name_color: Color32,
    /// Optional right-aligned status letter and its color.
    letter: Option<(&'a str, Color32)>,
}

/// Picks `(icon_color, name_color)` for a normal-tree row.
///
/// - A directory that contains uncommitted changes (`dir_changed`) renders both
///   icon and name in the untracked/added green, propagating change state up the
///   tree like `VSCode`. Other directories use the accent icon + soft name.
/// - Files use their git-status color (or the neutral foreground when clean) for
///   both icon and name.
///
/// Color precedence (highest first), `VSCode`-style:
/// 1. Git "changed" coloring (dir-green propagation, or a file's status
///    decoration) wins — `ignored` is intentionally ignored for these rows, since
///    "this entry changed" is the stronger signal even when it is also
///    gitignored. The status letter (drawn elsewhere) is likewise unaffected.
/// 2. Otherwise, a gitignored entry (`ignored`) is dimmed to `theme::FG_DIM()`
///    for both icon and name, visually setting temp/ignored files apart from
///    normal entries.
/// 3. Otherwise, the normal colors apply (accent icon + soft name for dirs,
///    neutral fg for files).
///
/// Selection is not modeled here: this widget has no per-row selection state
/// (rows highlight on hover only, painted independently of these colors), so the
/// selection-always-wins rule has no row to apply to. If selection is added
/// later, it must override the value returned here at the call site.
fn row_colors(
    is_dir: bool,
    dir_changed: bool,
    ignored: bool,
    decoration: Option<(&str, Color32)>,
) -> (Color32, Color32) {
    if is_dir {
        if dir_changed {
            (theme::PALETTE_GREEN(), theme::PALETTE_GREEN())
        } else if ignored {
            (theme::FG_DIM(), theme::FG_DIM())
        } else {
            (theme::ACCENT(), theme::FG_SOFT())
        }
    } else if let Some((_, color)) = decoration {
        // Changed file: status color wins over dim.
        (color, color)
    } else if ignored {
        (theme::FG_DIM(), theme::FG_DIM())
    } else {
        (theme::FG(), theme::FG())
    }
}

/// Return value from [`render_row`].
struct RowResponse {
    /// The user single-clicked this row (also fires when a directory is clicked
    /// to expand/collapse, and on the first click of a double-click sequence for
    /// files). Used to record the selection.
    clicked: bool,
    /// The primary action should fire: clicked for a directory (toggle), or
    /// double-clicked for a file (open in editor).
    primary_action: bool,
}

/// Renders one tree row. Returns a [`RowResponse`] indicating whether the row
/// was clicked (selection) and/or triggered its primary action (toggle/open).
fn render_row(
    ui: &mut egui::Ui,
    node: &FileNode,
    depth: usize,
    status: Option<&GitStatus>,
    row_hits: &mut Vec<RowHit>,
    is_selected: bool,
) -> RowResponse {
    let decoration = status.and_then(|s| status_decoration(status_for_path(s, &node.path)));

    // Normal-tree folder propagation: a directory that CONTAINS uncommitted
    // changes tints its icon+name green (VSCode-style), even though the folder
    // itself has no direct status. Files keep their own status color.
    let dir_changed = node.is_dir && status.is_some_and(|s| dir_contains_changes(s, &node.path));
    let (icon_color, name_color) = row_colors(node.is_dir, dir_changed, node.ignored, decoration);

    let visual = RowVisual {
        depth,
        chevron: node.is_dir.then_some(node.children.is_some()),
        icon: file_type_icon(&node.path, node.is_dir),
        icon_color,
        name: &node.name,
        name_color,
        letter: decoration,
    };

    let response = paint_tree_row(ui, &visual, is_selected);

    // Record the row's screen-space hit box for the OS-file-drop handler. The
    // tree paints inside a transform-layer Area, so map the local rect to global
    // coordinates the app-level pointer position is expressed in.
    let to_global = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
    let screen_rect = to_global * response.rect;
    row_hits.push(RowHit {
        rect: (
            screen_rect.min.x,
            screen_rect.min.y,
            screen_rect.max.x,
            screen_rect.max.y,
        ),
        path: node.path.clone(),
        is_dir: node.is_dir,
    });

    let primary_action = if node.is_dir {
        response.clicked()
    } else {
        response.double_clicked()
    };
    RowResponse {
        clicked: response.clicked(),
        primary_action,
    }
}

/// Paints one fully-manual tree row at explicit x offsets and returns the row's
/// click `Response`. Layout: `[indent][chevron col][icon col][gap][name,
/// ellipsis-truncated before the letter reserve]` with the status letter painted
/// absolutely at the right edge. No inner `ui.label`/scope is used, so nothing
/// can rewind the cursor and stack the name under the icon.
///
/// `is_selected` draws a subtle selection background using the accent token so
/// the Ctrl+V target is always visible. Hover and selection can coexist.
fn paint_tree_row(ui: &mut egui::Ui, v: &RowVisual<'_>, is_selected: bool) -> egui::Response {
    let row_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), ROW_HEIGHT));
    let response = ui.allocate_rect(row_rect, Sense::click());

    // Selection background (subtle accent) rendered below the hover highlight.
    if is_selected {
        ui.painter()
            .rect_filled(row_rect, CornerRadius::ZERO, theme::alpha(theme::ACCENT(), 20));
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(row_rect, CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }

    let painter = ui.painter();
    let mid_y = row_rect.center().y;

    #[allow(clippy::cast_precision_loss)]
    let indent = BASE_INDENT + v.depth as f32 * INDENT_PER_DEPTH;
    let mut x = row_rect.min.x + indent;

    // Chevron column (constant width for both dirs and files so icons align).
    if let Some(is_open) = v.chevron {
        let chevron = if is_open { "\u{25bc}" } else { "\u{25b6}" };
        painter.text(
            Pos2::new(x + CHEVRON_COL_WIDTH / 2.0, mid_y),
            Align2::CENTER_CENTER,
            chevron,
            FontId::proportional(CHEVRON_FONT_SIZE),
            theme::FG_DIM(),
        );
    }
    x += CHEVRON_COL_WIDTH;

    // Icon column: painted centered and clipped so a wide glyph can't bleed out.
    let icon_rect = Rect::from_min_size(Pos2::new(x, row_rect.min.y), Vec2::new(ICON_COL_WIDTH, ROW_HEIGHT));
    painter.with_clip_rect(icon_rect).text(
        icon_rect.center(),
        Align2::CENTER_CENTER,
        v.icon,
        FontId::proportional(ICON_FONT_SIZE),
        v.icon_color,
    );
    x += ICON_COL_WIDTH + ICON_NAME_GAP;

    // Name: laid out as a single-row, ellipsis-truncated galley bounded so it
    // never reaches the status-letter reserve at the right edge.
    let reserve = if v.letter.is_some() {
        LETTER_RESERVE
    } else {
        PLAIN_RESERVE
    };
    let name_max_x = (row_rect.max.x - reserve).max(x + 8.0);
    let mut job = LayoutJob::single_section(
        v.name.to_owned(),
        TextFormat {
            font_id: FontId::proportional(ROW_FONT_SIZE),
            color: v.name_color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(name_max_x - x);
    let galley = painter.layout_job(job);
    let name_y = mid_y - galley.size().y / 2.0;
    painter.galley(Pos2::new(x, name_y), galley, v.name_color);

    // Status letter, painted absolutely at the right edge.
    if let Some((letter, color)) = v.letter {
        painter.text(
            Pos2::new(row_rect.max.x - 12.0, mid_y),
            Align2::RIGHT_CENTER,
            letter,
            FontId::monospace(11.0),
            color,
        );
    }

    response
}

/// Renders the grouped "only uncommitted files" tree. With no git snapshot,
/// shows a dim "Sem reposit\u{f3}rio git" message; with an empty change set,
/// shows "Nada para commitar". Changed files are nested under their parent
/// folders (collapsed by default, single-child chains compacted VSCode-style);
/// folders render green with a caret and expand on click. Double-clicking a
/// file row collects a [`TreeAction::Open`].
fn render_changes_only(
    ui: &mut egui::Ui,
    status: Option<&GitStatus>,
    expanded: &HashSet<PathBuf>,
    action: &mut Option<TreeAction>,
) {
    let Some(status) = status else {
        render_centered_dim(ui, "Sem reposit\u{f3}rio git");
        return;
    };

    let tree = changed_file_tree(status);
    if tree.is_empty() {
        render_centered_dim(ui, "Nada para commitar");
        return;
    }

    render_changed_nodes(ui, &tree, 0, expanded, action);
}

fn render_changed_nodes(
    ui: &mut egui::Ui,
    nodes: &[ChangedTreeNode],
    depth: usize,
    expanded: &HashSet<PathBuf>,
    action: &mut Option<TreeAction>,
) {
    for node in nodes {
        if node.is_dir {
            let is_open = expanded.contains(&node.abs_path);
            if render_changed_row(ui, node, depth, is_open) && action.is_none() {
                *action = Some(if is_open {
                    TreeAction::CollapseChanged(node.abs_path.clone())
                } else {
                    TreeAction::ExpandChanged(node.abs_path.clone())
                });
            }
            if is_open {
                render_changed_nodes(ui, &node.children, depth + 1, expanded, action);
            }
        } else if render_changed_row(ui, node, depth, false) && action.is_none() {
            *action = Some(TreeAction::Open(node.abs_path.clone()));
        }
    }
}

/// Caret and folder glyph for a directory row of the filtered tree, by
/// expansion state. Carets mirror the normal tree (`\u{25bc}`/`\u{25b6}`); the
/// folder glyph swaps between closed and open. The chevron is drawn by
/// [`paint_tree_row`] from `RowVisual.chevron`; this helper remains the single
/// source of truth for the open/closed folder icon (and its caret pairing is
/// asserted by tests).
fn changed_dir_visuals(is_open: bool) -> (&'static str, &'static str) {
    if is_open {
        ("\u{25bc}", "\u{f07c}") // nf-fa-folder_open
    } else {
        ("\u{25b6}", "\u{f07b}") // nf-fa-folder
    }
}

/// Renders one row of the grouped uncommitted-files tree: directories show a
/// caret plus a green folder icon and green name (green marks "contains
/// uncommitted changes"); files show their type icon, the name colored by
/// status (truncated with `…` when too wide), and a fixed right-aligned
/// status letter. Returns `true` when the row should act — click for a
/// directory (toggle), double-click for a file (open in editor).
fn render_changed_row(ui: &mut egui::Ui, node: &ChangedTreeNode, depth: usize, is_open: bool) -> bool {
    let decoration = status_decoration(node.status);

    let (icon, icon_color, name_color) = if node.is_dir {
        // Filter-tree dirs are green by construction (they all contain changes).
        let (_, folder_icon) = changed_dir_visuals(is_open);
        (folder_icon, theme::PALETTE_GREEN(), theme::PALETTE_GREEN())
    } else {
        let c = decoration.map_or_else(theme::FG, |(_, color)| color);
        (file_type_icon(&node.abs_path, false), c, c)
    };

    let visual = RowVisual {
        depth,
        chevron: node.is_dir.then_some(is_open),
        icon,
        icon_color,
        name: &node.name,
        name_color,
        letter: decoration,
    };

    // The filtered tree has no persistent selection model — selection only tracks
    // rows in the normal (on-disk) tree, so `is_selected` is always false here.
    let response = paint_tree_row(ui, &visual, false);

    if node.is_dir {
        response.clicked()
    } else {
        response.double_clicked()
    }
}

/// Centered, dim status message used for the empty / no-repo filtered states.
fn render_centered_dim(ui: &mut egui::Ui, text: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(RichText::new(text).size(13.0).color(theme::FG_DIM()));
    });
}

fn render_footer(ui: &mut egui::Ui, code_missing: bool) {
    if !code_missing {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(
            RichText::new("VS Code (`code`) n\u{e3}o encontrado no PATH")
                .font(FontId::proportional(10.0))
                .color(theme::FG_DIM()),
        );
    });
    ui.add_space(4.0);
}

/// Reveals `target` in the tree by expanding every ancestor directory between
/// `root` (exclusive) and `target`'s parent (inclusive), lazily scanning each
/// level so the file's row becomes visible.
///
/// Walks the path components from `root` down: at each step it finds the matching
/// directory node, ensures its children are loaded, then descends. Stops early
/// (no panic) if a component isn't found — e.g. the file was deleted, lives under
/// a `HARD_SKIP` dir, or sits outside `root`. There is no persistent selection
/// model, so this only expands-to-visible; a highlight is a possible Next-Step.
fn reveal_in_tree(roots: &mut Vec<FileNode>, root: &Path, target: &Path) {
    let Ok(rel) = target.strip_prefix(root) else {
        return;
    };
    // The components to descend are every ancestor dir of the file (drop the
    // file name itself).
    let mut components: Vec<_> = rel.components().collect();
    components.pop(); // file name — we only expand directories

    let mut abs = root.to_path_buf();
    let mut level: &mut Vec<FileNode> = roots;
    for comp in components {
        abs.push(comp);
        let Some(idx) = level.iter().position(|n| n.is_dir && n.path == abs) else {
            return; // ancestor not present in the scanned tree; stop quietly
        };
        FileTreeState::ensure_children(&mut level[idx]);
        match level[idx].children.as_mut() {
            Some(children) => level = children,
            None => return,
        }
    }
}

/// Finds the node at `path` and lazily scans its children (expand).
fn expand_node(nodes: &mut [FileNode], path: &Path) {
    if let Some(node) = find_node_mut(nodes, path) {
        FileTreeState::ensure_children(node);
    }
}

/// Finds the node at `path` and drops its cached children (collapse, re-hiding
/// the subtree). Re-expanding lazily re-scans.
fn collapse_node(nodes: &mut [FileNode], path: &Path) {
    if let Some(node) = find_node_mut(nodes, path) {
        node.children = None;
    }
}

/// Depth-first search for the (unique) node whose `path` matches.
fn find_node_mut<'n>(nodes: &'n mut [FileNode], path: &Path) -> Option<&'n mut FileNode> {
    for node in nodes {
        if node.path == path {
            return Some(node);
        }
        if let Some(children) = &mut node.children
            && let Some(found) = find_node_mut(children, path)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn known_extensions_get_distinct_icons() {
        let rs = file_type_icon(Path::new("main.rs"), false);
        let json = file_type_icon(Path::new("pkg.json"), false);
        let generic = file_type_icon(Path::new("data.unknownext"), false);
        assert_ne!(rs, generic);
        assert_ne!(json, generic);
        assert_ne!(rs, json);
    }

    #[test]
    fn directories_use_folder_icons() {
        let closed = file_type_icon(Path::new("src"), true);
        let file = file_type_icon(Path::new("src.rs"), false);
        assert_ne!(closed, file);
    }

    #[test]
    fn extensionless_file_falls_back_to_generic() {
        let dockerfile = file_type_icon(Path::new("Dockerfile"), false);
        let generic = file_type_icon(Path::new("noext"), false);
        // both resolve to *some* glyph without panicking
        assert!(!dockerfile.is_empty());
        assert!(!generic.is_empty());
    }

    #[test]
    fn status_decoration_maps_statuses_to_letters_and_colors() {
        assert_eq!(status_decoration(None), None);
        assert_eq!(
            status_decoration(Some(FileStatus::Untracked)),
            Some(("U", theme::PALETTE_GREEN()))
        );
        assert_eq!(
            status_decoration(Some(FileStatus::Added)),
            Some(("A", theme::PALETTE_GREEN()))
        );
        assert_eq!(
            status_decoration(Some(FileStatus::Modified)),
            Some(("M", theme::PALETTE_YELLOW()))
        );
        assert_eq!(
            status_decoration(Some(FileStatus::Renamed)),
            Some(("R", theme::PALETTE_YELLOW()))
        );
        assert_eq!(
            status_decoration(Some(FileStatus::Deleted)),
            Some(("D", theme::PALETTE_RED()))
        );
    }

    #[test]
    fn changed_dir_visuals_follow_expansion_state() {
        let (closed_caret, closed_icon) = changed_dir_visuals(false);
        let (open_caret, open_icon) = changed_dir_visuals(true);
        // carets match the normal tree's glyphs
        assert_eq!(closed_caret, "\u{25b6}");
        assert_eq!(open_caret, "\u{25bc}");
        // closed vs open folder icons differ
        assert_ne!(closed_icon, open_icon);
        assert_eq!(closed_icon, "\u{f07b}"); // nf-fa-folder
        assert_eq!(open_icon, "\u{f07c}"); // nf-fa-folder_open
    }

    #[test]
    fn dir_with_changes_inside_renders_green_in_normal_tree() {
        // Folder propagation: a changed dir tints icon+name green; a clean dir
        // keeps accent icon + soft name; a clean file uses the neutral fg.
        let (changed_icon, changed_name) = row_colors(true, true, false, None);
        assert_eq!(changed_icon, theme::PALETTE_GREEN());
        assert_eq!(changed_name, theme::PALETTE_GREEN());

        let (clean_icon, clean_name) = row_colors(true, false, false, None);
        assert_eq!(clean_icon, theme::ACCENT());
        assert_eq!(clean_name, theme::FG_SOFT());
        assert_ne!(clean_name, theme::PALETTE_GREEN());

        // A file with a status keeps its status color (not the dir-green path).
        let (file_icon, file_name) = row_colors(false, false, false, Some(("M", theme::PALETTE_YELLOW())));
        assert_eq!(file_icon, theme::PALETTE_YELLOW());
        assert_eq!(file_name, theme::PALETTE_YELLOW());
    }

    #[test]
    fn ignored_entries_are_dimmed_when_clean() {
        // A clean (unchanged) gitignored file dims both icon and name, instead of
        // the neutral foreground a normal file would get.
        let (icon, name) = row_colors(false, false, true, None);
        assert_eq!(icon, theme::FG_DIM());
        assert_eq!(name, theme::FG_DIM());
        assert_ne!(name, theme::FG());

        // A clean gitignored directory dims too, instead of accent icon/soft name.
        let (dir_icon, dir_name) = row_colors(true, false, true, None);
        assert_eq!(dir_icon, theme::FG_DIM());
        assert_eq!(dir_name, theme::FG_DIM());
        assert_ne!(dir_icon, theme::ACCENT());
    }

    #[test]
    fn changed_coloring_takes_precedence_over_ignored_dim() {
        // A gitignored file that ALSO has a git status keeps its status color —
        // "changed" is the stronger signal, so dim does not apply.
        let (icon, name) = row_colors(false, false, true, Some(("M", theme::PALETTE_YELLOW())));
        assert_eq!(icon, theme::PALETTE_YELLOW());
        assert_eq!(name, theme::PALETTE_YELLOW());
        assert_ne!(name, theme::FG_DIM());

        // A gitignored directory that contains changes stays green, not dim.
        let (dir_icon, dir_name) = row_colors(true, true, true, None);
        assert_eq!(dir_icon, theme::PALETTE_GREEN());
        assert_eq!(dir_name, theme::PALETTE_GREEN());
        assert_ne!(dir_name, theme::FG_DIM());
    }

    #[test]
    fn missing_binary_sets_flag_and_does_not_panic() {
        let mut code_missing = false;
        // a program name that certainly does not exist on PATH
        let ok = try_launch_editor("definitely-not-a-real-binary-xyz", Path::new("/tmp/x"));
        if !ok {
            code_missing = true;
        }
        assert!(!ok);
        assert!(code_missing);
    }

    /// Painted geometry of one row's glyphs, extracted from emitted shapes.
    struct RowGeom {
        /// Left ink x of the name galley.
        name_left: f32,
        /// Right-most ink x among single-glyph shapes painted to the LEFT of the
        /// name (chevron + icon column). `None` if no such glyph (e.g. tofu).
        icon_right: Option<f32>,
    }

    /// Renders `build` headlessly through the SAME container stack the app uses
    /// (Area with a transform layer + a bounded child Ui) and with the SAME
    /// registered fonts, then extracts, for each painted name in `names`, its
    /// row geometry. GPU-free: `Context::run` lays out and emits paint `Shape`s
    /// without a renderer. Using the real fonts means the icon glyphs have their
    /// true paint extents (not tofu), so overlap is measured faithfully.
    fn row_geoms(names: &[&str], build: impl Fn(&mut egui::Ui)) -> Vec<RowGeom> {
        use egui::epaint::Shape;

        let ctx = egui::Context::default();
        ctx.set_fonts(crate::app::configure_fonts());
        let mut out = ctx.run(egui::RawInput::default(), |_| {});
        for _ in 0..4 {
            out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::Area::new(egui::Id::new("test_panel"))
                    .fixed_pos(egui::Pos2::new(100.0, 100.0))
                    .constrain(false)
                    .interactable(false)
                    .show(ctx, |ui| {
                        let t = egui::emath::TSTransform::from_translation(egui::Vec2::new(50.0, 50.0));
                        ui.ctx().set_transform_layer(ui.layer_id(), t);
                        let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(280.0, 400.0), egui::Sense::hover());
                        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(&mut child_ui, |ui| build(ui));
                    });
            });
        }

        names
            .iter()
            .map(|name| {
                let name_left = out
                    .shapes
                    .iter()
                    .find_map(|cs| match &cs.shape {
                        Shape::Text(t) if t.galley.job.text == *name => Some(t.pos.x),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("name galley '{name}' was painted"));

                let mut icon_right = f32::MIN;
                for cs in &out.shapes {
                    if let Shape::Text(t) = &cs.shape {
                        let text = t.galley.job.text.as_str();
                        let ink = t.galley.mesh_bounds.translate(t.pos.to_vec2());
                        // single-glyph shapes (chevron, icon) painted left of the name
                        if text.chars().count() == 1 && !names.contains(&text) && ink.max.x <= name_left + 1.0 {
                            icon_right = icon_right.max(ink.max.x);
                        }
                    }
                }
                RowGeom {
                    name_left,
                    icon_right: (icon_right > f32::MIN).then_some(icon_right),
                }
            })
            .collect()
    }

    #[test]
    fn render_row_layout_orders_and_indents_by_depth() {
        use horizon_core::file_tree::FileNode;

        // depth 0 file and a depth-2 file via two nested expanded dirs.
        let deep_file = FileNode {
            name: "methods.csv".to_string(),
            path: std::path::PathBuf::from("/tmp/a/b/methods.csv"),
            is_dir: false,
            children: None,
            ignored: false,
        };
        let dir_b = FileNode {
            name: "bbb".to_string(),
            path: std::path::PathBuf::from("/tmp/a/b"),
            is_dir: true,
            children: Some(vec![deep_file]),
            ignored: false,
        };
        let dir_a = FileNode {
            name: "aaa".to_string(),
            path: std::path::PathBuf::from("/tmp/a"),
            is_dir: true,
            children: Some(vec![dir_b]),
            ignored: false,
        };
        let top_file = FileNode {
            name: "bootstrap".to_string(),
            path: std::path::PathBuf::from("/tmp/bootstrap"),
            is_dir: false,
            children: None,
            ignored: false,
        };

        let geoms = row_geoms(&["bootstrap", "methods.csv"], |ui| {
            let mut sink = RenderSink {
                action: &mut None,
                selection: &mut None,
                row_hits: &mut Vec::new(),
            };
            render_nodes(ui, &[dir_a.clone(), top_file.clone()], 0, None, &mut sink, None);
        });
        let depth0 = &geoms[0];
        let depth2 = &geoms[1];

        // No overlap: name starts at/after the icon ink at both depths.
        if let Some(r) = depth0.icon_right {
            assert!(
                depth0.name_left >= r,
                "depth-0 name must start right of icon: name={} icon={r}",
                depth0.name_left
            );
        }
        if let Some(r) = depth2.icon_right {
            assert!(
                depth2.name_left >= r,
                "depth-2 name must start right of icon: name={} icon={r}",
                depth2.name_left
            );
        }
        // Real per-depth indentation: depth-2 name is further right than depth-0
        // by about 2 * INDENT_PER_DEPTH (tree is not flat).
        let delta = depth2.name_left - depth0.name_left;
        assert!(
            delta >= 2.0 * INDENT_PER_DEPTH - 1.0,
            "depth-2 row must indent past depth-0 by ~2 levels: delta={delta}"
        );
    }

    #[test]
    fn render_changed_row_name_does_not_overlap_icon() {
        use horizon_core::file_tree::ChangedTreeNode;

        let node = ChangedTreeNode {
            name: "bootstrap".to_string(),
            abs_path: std::path::PathBuf::from("/tmp/bootstrap"),
            is_dir: false,
            status: Some(FileStatus::Modified),
            children: Vec::new(),
        };
        let geoms = row_geoms(&["bootstrap"], |ui| {
            render_changed_row(ui, &node, 2, false);
        });
        if let Some(r) = geoms[0].icon_right {
            assert!(
                geoms[0].name_left >= r,
                "changed-row name must start right of the icon ink: name_left={} icon_right={r}",
                geoms[0].name_left
            );
        }
    }
}
