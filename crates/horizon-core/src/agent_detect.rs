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
        },
        _ => RunningAgent {
            binary: "claude".to_owned(),
            config_dir: home.join(".claude"),
        },
    }
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
    Some(classify_agent_from_environ(&environ, &home))
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

    use super::classify_agent_from_environ;

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
}
