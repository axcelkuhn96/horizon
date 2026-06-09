use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Rgb};

use super::TerminalEventProxy;

pub(super) trait ColorLookup {
    fn lookup(&self, index: usize) -> Rgb;
}

impl ColorLookup for alacritty_terminal::term::color::Colors {
    fn lookup(&self, index: usize) -> Rgb {
        self[index].unwrap_or_else(|| default_terminal_rgb(index))
    }
}

pub(super) fn default_terminal_rgb(index: usize) -> Rgb {
    if let Some(color) = TERMINAL_BASE_COLORS.get(index) {
        return *color;
    }

    match index {
        16..=231 => {
            let idx = index - 16;
            let steps = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
            Rgb {
                r: steps[idx / 36],
                g: steps[(idx % 36) / 6],
                b: steps[idx % 6],
            }
        }
        232..=255 => {
            let value = 8 + ((index - 232) * 10);
            let value = u8::try_from(value).unwrap_or(u8::MAX);
            Rgb {
                r: value,
                g: value,
                b: value,
            }
        }
        256 | 267 => Rgb { r: 224, g: 230, b: 241 },
        257 | 268 => Rgb { r: 15, g: 19, b: 28 },
        258 => Rgb { r: 196, g: 223, b: 255 },
        _ => Rgb { r: 255, g: 255, b: 255 },
    }
}

pub(super) fn replay_terminal_bytes(term: &Arc<FairMutex<Term<TerminalEventProxy>>>, bytes: &[u8]) {
    let mut parser = ansi::Processor::<ansi::StdSyncHandler>::default();
    let mut terminal = term.lock();
    parser.advance(&mut *terminal, bytes);

    let reset_bytes = replay_mode_reset_bytes(*terminal.mode());
    if !reset_bytes.is_empty() {
        parser.advance(&mut *terminal, &reset_bytes);
    }
}

fn replay_mode_reset_bytes(mode: TermMode) -> Vec<u8> {
    let mut bytes = Vec::new();

    if mode.contains(TermMode::APP_CURSOR) {
        bytes.extend_from_slice(b"\x1b[?1l");
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        bytes.extend_from_slice(b"\x1b>");
    }
    if mode.intersects(TermMode::MOUSE_MODE) {
        bytes.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
    }
    if mode.contains(TermMode::FOCUS_IN_OUT) {
        bytes.extend_from_slice(b"\x1b[?1004l");
    }
    if mode.contains(TermMode::UTF8_MOUSE) {
        bytes.extend_from_slice(b"\x1b[?1005l");
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        bytes.extend_from_slice(b"\x1b[?1006l");
    }
    if mode.contains(TermMode::ALT_SCREEN) {
        bytes.extend_from_slice(b"\x1b[?1049l");
    }
    if !mode.contains(TermMode::SHOW_CURSOR) {
        bytes.extend_from_slice(b"\x1b[?25h");
    }

    bytes
}

