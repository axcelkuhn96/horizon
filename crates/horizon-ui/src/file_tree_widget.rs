//! `FileExplorer` panel rendering helpers.
//!
//! This module currently provides [`file_type_icon`], which maps a path to a
//! Symbols Nerd Font glyph (Private Use Area codepoints). The
//! `FileExplorerView` rendering struct lands in a later task.

use std::path::Path;

/// Returns a Nerd Font glyph (Private Use Area) for a path. `is_dir` selects a
/// folder glyph. Unknown extensions fall back to a generic file glyph.
///
/// The returned glyph resolves against the Symbols Nerd Font registered in the
/// egui fallback stacks. Never panics.
// Consumed by `FileExplorerView` in a later task; the renderer that calls it
// has not landed yet.
#[allow(dead_code)]
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
}
