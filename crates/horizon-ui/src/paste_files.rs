//! Clipboard file-list reading for the File Explorer Ctrl+V paste action.
//!
//! Reads `text/uri-list` from the OS clipboard via `arboard` (already a
//! dependency). The read is done synchronously on the key press — acceptable
//! because `arboard` only interacts with the Wayland/X11 clipboard protocol,
//! not the disk. Marked as a concern in the module docs if it ever blocks.
//!
//! **Concern**: on some Wayland compositors, `arboard::get().file_list()` can
//! briefly block while the selection owner serialises its data. In practice
//! this is sub-millisecond for normal file-manager copies, but it is a
//! synchronous OS call inside the render loop. A future improvement would be
//! to move the read to a background thread and deliver the result one frame
//! later, similar to the image-paste flow.

use std::path::PathBuf;

/// Read the list of files currently on the OS clipboard (`text/uri-list`).
///
/// Returns `None` (no-op) when:
/// - the clipboard holds no file list (`ContentNotAvailable`),
/// - the list is empty,
/// - or `arboard::Clipboard` cannot be initialised.
///
/// Other `arboard` errors are logged at `debug` level and treated as no-op
/// so a transient clipboard error never panics or surfaces to the user.
#[must_use]
pub(crate) fn read_clipboard_file_list() -> Option<Vec<PathBuf>> {
    read_clipboard_file_list_impl()
}

#[cfg(target_os = "linux")]
fn read_clipboard_file_list_impl() -> Option<Vec<PathBuf>> {
    use arboard::Clipboard;

    let mut clipboard = match Clipboard::new() {
        Ok(cb) => cb,
        Err(err) => {
            tracing::debug!("clipboard unavailable for file-list paste: {err}");
            return None;
        }
    };
    match clipboard.get().file_list() {
        Ok(paths) if !paths.is_empty() => Some(paths),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => None,
        Err(err) => {
            tracing::debug!("clipboard file-list read failed: {err}");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn read_clipboard_file_list_impl() -> Option<Vec<PathBuf>> {
    None
}

/// Pure predicate: should a Ctrl+V keystroke trigger the explorer paste flow?
///
/// Returns `true` only when the focused panel is a `FileExplorer`. Kept as a
/// free function so it can be unit-tested without egui, mirroring
/// `search_shortcut_command` in the search dispatch.
#[must_use]
pub(crate) fn paste_targets_explorer(focused_kind: Option<horizon_core::PanelKind>) -> bool {
    matches!(focused_kind, Some(horizon_core::PanelKind::FileExplorer))
}

#[cfg(test)]
mod tests {
    use super::paste_targets_explorer;
    use horizon_core::PanelKind;

    #[test]
    fn paste_targets_explorer_true_for_file_explorer() {
        assert!(paste_targets_explorer(Some(PanelKind::FileExplorer)));
    }

    #[test]
    fn paste_targets_explorer_false_for_terminal() {
        assert!(!paste_targets_explorer(Some(PanelKind::Shell)));
    }

    #[test]
    fn paste_targets_explorer_false_for_editor() {
        assert!(!paste_targets_explorer(Some(PanelKind::Editor)));
    }

    #[test]
    fn paste_targets_explorer_false_for_none() {
        assert!(!paste_targets_explorer(None));
    }
}
