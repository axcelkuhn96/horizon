//! Synchronous file-content search engine ("search in files").
//!
//! This is the pure grep core powering a VSCode-style "search in files"
//! feature. It walks a directory tree (mirroring the File Explorer's
//! visibility rules), scans each text file line by line, and reports matches
//! with 1-based line numbers and byte spans. There is no UI and no threading
//! here — a later task wires this engine to a background thread and the egui
//! panel.
//!
//! Visibility mirrors [`crate::file_tree`]: the same [`HARD_SKIP`] directories
//! (`.git`, `node_modules`, `target`) are always pruned, gitignored files ARE
//! searched (`git_ignore(false)`), and dotfiles are visible (`hidden(false)`).
//!
//! ## Assumptions
//! - **ASCII case-folding** for the substring matcher. Case-insensitive
//!   substring matching lowercases ASCII bytes only; non-ASCII bytes compare
//!   byte-for-byte. Spans are always reported against the ORIGINAL line bytes.
//!   For full Unicode case-insensitivity, use regex mode with `case_sensitive`
//!   left `false` (the `regex` crate handles Unicode case folding).
//! - **Non-UTF8 files are skipped** entirely (we read the file as UTF-8 and
//!   skip on failure).

use std::fmt;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};

/// Directories that are never searched, matching the File Explorer tree.
const HARD_SKIP: [&str; 3] = [".git", "node_modules", "target"];

/// Bytes read from the head of a file to sniff for binary content.
const BINARY_SNIFF_BYTES: usize = 1024;

/// Options controlling a [`search_files`] invocation.
#[derive(Clone, Debug)]
pub struct FileSearchOptions {
    /// When `true`, matching is case-sensitive. Defaults to `false`.
    pub case_sensitive: bool,
    /// When `true`, `query` is treated as a regular expression; otherwise it is
    /// a literal substring. Defaults to `false`.
    pub regex: bool,
    /// Maximum TOTAL number of matches to collect across all files before the
    /// search stops early and reports `truncated = true`. Defaults to `1000`.
    pub max_results: usize,
    /// Files larger than this many bytes are skipped. Defaults to 2 MiB.
    pub max_file_bytes: u64,
}

impl Default for FileSearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            regex: false,
            max_results: 1000,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}

/// A single match within a line of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// 1-based line number of the match.
    pub line_number: usize,
    /// Full text of the line containing the match (without the line ending).
    pub line_text: String,
    /// Byte range `(start, end)` of the match within `line_text`.
    pub span: (usize, usize),
}

/// All matches found in a single file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearchResult {
    /// Absolute or root-relative path of the file (as produced by the walk).
    pub path: PathBuf,
    /// Matches in file order (by line, then by position within the line).
    pub matches: Vec<SearchMatch>,
}

/// The full outcome of a [`search_files`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOutcome {
    /// Per-file results, sorted by path for deterministic output.
    pub results: Vec<FileSearchResult>,
    /// `true` when the search stopped early after hitting `max_results`.
    pub truncated: bool,
}

