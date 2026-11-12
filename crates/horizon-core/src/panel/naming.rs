//! Validation of user-typed panel names.

/// Longest accepted panel name, in characters. A name travels in a titlebar, a sidebar row,
/// `horizon link list` and the send dialog, so it has to stay short enough to read there.
pub(super) const MAX_PANEL_NAME_CHARS: usize = 80;

/// The name to store for `input`: trimmed, or `None` when the input is blank, longer than
/// [`MAX_PANEL_NAME_CHARS`] or carries a control character (a pasted newline or escape).
pub(super) fn normalize_panel_name(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    let acceptable = !trimmed.is_empty()
        && trimmed.chars().count() <= MAX_PANEL_NAME_CHARS
        && !trimmed.chars().any(char::is_control);
    acceptable.then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_trims_and_rejects_blank_and_overlong() {
        assert_eq!(normalize_panel_name("  backend  "), Some("backend"));
        assert_eq!(normalize_panel_name(""), None);
        assert_eq!(normalize_panel_name(" \t "), None);
        let longest = "x".repeat(MAX_PANEL_NAME_CHARS);
        assert_eq!(normalize_panel_name(&longest), Some(longest.as_str()));
        assert_eq!(normalize_panel_name(&format!("{longest}x")), None);
    }

    #[test]
    fn name_length_counts_characters_not_bytes() {
        let wide = "é".repeat(MAX_PANEL_NAME_CHARS);
        assert_eq!(normalize_panel_name(&wide), Some(wide.as_str()));
    }

    #[test]
    fn names_with_control_characters_are_rejected() {
        assert_eq!(normalize_panel_name("a\nb"), None);
        assert_eq!(normalize_panel_name("a\u{1b}[31mb"), None);
        assert_eq!(normalize_panel_name("a\tb"), None);
    }
}
