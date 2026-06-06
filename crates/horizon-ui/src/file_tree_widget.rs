//! `FileExplorer` panel rendering helpers.
//!
//! Provides [`file_type_icon`] (path -> Symbols Nerd Font glyph) and
//! [`FileExplorerView`], the egui widget that renders the lazy, gitignore-aware
//! project file tree with live git-status decorations. Mirrors the structure of
//! `git_changes_widget::GitChangesView` (`new(panel)` + `show(ui, is_focused)`).

use std::path::Path;
use std::process::Command;

use egui::{Align, Color32, CornerRadius, FontId, Layout, Rect, RichText, ScrollArea, Vec2};
use horizon_core::file_tree::{ChangedTreeNode, FileNode, FileTreeState, changed_file_tree, status_for_path};
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
const INDENT_PER_DEPTH: f32 = 14.0;
const BASE_INDENT: f32 = 8.0;
/// Right-edge space reserved for the status letter so long names truncate
/// with `…` instead of running underneath it.
const LETTER_RESERVE: f32 = 26.0;
/// Right-edge padding for rows without a status letter.
const PLAIN_RESERVE: f32 = 10.0;

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
}

impl<'a> FileExplorerView<'a> {
    pub fn new(panel: &'a mut Panel) -> Self {
        Self { panel }
    }

    /// Renders the file explorer panel. Returns `true` if the pointer is over
    /// the panel (for focus tracking), mirroring `GitChangesView`.
    pub fn show(&mut self, ui: &mut egui::Ui, _is_focused: bool) -> bool {
        let clicked = ui.rect_contains_pointer(ui.max_rect());

        let panel_id = self.panel.id.0;
        let Some(state) = self.panel.content.file_explorer_mut() else {
            return clicked;
        };

        if !state.loaded {
            state.reload_root();
        }

        let mut refresh = false;
        // Own the toggle value before the immutable borrows below; the header
        // flips it in place and we persist it back into `state` afterwards.
        let mut show_only = state.show_only_changes;
        render_header(ui, state, &mut refresh, &mut show_only);
        state.show_only_changes = show_only;

        // Clone the status Arc out before the recursive render so the immutable
        // borrow of `state.git_status` does not conflict with the mutations we
        // apply afterwards (mirrors GitChangesView cloning `viewer.status`).
        let status = state.git_status.clone();
        let mut action: Option<TreeAction> = None;

        // Bound the scroll area to the panel body so a tall tree clips and
        // scrolls inside the panel instead of painting over neighbouring
        // panels. Reserve footer space only when the footer is shown.
        let footer_h = if state.code_missing { FOOTER_HEIGHT } else { 0.0 };
        let max_h = scroll_viewport_height(ui.max_rect().bottom(), ui.cursor().top(), footer_h);

        let scroll_output = ScrollArea::vertical()
            .max_height(max_h)
            .auto_shrink([false, false])
            .id_salt(("file_explorer_tree", panel_id))
            .show(ui, |ui| {
                ui.add_space(2.0);
                if show_only {
                    render_changes_only(ui, status.as_deref(), &mut action);
                } else {
                    render_nodes(ui, &state.roots, 0, status.as_deref(), &mut action);
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

        if refresh {
            state.reload_root();
        }

        match action {
            Some(TreeAction::Expand(path)) => expand_node(&mut state.roots, &path),
            Some(TreeAction::Collapse(path)) => collapse_node(&mut state.roots, &path),
            Some(TreeAction::Open(path)) => open_in_vscode(&path, &mut state.code_missing),
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

fn render_nodes(
    ui: &mut egui::Ui,
    nodes: &[FileNode],
    depth: usize,
    status: Option<&GitStatus>,
    action: &mut Option<TreeAction>,
) {
    for node in nodes {
        render_node(ui, node, depth, status, action);
    }
}

fn render_node(
    ui: &mut egui::Ui,
    node: &FileNode,
    depth: usize,
    status: Option<&GitStatus>,
    action: &mut Option<TreeAction>,
) {
    let is_dir = node.is_dir;
    let is_open = node.children.is_some();

    if render_row(ui, node, depth, status) {
        // First interaction in the frame wins; later ones are ignored.
        if action.is_none() {
            *action = Some(if is_dir {
                if is_open {
                    TreeAction::Collapse(node.path.clone())
                } else {
                    TreeAction::Expand(node.path.clone())
                }
            } else {
                TreeAction::Open(node.path.clone())
            });
        }
    }

    if let Some(children) = &node.children {
        render_nodes(ui, children, depth + 1, status, action);
    }
}

/// Renders one tree row. Returns `true` if the row should toggle (folder) or
/// open (file) — i.e. clicked for a folder, double-clicked for a file.
fn render_row(ui: &mut egui::Ui, node: &FileNode, depth: usize, status: Option<&GitStatus>) -> bool {
    let row_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), ROW_HEIGHT));
    let response = ui.allocate_rect(row_rect, egui::Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(row_rect, CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }

    let decoration = status.and_then(|s| status_decoration(status_for_path(s, &node.path)));
    let name_color = decoration.map_or_else(theme::FG, |(_, color)| color);

    #[allow(clippy::cast_precision_loss)]
    let indent = BASE_INDENT + depth as f32 * INDENT_PER_DEPTH;

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(row_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(indent);

            if node.is_dir {
                let caret = if node.children.is_some() {
                    "\u{25bc}"
                } else {
                    "\u{25b6}"
                };
                ui.label(RichText::new(caret).size(7.0).color(theme::FG_DIM()));
                ui.add_space(4.0);
            } else {
                // Align files with folders that have a caret in front.
                ui.add_space(11.0);
            }

            ui.label(
                RichText::new(file_type_icon(&node.path, node.is_dir))
                    .font(FontId::proportional(ICON_FONT_SIZE))
                    .color(if node.is_dir { theme::ACCENT() } else { name_color }),
            );
            ui.add_space(6.0);

            // Cap the label at the letter zone so long names truncate with
            // `…` instead of overflowing the panel / running under the letter.
            let reserve = if decoration.is_some() {
                LETTER_RESERVE
            } else {
                PLAIN_RESERVE
            };
            ui.set_max_width((row_rect.width() - reserve).max(20.0));
            ui.add(
                egui::Label::new(
                    RichText::new(&node.name)
                        .font(FontId::proportional(ROW_FONT_SIZE))
                        .color(if node.is_dir { theme::FG_SOFT() } else { name_color }),
                )
                .truncate(),
            );
        },
    );

    if let Some((letter, color)) = decoration {
        paint_status_letter(ui, row_rect, letter, color);
    }

    if node.is_dir {
        response.clicked()
    } else {
        response.double_clicked()
    }
}

/// Renders the grouped "only uncommitted files" tree. With no git snapshot,
/// shows a dim "Sem reposit\u{f3}rio git" message; with an empty change set,
/// shows "Nada para commitar". Changed files are nested under their parent
/// folders (always expanded, single-child chains compacted VSCode-style).
/// Double-clicking a file row collects a [`TreeAction::Open`].
fn render_changes_only(ui: &mut egui::Ui, status: Option<&GitStatus>, action: &mut Option<TreeAction>) {
    let Some(status) = status else {
        render_centered_dim(ui, "Sem reposit\u{f3}rio git");
        return;
    };

    let tree = changed_file_tree(status);
    if tree.is_empty() {
        render_centered_dim(ui, "Nada para commitar");
        return;
    }

    render_changed_nodes(ui, &tree, 0, action);
}

fn render_changed_nodes(ui: &mut egui::Ui, nodes: &[ChangedTreeNode], depth: usize, action: &mut Option<TreeAction>) {
    for node in nodes {
        if render_changed_row(ui, node, depth) && action.is_none() {
            *action = Some(TreeAction::Open(node.abs_path.clone()));
        }
        render_changed_nodes(ui, &node.children, depth + 1, action);
    }
}

/// Renders one row of the grouped uncommitted-files tree: folders show an
/// open-folder icon with a neutral name; files show their type icon, the name
/// colored by status (truncated with `…` when too wide), and a fixed
/// right-aligned status letter. Returns `true` when a file row is
/// double-clicked (open in editor).
fn render_changed_row(ui: &mut egui::Ui, node: &ChangedTreeNode, depth: usize) -> bool {
    let row_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), ROW_HEIGHT));
    let response = ui.allocate_rect(row_rect, egui::Sense::click());

