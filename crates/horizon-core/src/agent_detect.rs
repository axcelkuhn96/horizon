use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Identifies which `claude` account/binary is running inside a shell PTY.
///
/// Both accounts share the same binary; the distinguishing signal is the
/// `CLAUDE_CONFIG_DIR` environment variable in the process's `/proc/<pid>/environ`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAgent {
    /// `"claude"` for the primary account, `"claude2"` for the secondary account
    /// (whose `CLAUDE_CONFIG_DIR` ends with `.claude-conta2`).
    pub binary: String,
    /// Resolved config directory for this agent (e.g. `~/.claude` or `~/.claude-conta2`).
    pub config_dir: PathBuf,
    /// The session UUID passed to `--resume` when claude was launched, if any.
    ///
    /// `None` when claude was started fresh (no `--resume`) or when the cmdline
    /// could not be read (non-Linux, permission error, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Parse NUL-separated environ bytes, extract `CLAUDE_CONFIG_DIR`, and return
/// the matching [`RunningAgent`].
///
/// Rules:
/// - If `CLAUDE_CONFIG_DIR` ends with `.claude-conta2` → `binary = "claude2"`, `config_dir` = that path.
/// - Otherwise (absent or any other value) → `binary = "claude"`, `config_dir = <home>/.claude`.
///
/// The function is pure and unit-tested; it never reads the filesystem.
#[must_use]
pub fn classify_agent_from_environ(environ: &[u8], home: &Path) -> RunningAgent {
    let config_dir_value = environ
        .split(|&b| b == 0)
        .find_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            entry.strip_prefix("CLAUDE_CONFIG_DIR=")
        })
        .map(str::to_owned);

    match config_dir_value {
        Some(val) if val.ends_with(".claude-conta2") => RunningAgent {
            binary: "claude2".to_owned(),
            config_dir: PathBuf::from(val),
            session_id: None,
        },
        _ => RunningAgent {
            binary: "claude".to_owned(),
            config_dir: home.join(".claude"),
            session_id: None,
        },
    }
}

/// Extract the `--resume <uuid>` session id from NUL-separated cmdline bytes.
///
/// Accepts two forms:
/// - `--resume <uuid>` (two separate NUL-delimited tokens)
/// - `--resume=<uuid>` (single token)
///
/// The candidate uuid is validated: only chars `[0-9a-fA-F-]`, length 32–128.
/// Returns `None` if absent, malformed, or failing validation.
#[must_use]
pub fn session_id_from_cmdline(cmdline: &[u8]) -> Option<String> {
    let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).collect();

    for (i, arg) in args.iter().enumerate() {
        let Ok(s) = std::str::from_utf8(arg) else {
            continue;
        };

        // Form 1: --resume=<uuid>
        if let Some(candidate) = s.strip_prefix("--resume=") {
            if is_uuid_like(candidate) {
                return Some(candidate.to_owned());
            }
            return None;
        }

        // Form 2: --resume <uuid>  (two separate tokens)
        if s == "--resume" {
            let next = args.get(i + 1)?;
            let Ok(candidate) = std::str::from_utf8(next) else {
                return None;
            };
            if is_uuid_like(candidate) {
                return Some(candidate.to_owned());
            }
            return None;
        }
    }

    None
}

/// Returns `true` when `s` looks like a UUID: only `[0-9a-fA-F-]`, length 32–128.
///
/// Intentionally looser than RFC 4122 (no canonical 8-4-4-4-12 grouping check):
/// it accepts any hex+dash id claude might emit. Do NOT "fix" this to
/// `Uuid::parse_str` — that would reject valid non-standard session ids.
fn is_uuid_like(s: &str) -> bool {
    let len = s.len();
    (32..=128).contains(&len) && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Scan terminal scrollback text for `--resume <uuid>` occurrences and return
/// the **last** (most recent) matching uuid.
///
/// This is the fallback path when argv gave no session id: claude prints
/// `Resume this session with:\nclaude --resume <uuid>` on graceful exit, or a
/// user may have typed `claude --resume <uuid>` or `claude2 --resume <uuid>`.
///
/// The leading binary is incidental — we match on the `--resume` token
/// followed by a whitespace-delimited uuid-like token.  ANSI/color escape
/// codes are stripped before matching so colored terminal output does not
/// interfere.
///
/// Returns the last uuid found (validated with [`is_uuid_like`]), or `None`
/// when no valid `--resume <uuid>` pair is present.
#[must_use]
pub fn session_id_from_resume_line(text: &str) -> Option<String> {
    // Strip ANSI escape sequences (\x1b[...m and other CSI sequences).
    // We only need to remove non-UUID chars that might be injected around the
    // uuid token by the terminal's color output.
    let stripped = strip_ansi_escapes(text);

    let mut last_uuid: Option<String> = None;

    // Tokenize on whitespace and scan for --resume <next-token> pairs.
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "--resume"
            && let Some(candidate) = tokens.get(i + 1)
        {
            // Strip any remaining non-uuid chars from the candidate
            // (e.g. residual ANSI bytes that survived the gross strip).
            let clean = candidate.trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-');
            if is_uuid_like(clean) {
                last_uuid = Some(clean.to_owned());
            }
            i += 2;
            continue;
        }
        i += 1;
    }

    last_uuid
}