/// Errors returned by [`search_files`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    /// The supplied regex failed to compile. Carries the compiler message.
    InvalidRegex(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegex(msg) => write!(f, "invalid regex: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// Compiled query, built once and reused for every line.
enum Matcher {
    /// Literal substring search. `needle` is the (possibly lowercased) pattern.
    Substring { needle: String, case_sensitive: bool },
    /// Regular-expression search.
    Regex(Regex),
}

impl Matcher {
    /// Build a matcher from the query and options, or fail on bad regex.
    fn build(query: &str, opts: &FileSearchOptions) -> Result<Self, SearchError> {
        if opts.regex {
            let re = RegexBuilder::new(query)
                .case_insensitive(!opts.case_sensitive)
                .build()
                .map_err(|err| SearchError::InvalidRegex(err.to_string()))?;
            Ok(Self::Regex(re))
        } else if opts.case_sensitive {
            Ok(Self::Substring {
                needle: query.to_string(),
                case_sensitive: true,
            })
        } else {
            Ok(Self::Substring {
                needle: ascii_lowercase(query),
                case_sensitive: false,
            })
        }
    }

    /// Collect all non-overlapping match spans within a single line.
    fn find_spans(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Substring { needle, case_sensitive } => {
                if needle.is_empty() {
                    return Vec::new();
                }
                // Case-sensitive: search the line directly (no clone). Otherwise
                // build an ASCII-lowercased copy. ASCII lowercasing never changes
                // byte length, so offsets into `lowered` line up exactly with the
                // original line bytes (and stay on UTF-8 char boundaries).
                let lowered;
                let haystack: &str = if *case_sensitive {
                    line
                } else {
                    lowered = ascii_lowercase(line);
                    &lowered
                };
                let mut spans = Vec::new();
                let mut from = 0usize;
                while let Some(rel) = haystack[from..].find(needle.as_str()) {
                    let start = from + rel;
                    let end = start + needle.len();
                    spans.push((start, end));
                    from = end;
                }
                spans
            }
            Self::Regex(re) => re
                .find_iter(line)
                .map(|m| (m.start(), m.end()))
                .filter(|(s, e)| s != e) // ignore zero-width matches
                .collect(),
        }
    }
}

/// Lowercase only ASCII letters, leaving every other byte unchanged. This keeps
/// the output byte length identical to the input so spans remain valid offsets
/// into the original string.
fn ascii_lowercase(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// Search every text file under `root` for `query`.
///
/// Walks `root` with [`ignore::WalkBuilder`], skipping [`HARD_SKIP`]
/// directories, binary files (NUL byte in the first ~1 KiB), oversized files
/// (`> opts.max_file_bytes`), and non-UTF8 files. Results are sorted by path.
///
/// # Errors
/// Returns [`SearchError::InvalidRegex`] when `opts.regex` is set and `query`
/// is not a valid regular expression. Per-file IO errors are never propagated;
/// such files are simply skipped.
pub fn search_files(
    root: &Path,
    query: &str,
    opts: &FileSearchOptions,
) -> Result<SearchOutcome, SearchError> {
    if query.is_empty() {
        return Ok(SearchOutcome {
            results: Vec::new(),
            truncated: false,
        });
    }

    let matcher = Matcher::build(query, opts)?;

    let walker = WalkBuilder::new(root)
        .hidden(false) // show dotfiles, mirroring the tree
        .git_ignore(false) // search gitignored files too
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .is_none_or(|name| !HARD_SKIP.contains(&name))
        })
        .build();

    let mut results: Vec<FileSearchResult> = Vec::new();
    let mut total_matches: usize = 0;

    // Over-scan by one: collect up to `max_results + 1` matches so we can
    // distinguish "exactly at the cap with nothing beyond" (NOT truncated) from
    // "genuinely overflowed" (truncated). The +1 is trimmed off below.
    let budget = opts.max_results.saturating_add(1);

    for entry in walker {
        if total_matches >= budget {
            break;
        }

        let Ok(entry) = entry else {
            continue; // permission denied etc.: skip, never panic
        };

        // Only search regular files.
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let path = entry.path();

        // NOTE: this stat + binary-sniff + read path opens the file up to three
        // times. Left as-is for clarity; the bounded sizes keep it cheap.
        if !file_is_searchable(path, opts.max_file_bytes) {
            continue;
        }

        let Ok(contents) = std::fs::read_to_string(path) else {
            continue; // non-UTF8 or IO error: skip
        };

        let remaining = budget.saturating_sub(total_matches);
        let found = scan_contents(&contents, &matcher, remaining);

        if !found.is_empty() {
            total_matches += found.len();
            results.push(FileSearchResult {
                path: path.to_path_buf(),
                matches: found,
            });
        }
    }

    // WalkBuilder order is not guaranteed stable; sort for determinism. Trim any
    // overflow (matches beyond `max_results`) AFTER sorting, so the omitted
    // matches are the ones that sort last by path.
    results.sort_by(|a, b| a.path.cmp(&b.path));
    let truncated = total_matches > opts.max_results;
    if truncated {
        trim_to(&mut results, opts.max_results);
    }

    Ok(SearchOutcome { results, truncated })
}

/// Returns `true` when the file at `path` is worth scanning: it exists, is no
/// larger than `max_file_bytes`, and is not binary (no NUL in the first
/// [`BINARY_SNIFF_BYTES`] bytes).
fn file_is_searchable(path: &Path, max_file_bytes: u64) -> bool {
    // Reject oversized files using stat metadata when available.
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > max_file_bytes
    {
        return false;
    }
    !looks_binary(path)
}

/// Sniff up to [`BINARY_SNIFF_BYTES`] from the head of the file; treat the
/// presence of a NUL byte as "binary". On read error, report binary (skip).
fn looks_binary(path: &Path) -> bool {
    use std::io::Read;

    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    let mut head = [0u8; BINARY_SNIFF_BYTES];
    let mut handle = file.take(BINARY_SNIFF_BYTES as u64);
    match handle.read(&mut head) {
        Ok(n) => head[..n].contains(&0u8),
        Err(_) => true,
    }
}

/// Scan already-loaded file `contents` line by line, collecting at most
/// `remaining` matches (in line order, then position within the line).
fn scan_contents(contents: &str, matcher: &Matcher, remaining: usize) -> Vec<SearchMatch> {
    let mut found = Vec::new();
    if remaining == 0 {
        return found;
    }

    for (idx, line) in contents.lines().enumerate() {
        for (start, end) in matcher.find_spans(line) {
            found.push(SearchMatch {
                line_number: idx + 1, // 1-based
                line_text: line.to_string(),
                span: (start, end),
            });
            if found.len() >= remaining {
                return found;
            }
        }
    }

    found
}