    if response.hovered() {
        ui.painter()
            .rect_filled(row_rect, CornerRadius::ZERO, theme::alpha(theme::FG(), 6));
    }

    let decoration = status_decoration(node.status);
    let name_color = decoration.map_or_else(theme::FG, |(_, color)| color);

    #[allow(clippy::cast_precision_loss)]
    let indent = BASE_INDENT + depth as f32 * INDENT_PER_DEPTH;

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(row_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add_space(indent);

            let (icon, icon_color, text_color) = if node.is_dir {
                ("\u{f07c}", theme::ACCENT(), theme::FG_SOFT()) // nf-fa-folder_open
            } else {
                (file_type_icon(&node.abs_path, false), name_color, name_color)
            };
            ui.label(
                RichText::new(icon)
                    .font(FontId::proportional(ICON_FONT_SIZE))
                    .color(icon_color),
            );
            ui.add_space(6.0);

            let reserve = if decoration.is_some() {
                LETTER_RESERVE
            } else {
                PLAIN_RESERVE
            };
            ui.set_max_width((row_rect.width() - reserve).max(20.0));
            ui.add(
                egui::Label::new(
                    RichText::new(&node.name)
                        .font(FontId::proportional(ROW_FONT_SIZE))
                        .color(text_color),
                )
                .truncate(),
            );
        },
    );

    if let Some((letter, color)) = decoration {
        paint_status_letter(ui, row_rect, letter, color);
    }

    !node.is_dir && response.double_clicked()
}

/// Paints the status letter right-aligned inside the row, independent of the
/// row's left-to-right content so truncated names can never collide with it.
fn paint_status_letter(ui: &egui::Ui, row_rect: Rect, letter: &str, color: Color32) {
    ui.painter().text(
        egui::Pos2::new(row_rect.max.x - 12.0, row_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        letter,
        FontId::monospace(11.0),
        color,
    );
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
}