/// Remove ANSI/VT escape sequences from `text` (CSI, OSC, and plain `\x1b`
/// sequences). Returns a `String` with only printable content.
fn strip_ansi_escapes(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    // CSI sequence: consume until a letter (final byte).
                    chars.next(); // consume '['
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    // OSC sequence: consume until ST (\x1b\\) or BEL (\x07).
                    chars.next(); // consume ']'
                    let mut prev = '\0';
                    for c in chars.by_ref() {
                        if c == '\x07' || (prev == '\x1b' && c == '\\') {
                            break;
                        }
                        prev = c;
                    }
                }
                _ => {
                    // Other escape: skip the next char if present.
                    chars.next();
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Walk the process tree rooted at `child_pid` (the PTY child, typically a
/// `script` wrapper) until we find a process whose `cmdline` identifies it as a
/// `claude` CLI invocation, then classify it via [`classify_agent_from_environ`].
///
/// Returns `None` if:
/// - no claude process is found in the subtree,
/// - any required `/proc` file is unreadable, or
/// - we are not running on Linux.
///
/// All I/O errors are silently swallowed; the environ is never logged.
#[cfg(target_os = "linux")]
#[must_use]
pub fn detect_running_agent(child_pid: i32) -> Option<RunningAgent> {
    let home = home_dir()?;
    let pid = u32::try_from(child_pid).ok()?;
    let claude_pid = find_claude_pid(pid)?;
    let environ = std::fs::read(format!("/proc/{claude_pid}/environ")).ok()?;
    let mut agent = classify_agent_from_environ(&environ, &home);
    // Read argv to extract --resume <session-id> if present.
    // IO errors → None (session_id stays None); cmdline contents are never logged.
    if let Ok(cmdline) = std::fs::read(format!("/proc/{claude_pid}/cmdline")) {
        agent.session_id = session_id_from_cmdline(&cmdline);
    }
    Some(agent)
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn detect_running_agent(_child_pid: i32) -> Option<RunningAgent> {
    None
}

// ── private helpers (Linux-only) ────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Recursively walk `/proc/<pid>/task/<pid>/children` starting from `pid`.
/// Returns the pid of the first process whose cmdline identifies it as a
/// claude CLI process.
#[cfg(target_os = "linux")]
fn find_claude_pid(pid: u32) -> Option<u32> {
    find_claude_pid_inner(pid, 0)
}

/// Bound the recursion so a pathologically deep/forky process tree cannot
/// overflow the stack.
#[cfg(target_os = "linux")]
const MAX_PROC_WALK_DEPTH: u32 = 32;

#[cfg(target_os = "linux")]
fn find_claude_pid_inner(pid: u32, depth: u32) -> Option<u32> {
    if depth >= MAX_PROC_WALK_DEPTH {
        return None;
    }

    if cmdline_is_claude(pid) {
        return Some(pid);
    }

    let children_path = format!("/proc/{pid}/task/{pid}/children");
    let children_text = std::fs::read_to_string(children_path).ok()?;

    for token in children_text.split_whitespace() {
        let Ok(child) = token.parse::<u32>() else {
            continue;
        };
        if let Some(found) = find_claude_pid_inner(child, depth + 1) {
            return Some(found);
        }
    }

    None
}

/// Returns `true` when the process `cmdline` looks like a claude CLI.
///
/// The actual `claude` executable is a Node.js script, so `/proc/<pid>/comm`
/// is typically `node`. We therefore inspect only `argv[0]` (the command-name
/// position) of the NUL-separated `cmdline` for path fragments that identify
/// the claude CLI entry-point. Restricting to `argv[0]` avoids false positives
/// from a bare `claude` token appearing later in another process's arguments.
#[cfg(target_os = "linux")]
fn cmdline_is_claude(pid: u32) -> bool {
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };

    // cmdline is NUL-separated; only the first entry is the executable path.
    let Some(argv0) = cmdline.split(|&b| b == 0).next() else {
        return false;
    };
    let Ok(argv0) = std::str::from_utf8(argv0) else {
        return false;
    };

    argv0.contains(".local/bin/claude") || argv0.ends_with("/bin/claude") || argv0 == "claude"
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{classify_agent_from_environ, session_id_from_cmdline, session_id_from_resume_line};

    fn home() -> &'static Path {
        Path::new("/home/x")
    }

    fn env_bytes(vars: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (k, v) in vars {
            bytes.extend_from_slice(k.as_bytes());
            bytes.push(b'=');
            bytes.extend_from_slice(v.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    // ── session_id_from_cmdline tests ─────────────────────────────────────────

    const UUID: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

    // (a) standard two-token form
    #[test]
    fn cmdline_resume_two_token() {
        let cmdline = b"claude\0--resume\0f054f32b-7840-4c95-9f84-e61f37db5f5c\0";
        assert_eq!(
            session_id_from_cmdline(cmdline),
            Some("f054f32b-7840-4c95-9f84-e61f37db5f5c".to_owned())
        );
    }

    // (b) no --resume at all → None
    #[test]
    fn cmdline_no_resume() {
        assert_eq!(session_id_from_cmdline(b"claude\0"), None);
    }

    // (c) --resume as the last token (no value after it) → None
    #[test]
    fn cmdline_resume_last_token_no_value() {
        assert_eq!(session_id_from_cmdline(b"claude\0--resume\0"), None);
    }

    // (d) --resume followed by a non-uuid (path) → None
    #[test]
    fn cmdline_resume_non_uuid_value() {
        assert_eq!(
            session_id_from_cmdline(b"claude\0--resume\0./some/path\0"),
            None
        );
    }

    // (e) joined --resume=<uuid> form → Some(uuid)
    #[test]
    fn cmdline_resume_equals_form() {
        let cmdline = format!("claude\0--resume={UUID}\0");
        assert_eq!(
            session_id_from_cmdline(cmdline.as_bytes()),
            Some(UUID.to_owned())
        );
    }

    // (f) extra args surrounding --resume → uuid still found
    #[test]
    fn cmdline_resume_with_surrounding_args() {
        let cmdline = format!("claude\0--model\0sonnet\0--resume\0{UUID}\0--foo\0");
        assert_eq!(
            session_id_from_cmdline(cmdline.as_bytes()),
            Some(UUID.to_owned())
        );
    }

    // (g) over-long (>128 chars) all-hex value → None
    #[test]
    fn cmdline_resume_overlength_uuid() {
        let long_hex = "a".repeat(129);
        let cmdline = format!("claude\0--resume\0{long_hex}\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), None);
    }

    // (h) First --resume= token has a bad value: we return None immediately and
    // do NOT scan ahead to a later valid --resume. claude never emits two
    // --resume flags, so the early-return on a malformed first match is by design.
    #[test]
    fn cmdline_resume_short_circuits_on_first_match() {
        let bad = "z".repeat(36);
        let cmdline = format!("--resume={bad}\0--resume\0{UUID}\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), None);
    }

    // boundary: minimum valid length (32) → Some
    #[test]
    fn cmdline_resume_min_length_uuid() {
        let id = "a".repeat(32);
        let cmdline = format!("claude\0--resume\0{id}\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), Some(id));
    }

    // boundary: maximum valid length (128) → Some
    #[test]
    fn cmdline_resume_max_length_uuid() {
        let id = "a".repeat(128);
        let cmdline = format!("claude\0--resume\0{id}\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), Some(id));
    }

    // empty cmdline → None (no panic)
    #[test]
    fn cmdline_empty() {
        assert_eq!(session_id_from_cmdline(b""), None);
    }

    // (a) CLAUDE_CONFIG_DIR points to .claude-conta2 → claude2
    #[test]
    fn conta2_config_dir_returns_claude2() {
        let environ = env_bytes(&[("CLAUDE_CONFIG_DIR", "/home/x/.claude-conta2")]);
        let agent = classify_agent_from_environ(&environ, home());
        assert_eq!(agent.binary, "claude2");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude-conta2"));
    }

    // (b) No CLAUDE_CONFIG_DIR → default claude
    #[test]
    fn missing_config_dir_returns_default_claude() {
        let environ = env_bytes(&[("HOME", "/home/x"), ("TERM", "xterm-256color")]);
        let agent = classify_agent_from_environ(&environ, home());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    // (c) CLAUDE_CONFIG_DIR is some OTHER path → default claude
    #[test]
    fn other_config_dir_returns_default_claude() {
        let environ = env_bytes(&[("CLAUDE_CONFIG_DIR", "/tmp/foo")]);
        let agent = classify_agent_from_environ(&environ, home());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    // (d) CLAUDE_CONFIG_DIR interleaved with other vars → still found
    #[test]
    fn interleaved_vars_still_finds_config_dir() {
        let environ = env_bytes(&[
            ("USER", "x"),
            ("HOME", "/home/x"),
            ("CLAUDE_CONFIG_DIR", "/home/x/.claude-conta2"),
            ("TERM", "xterm-256color"),
            ("PATH", "/usr/bin:/bin"),
        ]);
        let agent = classify_agent_from_environ(&environ, home());
        assert_eq!(agent.binary, "claude2");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude-conta2"));
    }

    // (e) Malformed / empty environ → default claude (no panic)
    #[test]
    fn empty_environ_returns_default_claude() {
        let agent = classify_agent_from_environ(&[], home());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    #[test]
    fn garbage_bytes_returns_default_claude() {
        // Arbitrary non-UTF8 bytes should not panic
        let garbage: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0xAB, 0xCD];
        let agent = classify_agent_from_environ(&garbage, home());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    // ── session_id_from_resume_line tests ─────────────────────────────────────

    const RESUME_UUID: &str = "6c81741c-b21f-4d2f-bc63-9c245c1b8c0d";
    const RESUME_UUID2: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // (a) claude's graceful-exit resume block → Some(uuid)
    #[test]
    fn resume_line_claude_exit_block() {
        let text = format!("Resume this session with:\nclaude --resume {RESUME_UUID}\n");
        assert_eq!(
            session_id_from_resume_line(&text),
            Some(RESUME_UUID.to_owned())
        );
    }

    // (b) two resume lines → returns the LAST uuid
    #[test]
    fn resume_line_returns_last_uuid_when_multiple() {
        let text = format!(
            "claude --resume {RESUME_UUID}\nsome other output\nclaude --resume {RESUME_UUID2}\n"
        );
        assert_eq!(
            session_id_from_resume_line(&text),
            Some(RESUME_UUID2.to_owned())
        );
    }

    // (c) no resume line → None
    #[test]
    fn resume_line_absent_returns_none() {
        assert_eq!(session_id_from_resume_line("hello world\nno resume here\n"), None);
    }

    // (d) --resume followed by a garbage/non-uuid token → None for that occurrence
    #[test]
    fn resume_line_garbage_token_returns_none() {
        assert_eq!(
            session_id_from_resume_line("claude --resume ./some/path"),
            None
        );
    }

    // (e) ANSI color escapes around the uuid → still extracts the uuid
    #[test]
    fn resume_line_ansi_escapes_stripped() {
        // Simulate: claude --resume \x1b[0m<uuid>\x1b[0m
        let text = format!("claude --resume \x1b[0m{RESUME_UUID}\x1b[0m");
        assert_eq!(
            session_id_from_resume_line(&text),
            Some(RESUME_UUID.to_owned())
        );
    }

    // (f) claude2 binary → Some(uuid) (binary is incidental)
    #[test]
    fn resume_line_claude2_binary() {
        let text = format!("claude2 --resume {RESUME_UUID}");
        assert_eq!(
            session_id_from_resume_line(&text),
            Some(RESUME_UUID.to_owned())
        );
    }

    // empty text → None (no panic)
    #[test]
    fn resume_line_empty_text() {
        assert_eq!(session_id_from_resume_line(""), None);
    }

    // --resume present but no following token → None (no panic)
    #[test]
    fn resume_line_no_token_after_flag() {
        assert_eq!(session_id_from_resume_line("claude --resume"), None);
    }
}
