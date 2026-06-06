//! Right-click context menu for terminal panels.
//!
//! Offers Copy (enabled only when the terminal has a selection), Paste (clipboard
//! text into the PTY), and Paste Image (a clipboard image saved as a temp PNG
//! whose file path is pasted into the PTY — reusing [`crate::image_paste`]). The
//! menu reuses the exact copy/paste seams the keyboard handler uses so behavior
//! stays identical to Ctrl+C / Ctrl+V.

use std::path::Path;

use egui::{Button, RichText, Ui};

use crate::image_paste;
use crate::input;
use crate::primary_selection::PrimarySelection;
use crate::theme;

/// An action chosen from the terminal context menu, applied after the menu
/// closure returns so the menu does not borrow the terminal while it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TerminalContextAction {
    /// Copy the current selection to the clipboard (and primary selection).
    Copy,
    /// Paste the clipboard text into the terminal.
    Paste,
    /// Paste a clipboard image as a temp PNG file path into the terminal.
    PasteImage,
}

/// Whether the "Copy" menu item should be enabled: only when there is a
/// selection to copy. Pure so it can be unit-tested without egui.
pub(super) fn copy_enabled(has_selection: bool) -> bool {
    has_selection
}

/// Render the terminal context-menu items and return the chosen action, if any.
///
/// `has_selection` gates the Copy item. The Paste items are always offered (the
/// clipboard is consulted lazily when the action is applied), matching how the
/// keyboard paste path behaves.
pub(super) fn show_terminal_context_menu(ui: &mut Ui, has_selection: bool) -> Option<TerminalContextAction> {
    ui.set_min_width(160.0);
    let mut action = None;

    let copy = ui.add_enabled(
        copy_enabled(has_selection),
        Button::new(RichText::new("Copy").size(12.0).color(theme::FG_SOFT())).frame(false),
    );
    if copy.clicked() {
        action = Some(TerminalContextAction::Copy);
        ui.close();
    }

    if ui
        .add(Button::new(RichText::new("Paste").size(12.0).color(theme::FG_SOFT())).frame(false))
        .clicked()
    {
        action = Some(TerminalContextAction::Paste);
        ui.close();
    }

    if ui
        .add(Button::new(RichText::new("Paste Image").size(12.0).color(theme::FG_SOFT())).frame(false))
        .clicked()
    {
        action = Some(TerminalContextAction::PasteImage);
        ui.close();
    }

    action
}

/// Apply a context-menu `action` to `panel`. Mirrors the keyboard copy/paste
/// seams in [`super::input`] so menu and shortcut behavior stay identical.
pub(super) fn apply_terminal_context_action(
    ui: &egui::Ui,
    panel: &mut horizon_core::Panel,
    primary_selection: &PrimarySelection,
    pasted_dir: &Path,
    action: &TerminalContextAction,
) {
    let Some(terminal) = panel.terminal() else {
        return;
    };
    let mode = terminal.mode();

    match action {
        TerminalContextAction::Copy => {
            if let Some(text) = terminal.selection_to_string() {
                primary_selection.copy(&text);
                ui.ctx().copy_text(text);
                terminal.clear_selection();
            }
        }
        TerminalContextAction::Paste => {
            if let Some(text) = read_clipboard_text(ui) {
                terminal.clear_selection();
                let bytes = input::paste_bytes(&text, mode, true);
                terminal.write_input(&bytes);
            }
        }
        TerminalContextAction::PasteImage => {
            if let Some(path) = image_paste::read_clipboard_image_to_png(pasted_dir) {
                let path_text = path.to_string_lossy().into_owned();
                terminal.clear_selection();
                let bytes = input::paste_bytes(&path_text, mode, true);
                terminal.write_input(&bytes);
            } else {
                tracing::debug!("paste image requested but no clipboard image present");
            }
        }
    }
}

/// Read the current clipboard text via egui's clipboard. Returns `None` when the
/// clipboard holds no text (so a Paste with an empty/non-text clipboard is a
/// no-op rather than writing an empty string into the PTY).
fn read_clipboard_text(ui: &egui::Ui) -> Option<String> {
    let _ = ui;
    read_clipboard_text_impl()
}

#[cfg(target_os = "linux")]
fn read_clipboard_text_impl() -> Option<String> {
    use arboard::Clipboard;

    let mut clipboard = match Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            tracing::debug!("clipboard unavailable for paste: {error}");
            return None;
        }
    };
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Some(text),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => None,
        Err(error) => {
            tracing::debug!("clipboard text read failed: {error}");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn read_clipboard_text_impl() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::{TerminalContextAction, copy_enabled};

    #[test]
    fn copy_is_disabled_without_a_selection() {
        assert!(!copy_enabled(false));
    }

    #[test]
    fn copy_is_enabled_with_a_selection() {
        assert!(copy_enabled(true));
    }

    #[test]
    fn context_actions_are_distinct() {
        assert_ne!(TerminalContextAction::Copy, TerminalContextAction::Paste);
        assert_ne!(TerminalContextAction::Paste, TerminalContextAction::PasteImage);
    }
}