pub(super) fn current_cwd_for_pid(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let target_pid = shell_pid_behind_wrapper(pid);
        std::fs::read_link(format!("/proc/{target_pid}/cwd"))
            .or_else(|_| std::fs::read_link(format!("/proc/{pid}/cwd")))
            .ok()
    }

    #[cfg(target_os = "macos")]
    {
        current_cwd_for_pid_via_lsof(pid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// The transcript wrapper (`script`) is the PTY's direct child but never changes
/// its own working directory, so reading its cwd would always report the panel's
/// original spawn directory. When `pid` is that wrapper, return the shell it
/// launched so callers read the shell's live cwd (which tracks `cd`). Returns
/// `pid` unchanged when it is not a `script` wrapper (transcript capture
/// disabled), in which case the PTY child already is the shell.
#[cfg(target_os = "linux")]
fn shell_pid_behind_wrapper(pid: u32) -> u32 {
    if proc_comm(pid).as_deref() != Some("script") {
        return pid;
    }
    first_child_pid(pid).unwrap_or(pid)
}

#[cfg(target_os = "linux")]
fn proc_comm(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_string())
}

#[cfg(target_os = "linux")]
fn first_child_pid(pid: u32) -> Option<u32> {
    let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
    parse_first_child_pid(&children)
}

/// Parse the space-separated PID list from `/proc/<pid>/task/<pid>/children`,
/// returning the first child (the shell launched by the wrapper).
#[cfg(any(target_os = "linux", test))]
fn parse_first_child_pid(children: &str) -> Option<u32> {
    children.split_whitespace().find_map(|token| token.parse().ok())
}

#[cfg(target_os = "macos")]
fn current_cwd_for_pid_via_lsof(pid: u32) -> Option<PathBuf> {
    let target_pid = deepest_child_pid(pid).unwrap_or(pid);
    lsof_cwd_for_pid(target_pid).or_else(|| lsof_cwd_for_pid(pid))
}

#[cfg(target_os = "macos")]
fn deepest_child_pid(mut pid: u32) -> Option<u32> {
    let mut saw_child = false;

    while let Some(child_pid) = direct_child_pid(pid) {
        saw_child = true;
        pid = child_pid;
    }

    saw_child.then_some(pid)
}

#[cfg(target_os = "macos")]
fn lsof_cwd_for_pid(pid: u32) -> Option<PathBuf> {
    let output = std::process::Command::new("lsof")
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_lsof_cwd(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(any(target_os = "macos", test))]
fn parse_lsof_cwd(output: &str) -> Option<PathBuf> {
    let mut in_cwd_entry = false;

    for line in output.lines() {
        if let Some(descriptor) = line.strip_prefix('f') {
            in_cwd_entry = descriptor == "cwd";
            continue;
        }
        if in_cwd_entry && let Some(path) = line.strip_prefix('n') {
            return Some(PathBuf::from(path));
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn direct_child_pid(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    parse_pgrep_pid(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(any(target_os = "macos", test))]
fn parse_pgrep_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| line.trim().parse().ok())
}

const TERMINAL_BASE_COLORS: [Rgb; 16] = [
    rgb(0x1d, 0x1f, 0x21),
    rgb(0xcc, 0x66, 0x66),
    rgb(0xb5, 0xbd, 0x68),
    rgb(0xf0, 0xc6, 0x74),
    rgb(0x81, 0xa2, 0xbe),
    rgb(0xb2, 0x94, 0xbb),
    rgb(0x8a, 0xbe, 0xb7),
    rgb(0xc5, 0xc8, 0xc6),
    rgb(0x66, 0x66, 0x66),
    rgb(0xd5, 0x4e, 0x53),
    rgb(0xb9, 0xca, 0x4a),
    rgb(0xe7, 0xc5, 0x47),
    rgb(0x7a, 0xa6, 0xda),
    rgb(0xc3, 0x97, 0xd8),
    rgb(0x70, 0xc0, 0xb1),
    rgb(0xea, 0xea, 0xea),
];

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

const URL_SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

pub(super) fn find_url_at_column(chars: &[char], col: usize) -> Option<String> {
    for scheme in URL_SCHEMES {
        let scheme_chars: Vec<char> = scheme.chars().collect();
        let scheme_len = scheme_chars.len();
        if chars.len() < scheme_len {
            continue;
        }
        for start in 0..=chars.len() - scheme_len {
            if chars[start..start + scheme_len] != *scheme_chars {
                continue;
            }
            let end = url_end_column(chars, start);
            if col >= start && col < end {
                return Some(chars[start..end].iter().collect());
            }
        }
    }
    None
}

fn url_end_column(chars: &[char], start: usize) -> usize {
    let mut end = chars.len();
    for (index, character) in chars.iter().enumerate().skip(start) {
        if character.is_whitespace() || matches!(character, '<' | '>' | '"' | '\'') {
            end = index;
            break;
        }
    }
    strip_trailing_url_chars(chars, start, end)
}

fn strip_trailing_url_chars(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start && matches!(chars[end - 1], '.' | ',' | ';' | '!' | '?') {
        end -= 1;
    }

    for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
        while end > start && chars[end - 1] == close && unmatched_closing_delimiter(chars, start, end, open, close) {
            end -= 1;
        }
    }

    end
}

fn unmatched_closing_delimiter(chars: &[char], start: usize, end: usize, open: char, close: char) -> bool {
    let mut balance = 0usize;
    for character in &chars[start..end] {
        if *character == open {
            balance += 1;
        } else if *character == close {
            if balance == 0 {
                return true;
            }
            balance -= 1;
        }
    }
    false
}

/// Returns `(bare_path, optional_line_number)` for the file-path token at
/// `col`, or `None` when the column does not land on a recognised path.
///
/// Recognised tokens (see [`is_path_like`] for the exact rules):
/// - Absolute paths starting with `/` or `~/`.
/// - Relative tokens that contain a `/` with a substantial alphabetic segment.
/// - Relative tokens that look like `name.ext` with a letter-bearing stem.
///
/// Plain words, all-numeric versions (`0.3.1`, `3.14`), short ratios (`3/5`),
/// and flag-shaped tokens (leading `-`) are rejected to limit false positives
/// and prevent argv flag smuggling into the editor launcher.
#[must_use]
pub(super) fn find_file_path_at_column(chars: &[char], col: usize) -> Option<(String, Option<u32>)> {
    let mut index = 0;
    while index < chars.len() {
        // A token starts at a path boundary (or the beginning of the line).
        let at_boundary = index == 0 || is_path_boundary(chars[index - 1]);
        if !at_boundary || chars[index].is_whitespace() {
            index += 1;
            continue;
        }

        let start = index;
        let end = path_end_column(chars, start);
        if end <= start {
            index += 1;
            continue;
        }

        let token = &chars[start..end];
        // Strip any trailing :N or :N:M suffix and parse the line number.
        let (path_chars, line_number) = strip_line_col_suffix(token);

        // Reject tokens shorter than 2 chars (bare `/` etc.).
        if path_chars.len() < 2 {
            index = end;
            continue;
        }

        // Check that the column lands inside the path portion.
        if col >= start && col < start + path_chars.len() {
            // Accept the token if it looks like a file path.
            let token_str: String = path_chars.iter().collect();
            if is_path_like(path_chars) {
                return Some((token_str, line_number));
            }
        }

        index = end;
    }
    None
}

/// Returns `true` when `chars` (the bare path without any line/col suffix)
/// looks like a file-system path worth opening.
///
/// Rules (in order):
/// - A leading `-` is rejected outright: such tokens could be smuggled to the
///   editor as a CLI flag (e.g. `--goto`, `-rf`).
/// - Starts with `/` (absolute) or `~` (home-relative): always accept.
/// - Slash rule: a relative token containing `/` is accepted only when the
///   whole token is at least 4 chars long AND at least one slash-separated
///   segment is >= 2 chars and contains an ASCII letter. This accepts
///   `crates/foo`, `src/lib`, `./src/lib` while rejecting `3/5`, `1/3`,
///   `a/b`, `N/A`.
/// - `name.ext` rule: accept when the extension after the last `.` is 1–8
///   ASCII alphanumeric chars AND the stem (everything before that last `.`)
///   contains at least one ASCII letter. This keeps `main.rs`, `Cargo.toml`,
///   `bar.rs` while rejecting all-numeric versions like `0.3.1`, `3.14`,
///   `1.5`. (`v1.2.3` has stem `v1.2` whose `v` is a letter, so it passes —
///   acceptable, low harm.)
fn is_path_like(chars: &[char]) -> bool {
    // Flag-shaped token: never treat as a path (argv smuggling guard).
    if chars[0] == '-' {
        return false;
    }
    // Absolute or home-relative: always accept.
    if chars[0] == '/' || chars[0] == '~' {
        return true;
    }
    // Slash rule: needs a substantial, letter-bearing segment.
    if chars.contains(&'/') {
        if chars.len() < 4 {
            return false;
        }
        let token: String = chars.iter().collect();
        let has_substantial_segment = token
            .split('/')
            .any(|seg| seg.chars().count() >= 2 && seg.chars().any(|c| c.is_ascii_alphabetic()));
        return has_substantial_segment;
    }
    // name.ext heuristic: letter-bearing stem + short alphanumeric extension.
    if let Some(dot_pos) = chars.iter().rposition(|&c| c == '.') {
        let stem = &chars[..dot_pos];
        let ext = &chars[dot_pos + 1..];
        let stem_has_letter = stem.iter().any(char::is_ascii_alphabetic);
        if stem_has_letter && !ext.is_empty() && ext.len() <= 8 && ext.iter().all(char::is_ascii_alphanumeric) {
            return true;
        }
    }
    false
}

/// Strip a trailing `:N` or `:N:M` suffix from `chars`, returning the path
/// slice and the parsed line number.
///
/// For `:N` → line = N.
/// For `:N:M` → line = N (first number), M is column (stripped but discarded).
///
/// The algorithm strips from the right: the innermost (rightmost) number is
/// stripped first (column or sole line), then the next (line). After at most
/// two numeric suffixes are stripped we stop, so the last value stored in
/// `last_stripped` at the time we stop is the line number.
fn strip_line_col_suffix(chars: &[char]) -> (&[char], Option<u32>) {
    let mut result = chars;
    let mut stripped_count = 0u32;
    let mut last_stripped: Option<u32> = None;

    loop {
        let Some(colon_pos) = result.iter().rposition(|character| *character == ':') else {
            return (result, last_stripped);
        };
        let suffix = &result[colon_pos + 1..];
        if !suffix.is_empty() && suffix.iter().all(char::is_ascii_digit) {
            let n: u32 = suffix.iter().fold(0u32, |acc, c| {
                acc.saturating_mul(10).saturating_add(*c as u32 - '0' as u32)
            });
            last_stripped = Some(n);
            stripped_count += 1;
            result = &result[..colon_pos];
            // Stop after stripping at most two numeric components (:line:col).
            if stripped_count >= 2 {
                return (result, last_stripped);
            }
        } else {
            return (result, last_stripped);
        }
    }
}

fn is_path_boundary(character: char) -> bool {
    character.is_whitespace() || matches!(character, '"' | '\'' | '(' | '[' | '{' | '<' | '=' | ':')
}

fn path_end_column(chars: &[char], start: usize) -> usize {
    let mut end = chars.len();
    for (index, character) in chars.iter().enumerate().skip(start) {
        if character.is_whitespace() || matches!(character, '<' | '>' | '"' | '\'' | ')' | ']' | '}') {
            end = index;
            break;
        }
    }
    while end > start && matches!(chars[end - 1], '.' | ',' | ';' | '!' | '?') {
        end -= 1;
    }
    end
}

/// Open a URL or file path with the platform's default handler.
pub fn open_url(url: &str) {
    let command = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let result = std::process::Command::new(command)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(error) = result {
        tracing::warn!("failed to open URL {url}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{find_file_path_at_column, parse_lsof_cwd};

    #[test]
    fn parse_lsof_cwd_extracts_working_directory() {
        let output = "p1234\nfcwd\nn/tmp/project\n";

        assert_eq!(parse_lsof_cwd(output), Some(PathBuf::from("/tmp/project")));
    }

    #[test]
    fn parse_lsof_cwd_returns_none_without_cwd_entry() {
        let output = "p1234\nf1\nn/tmp/project/file.txt\n";

        assert_eq!(parse_lsof_cwd(output), None);
    }

    #[test]
    fn parse_pgrep_pid_extracts_first_child_pid() {
        let output = "12027\n12099\n";

        assert_eq!(super::parse_pgrep_pid(output), Some(12027));
    }

    #[test]
    fn parse_first_child_pid_extracts_first_child() {
        assert_eq!(super::parse_first_child_pid("31219 31300\n"), Some(31219));
    }

    #[test]
    fn parse_first_child_pid_returns_none_when_empty() {
        assert_eq!(super::parse_first_child_pid(""), None);
        assert_eq!(super::parse_first_child_pid("\n"), None);
    }

    // ---- find_file_path_at_column TDD tests ----

    #[test]
    fn absolute_path_no_suffix_detected() {
        let line: Vec<char> = "/a/b.rs".chars().collect();
        // click inside path
        assert_eq!(find_file_path_at_column(&line, 3), Some(("/a/b.rs".to_string(), None)));
    }

    #[test]
    fn absolute_path_with_line_suffix_detected() {
        let line: Vec<char> = "/a/b.rs:12".chars().collect();
        assert_eq!(
            find_file_path_at_column(&line, 3),
            Some(("/a/b.rs".to_string(), Some(12)))
        );
    }

    #[test]
    fn absolute_path_with_line_and_col_suffix_detected() {
        let line: Vec<char> = "/a/b.rs:12:5".chars().collect();
        assert_eq!(
            find_file_path_at_column(&line, 3),
            Some(("/a/b.rs".to_string(), Some(12)))
        );
    }

    #[test]
    fn relative_path_with_slash_detected() {
        let line: Vec<char> = "crates/foo/bar.rs:7".chars().collect();
        // click inside path (col 5 = 'o' in "foo")
        assert_eq!(
            find_file_path_at_column(&line, 5),
            Some(("crates/foo/bar.rs".to_string(), Some(7)))
        );
    }

    #[test]
    fn relative_file_name_with_extension_detected() {
        let line: Vec<char> = "main.rs:3".chars().collect();
        assert_eq!(
            find_file_path_at_column(&line, 2),
            Some(("main.rs".to_string(), Some(3)))
        );
    }

    #[test]
    fn plain_word_without_slash_or_extension_rejected() {
        let line: Vec<char> = "hello".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 2), None);
    }

    #[test]
    fn relative_dir_token_with_slash_detected() {
        // `src/lib` has a slash, so it passes the slash rule
        let line: Vec<char> = "src/lib".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 2), Some(("src/lib".to_string(), None)));
    }

    #[test]
    fn all_numeric_version_rejected() {
        let line: Vec<char> = "0.3.1".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 2), None);
    }

    #[test]
    fn decimal_number_rejected() {
        let line: Vec<char> = "3.14".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 1), None);
    }

    #[test]
    fn short_alpha_ratio_rejected() {
        // `N/A`: segments are single chars, so the slash rule rejects it.
        let line: Vec<char> = "N/A".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 1), None);
    }

    #[test]
    fn short_numeric_ratio_rejected() {
        let line: Vec<char> = "3/5".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 1), None);
    }

    #[test]
    fn leading_dash_token_rejected() {
        // Security: a flag-shaped token must never be returned as a path.
        let line: Vec<char> = "-rf".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 1), None);

        let line: Vec<char> = "--goto".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 2), None);
    }

    #[test]
    fn click_in_line_suffix_zone_returns_none() {
        // For `/a/b.rs:12`, the path span is `/a/b.rs` (cols 0..7). A click in
        // the `:12` suffix zone (col 8) is outside the clickable path span.
        let line: Vec<char> = "/a/b.rs:12".chars().collect();
        assert_eq!(find_file_path_at_column(&line, 8), None);
    }
}