/// Trim `results` so the total number of matches across all files is at most
/// `cap`, dropping matches (and then empty files) from the tail. Callers must
/// sort `results` by path first so the dropped matches are the ones that sort
/// last.
fn trim_to(results: &mut Vec<FileSearchResult>, cap: usize) {
    let mut kept = 0usize;
    for result in results.iter_mut() {
        if kept >= cap {
            result.matches.clear();
            continue;
        }
        let room = cap - kept;
        if result.matches.len() > room {
            result.matches.truncate(room);
        }
        kept += result.matches.len();
    }
    results.retain(|r| !r.matches.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write `contents` to `dir/rel`, creating parent directories as needed.
    fn write(dir: &Path, rel: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write file");
        path
    }

    fn opts() -> FileSearchOptions {
        FileSearchOptions::default()
    }

    #[test]
    fn empty_query_returns_empty_outcome() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"hello world\n");

        let outcome = search_files(dir.path(), "", &opts()).expect("search");
        assert!(outcome.results.is_empty());
        assert!(!outcome.truncated);
    }

    #[test]
    fn substring_hit_reports_path_line_and_span() {
        let dir = TempDir::new().expect("tempdir");
        let file = write(dir.path(), "a.txt", b"first line\nhello needle here\nlast\n");

        let outcome = search_files(dir.path(), "needle", &opts()).expect("search");
        assert_eq!(outcome.results.len(), 1);
        let result = &outcome.results[0];
        assert_eq!(result.path, file);
        assert_eq!(result.matches.len(), 1);
        let m = &result.matches[0];
        assert_eq!(m.line_number, 2);
        assert_eq!(m.line_text, "hello needle here");
        // "hello " is 6 bytes; "needle" is 6 bytes.
        assert_eq!(m.span, (6, 12));
        assert_eq!(&m.line_text[m.span.0..m.span.1], "needle");
        assert!(!outcome.truncated);
    }

    #[test]
    fn case_insensitive_by_default_but_case_sensitive_opt_in() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"Hello NEEDLE here\n");

        // Default: case-insensitive — lowercase query matches uppercase text.
        let insensitive = search_files(dir.path(), "needle", &opts()).expect("search");
        assert_eq!(insensitive.results.len(), 1);
        let m = &insensitive.results[0].matches[0];
        assert_eq!(&m.line_text[m.span.0..m.span.1], "NEEDLE");

        // case_sensitive = true: lowercase query no longer matches.
        let sensitive_opts = FileSearchOptions {
            case_sensitive: true,
            ..opts()
        };
        let sensitive = search_files(dir.path(), "needle", &sensitive_opts).expect("search");
        assert!(sensitive.results.is_empty());

        // case_sensitive = true with exact-case query matches.
        let exact = search_files(dir.path(), "NEEDLE", &sensitive_opts).expect("search");
        assert_eq!(exact.results.len(), 1);
    }

    #[test]
    fn multiple_matches_on_one_line_have_distinct_spans() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"ab ab ab\n");

        let outcome = search_files(dir.path(), "ab", &opts()).expect("search");
        assert_eq!(outcome.results.len(), 1);
        let matches = &outcome.results[0].matches;
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].span, (0, 2));
        assert_eq!(matches[1].span, (3, 5));
        assert_eq!(matches[2].span, (6, 8));
        for m in matches {
            assert_eq!(m.line_number, 1);
            assert_eq!(&m.line_text[m.span.0..m.span.1], "ab");
        }
    }

    #[test]
    fn regex_mode_matches_and_reports_span() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"id=42 and id=7\n");

        let regex_opts = FileSearchOptions {
            regex: true,
            ..opts()
        };
        let outcome = search_files(dir.path(), r"id=\d+", &regex_opts).expect("search");
        assert_eq!(outcome.results.len(), 1);
        let matches = &outcome.results[0].matches;
        assert_eq!(matches.len(), 2);
        assert_eq!(&matches[0].line_text[matches[0].span.0..matches[0].span.1], "id=42");
        assert_eq!(&matches[1].line_text[matches[1].span.0..matches[1].span.1], "id=7");
    }

    #[test]
    fn invalid_regex_returns_error_without_panicking() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"anything\n");

        let regex_opts = FileSearchOptions {
            regex: true,
            ..opts()
        };
        let err = search_files(dir.path(), "(unclosed", &regex_opts).expect_err("should error");
        match err {
            SearchError::InvalidRegex(msg) => assert!(!msg.is_empty()),
        }
    }

    #[test]
    fn binary_file_with_nul_is_skipped() {
        let dir = TempDir::new().expect("tempdir");
        // Contains the query "needle" but also a NUL byte → treated as binary.
        write(dir.path(), "bin.dat", b"needle\x00more needle\n");
        // A plain text control file that should still match.
        write(dir.path(), "text.txt", b"needle\n");

        let outcome = search_files(dir.path(), "needle", &opts()).expect("search");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].path, dir.path().join("text.txt"));
    }

    #[test]
    fn file_over_max_bytes_is_skipped() {
        let dir = TempDir::new().expect("tempdir");
        let big = vec![b'x'; 4096];
        let mut big_with_match = big.clone();
        big_with_match.extend_from_slice(b"\nneedle\n");
        write(dir.path(), "big.txt", &big_with_match);
        write(dir.path(), "small.txt", b"needle\n");

        let limited = FileSearchOptions {
            max_file_bytes: 1024,
            ..opts()
        };
        let outcome = search_files(dir.path(), "needle", &limited).expect("search");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].path, dir.path().join("small.txt"));
    }

    #[test]
    fn hard_skip_dir_node_modules_is_not_searched() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "node_modules/pkg/index.js", b"needle\n");
        write(dir.path(), "src/main.rs", b"needle\n");

        let outcome = search_files(dir.path(), "needle", &opts()).expect("search");
        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].path, dir.path().join("src/main.rs"));
    }

    #[test]
    fn gitignored_file_is_still_searched() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), ".gitignore", b"ignored.txt\n");
        write(dir.path(), "ignored.txt", b"needle\n");

        let outcome = search_files(dir.path(), "needle", &opts()).expect("search");
        let paths: Vec<&PathBuf> = outcome.results.iter().map(|r| &r.path).collect();
        assert!(
            paths.contains(&&dir.path().join("ignored.txt")),
            "gitignored file should still be searched, got {paths:?}"
        );
    }

    #[test]
    fn max_results_truncates_and_sets_flag() {
        let dir = TempDir::new().expect("tempdir");
        // Three files, each with one match (sorted: a < b < c).
        write(dir.path(), "a.txt", b"needle\n");
        write(dir.path(), "b.txt", b"needle\n");
        write(dir.path(), "c.txt", b"needle\n");

        let capped = FileSearchOptions {
            max_results: 2,
            ..opts()
        };
        let outcome = search_files(dir.path(), "needle", &capped).expect("search");
        let total: usize = outcome.results.iter().map(|r| r.matches.len()).sum();
        assert_eq!(total, 2, "total matches must be capped at max_results");
        assert!(outcome.truncated, "truncated flag must be set");
    }

    #[test]
    fn truncation_counts_matches_within_a_single_file() {
        let dir = TempDir::new().expect("tempdir");
        // One file with 4 matches but cap of 2.
        write(dir.path(), "a.txt", b"needle\nneedle\nneedle\nneedle\n");

        let capped = FileSearchOptions {
            max_results: 2,
            ..opts()
        };
        let outcome = search_files(dir.path(), "needle", &capped).expect("search");
        let total: usize = outcome.results.iter().map(|r| r.matches.len()).sum();
        assert_eq!(total, 2);
        assert!(outcome.truncated);
    }

    #[test]
    fn exact_cap_boundary_is_not_truncated() {
        let dir = TempDir::new().expect("tempdir");
        // Exactly 2 matches total with nothing beyond the cap of 2.
        write(dir.path(), "a.txt", b"needle\nneedle\n");

        let capped = FileSearchOptions {
            max_results: 2,
            ..opts()
        };
        let outcome = search_files(dir.path(), "needle", &capped).expect("search");
        let total: usize = outcome.results.iter().map(|r| r.matches.len()).sum();
        assert_eq!(total, 2, "all matches up to the cap are returned");
        assert!(
            !outcome.truncated,
            "truncated must be false when nothing was actually omitted"
        );
    }

    #[test]
    fn ascii_query_against_non_ascii_line_keeps_char_boundaries() {
        let dir = TempDir::new().expect("tempdir");
        // "café" contains a multi-byte UTF-8 char before and after the match.
        write(dir.path(), "a.txt", "café NEEDLE café\n".as_bytes());

        let outcome = search_files(dir.path(), "NEEDLE", &opts()).expect("search");
        assert_eq!(outcome.results.len(), 1);
        let m = &outcome.results[0].matches[0];
        // Slicing with the reported span must land on UTF-8 char boundaries and
        // yield exactly the matched text.
        assert_eq!(&m.line_text[m.span.0..m.span.1], "NEEDLE");
    }

    #[test]
    fn search_error_implements_display_and_error() {
        let err = SearchError::InvalidRegex("boom".to_string());
        assert_eq!(err.to_string(), "invalid regex: boom");
        let _as_err: &dyn std::error::Error = &err;
    }
}
