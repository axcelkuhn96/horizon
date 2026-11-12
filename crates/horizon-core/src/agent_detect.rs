use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{ClaudeProfile, expand_config_dir};

const DEFAULT_BINARY: &str = "claude";

/// Identifies which `claude` account/binary is running inside a shell PTY.
///
/// Both accounts share the same binary; the distinguishing signal is the
/// `CLAUDE_CONFIG_DIR` environment variable in the process's `/proc/<pid>/environ`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningAgent {
    /// Always `"claude"`; accounts are told apart by [`Self::config_dir`].
    /// Legacy runtime states may still carry `"claude2"`.
    pub binary: String,
    /// Resolved config directory for this agent (e.g. `~/.claude` or `~/.claude-work`).
    pub config_dir: PathBuf,
    /// The session UUID passed to `--resume` when claude was launched, if any.
    ///
    /// `None` when claude was started fresh (no `--resume`) or when the cmdline
    /// could not be read (non-Linux, permission error, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Default Claude config directory: `<home>/.claude`.
#[must_use]
pub fn default_claude_config_dir(home: &Path) -> PathBuf {
    home.join(".claude")
}

/// Parse NUL-separated environ bytes, extract `CLAUDE_CONFIG_DIR`, and return
/// the matching [`RunningAgent`].
///
/// Rules:
/// - `CLAUDE_CONFIG_DIR` equal to a profile's expanded directory → `config_dir` is
///   that profile's (normalized) directory.
/// - Any other non-empty value → `config_dir` = that value.
/// - Absent or empty → `config_dir = <home>/.claude`.
///
/// `binary` is always `"claude"`: profiles differ by config dir, not by binary.
///
/// The function is pure and unit-tested; it never reads the filesystem.
#[must_use]
pub fn classify_agent_from_environ(environ: &[u8], home: &Path, profiles: &[ClaudeProfile]) -> RunningAgent {
    let config_dir_value = environ.split(|&b| b == 0).find_map(|entry| {
        let entry = std::str::from_utf8(entry).ok()?;
        entry.strip_prefix("CLAUDE_CONFIG_DIR=")
    });

    let Some(value) = config_dir_value.filter(|value| !value.is_empty()) else {
        return RunningAgent {
            binary: DEFAULT_BINARY.to_owned(),
            config_dir: default_claude_config_dir(home),
            session_id: None,
        };
    };

    let config_dir = PathBuf::from(value);
    let config_dir = profiles
        .iter()
        .map(|profile| expand_config_dir(&profile.config_dir, home))
        .find(|expanded| *expanded == config_dir)
        .unwrap_or(config_dir);
    RunningAgent {
        binary: DEFAULT_BINARY.to_owned(),
        config_dir,
        session_id: None,
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

/// A running `claude` process and the account it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeProcess {
    /// Pid of the `claude` process itself (the one that owns `<config_dir>/sessions/<pid>.json`).
    pub pid: u32,
    pub agent: RunningAgent,
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
pub fn detect_claude_process(child_pid: i32, profiles: &[ClaudeProfile]) -> Option<ClaudeProcess> {
    let home = home_dir()?;
    let pid = u32::try_from(child_pid).ok()?;
    let claude_pid = find_claude_pid(pid)?;
    let environ = std::fs::read(format!("/proc/{claude_pid}/environ")).ok()?;
    let mut agent = classify_agent_from_environ(&environ, &home, profiles);
    // Read argv to extract --resume <session-id> if present.
    // IO errors → None (session_id stays None); cmdline contents are never logged.
    if let Ok(cmdline) = std::fs::read(format!("/proc/{claude_pid}/cmdline")) {
        agent.session_id = session_id_from_cmdline(&cmdline);
    }
    Some(ClaudeProcess { pid: claude_pid, agent })
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn detect_claude_process(_child_pid: i32, _profiles: &[ClaudeProfile]) -> Option<ClaudeProcess> {
    None
}

/// [`detect_claude_process`] without the pid: which account runs inside the PTY child.
#[must_use]
pub fn detect_running_agent(child_pid: i32, profiles: &[ClaudeProfile]) -> Option<RunningAgent> {
    detect_claude_process(child_pid, profiles).map(|process| process.agent)
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

    use super::{classify_agent_from_environ, default_claude_config_dir, session_id_from_cmdline};
    use crate::config::ClaudeProfile;

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
        assert_eq!(session_id_from_cmdline(b"claude\0--resume\0./some/path\0"), None);
    }

    // (e) joined --resume=<uuid> form → Some(uuid)
    #[test]
    fn cmdline_resume_equals_form() {
        let cmdline = format!("claude\0--resume={UUID}\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), Some(UUID.to_owned()));
    }

    // (f) extra args surrounding --resume → uuid still found
    #[test]
    fn cmdline_resume_with_surrounding_args() {
        let cmdline = format!("claude\0--model\0sonnet\0--resume\0{UUID}\0--foo\0");
        assert_eq!(session_id_from_cmdline(cmdline.as_bytes()), Some(UUID.to_owned()));
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

    fn profiles() -> Vec<ClaudeProfile> {
        [
            ("claude", "~/.claude"),
            ("work", "~/.claude-work"),
            ("conta2", "~/.claude-conta2"),
            ("lab", "/srv/claude-lab/"),
        ]
        .into_iter()
        .map(|(name, dir)| ClaudeProfile {
            name: name.to_string(),
            config_dir: dir.to_string(),
        })
        .collect()
    }

    #[test]
    fn classify_four_profiles_by_config_dir_env() {
        let cases = [
            ("/home/x/.claude", "/home/x/.claude"),
            ("/home/x/.claude-work", "/home/x/.claude-work"),
            ("/home/x/.claude-conta2/", "/home/x/.claude-conta2"),
            ("/srv/claude-lab", "/srv/claude-lab"),
        ];
        for (value, config_dir) in cases {
            let environ = env_bytes(&[("HOME", "/home/x"), ("CLAUDE_CONFIG_DIR", value)]);
            let agent = classify_agent_from_environ(&environ, home(), &profiles());
            assert_eq!(agent.binary, "claude", "binary for {value}");
            assert_eq!(agent.config_dir, PathBuf::from(config_dir), "config_dir for {value}");
        }
    }

    #[test]
    fn classify_without_env_is_default_claude() {
        let environ = env_bytes(&[("HOME", "/home/x"), ("TERM", "xterm-256color")]);
        let agent = classify_agent_from_environ(&environ, home(), &profiles());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
        assert_eq!(default_claude_config_dir(home()), PathBuf::from("/home/x/.claude"));
    }

    #[test]
    fn classify_unknown_dir_keeps_claude_binary_with_that_dir() {
        let environ = env_bytes(&[("CLAUDE_CONFIG_DIR", "/tmp/foo")]);
        let agent = classify_agent_from_environ(&environ, home(), &profiles());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/tmp/foo"));
    }

    #[test]
    fn classify_empty_env_value_is_default_claude() {
        let environ = env_bytes(&[("CLAUDE_CONFIG_DIR", "")]);
        let agent = classify_agent_from_environ(&environ, home(), &profiles());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    #[test]
    fn interleaved_vars_still_finds_config_dir() {
        let environ = env_bytes(&[
            ("USER", "x"),
            ("HOME", "/home/x"),
            ("CLAUDE_CONFIG_DIR", "/home/x/.claude-conta2"),
            ("TERM", "xterm-256color"),
            ("PATH", "/usr/bin:/bin"),
        ]);
        let agent = classify_agent_from_environ(&environ, home(), &profiles());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude-conta2"));
    }

    #[test]
    fn empty_environ_returns_default_claude() {
        let agent = classify_agent_from_environ(&[], home(), &[]);
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }

    #[test]
    fn garbage_bytes_returns_default_claude() {
        // Arbitrary non-UTF8 bytes should not panic
        let garbage: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0xAB, 0xCD];
        let agent = classify_agent_from_environ(&garbage, home(), &profiles());
        assert_eq!(agent.binary, "claude");
        assert_eq!(agent.config_dir, PathBuf::from("/home/x/.claude"));
    }
}
