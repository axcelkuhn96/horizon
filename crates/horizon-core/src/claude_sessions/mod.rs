//! Recent Claude sessions of a project and all its worktrees.
//!
//! Claude Code stores one transcript per working directory under
//! `<config dir>/projects/<encoded cwd>/<session id>.jsonl`, and every account shares that
//! directory through a symlink. The path therefore says nothing about WHICH account ran a
//! session: [`AccountIndex`] (a Horizon-owned file) remembers it for the sessions Horizon saw.
//!
//! - [`encode_project_dir`] / [`project_dirs_for`]: which transcript directories belong to a
//!   workspace (its repository's main directory plus every worktree).
//! - [`SessionScanner`] (on [`DirectoryScanner`]): a worker thread that lists those directories and reads only the HEAD
//!   of each new or changed transcript (title and first prompt), caching by `(path, mtime, len)`.
//! - [`AccountIndex`]: session id to account, written atomically with mode 0600 off the UI thread.
//! - [`resume_command_args`]: the only way a session id reaches a command line, UUID-strict.
//!
//! Transcripts are untrusted input: reads are size-capped, lines that do not parse are skipped,
//! shown text is stripped of control characters and nothing from them is ever logged.

mod cache;
mod index;
mod scan;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ClaudeProfile;
use crate::config::expand_config_dir;
use crate::git_overview::RepoOverview;

pub use index::{AccountEntry, AccountIndex, AccountIndexWriter, MAX_INDEX_ENTRIES};
pub use scan::{
    DirectoryScanner, DiscoveredAccount, ScanDir, ScanLimits, ScanRequest, ScanResult, SessionScanner,
    discover_live_accounts,
};

/// Why a session id or index operation was refused.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The id is not a canonical UUID (`8-4-4-4-12` hex digits).
    #[error("not a valid Claude session id")]
    InvalidSessionId,
    #[error("session index io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("session index could not be serialized: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Where a session ran: the main checkout, a linked worktree or a Claude agent worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionOrigin {
    /// The directory the session ran in; `claude --resume` must start here to find it.
    pub worktree: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
    /// Under `.claude/worktrees/` (a Claude agent worktree).
    pub is_agent: bool,
}

/// The account a session ran under, when Horizon knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionAccount {
    Profile(String),
    Unknown,
}

impl SessionAccount {
    #[must_use]
    pub fn profile(&self) -> Option<&str> {
        match self {
            Self::Profile(name) => Some(name),
            Self::Unknown => None,
        }
    }
}

/// One transcript, summarised from its head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub origin: SessionOrigin,
    /// Canonical UUID (the transcript's file stem).
    pub id: String,
    /// Claude's own summary or title when the transcript has one, else the first prompt; never empty.
    pub title: String,
    /// First real user message (control characters removed, bounded); may be empty.
    pub first_prompt: String,
    /// Transcript modification time.
    pub updated: SystemTime,
    pub size: u64,
    /// Directory to resume in (same as `origin.worktree`).
    pub cwd: PathBuf,
    pub account: SessionAccount,
}

/// `true` for a canonical lowercase or uppercase UUID: `8-4-4-4-12` hex digits.
#[must_use]
pub fn is_session_uuid(id: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut parts = id.split('-');
    for len in GROUPS {
        match parts.next() {
            Some(part) if part.len() == len && part.bytes().all(|byte| byte.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// Arguments that resume `session_id`: `["--resume", id]`. Anything but a UUID is refused before
/// a command is built, so an id read from disk can never inject shell syntax.
///
/// # Errors
///
/// [`SessionError::InvalidSessionId`] when `session_id` is not a canonical UUID.
pub fn resume_command_args(session_id: &str) -> Result<Vec<String>, SessionError> {
    if !is_session_uuid(session_id) {
        return Err(SessionError::InvalidSessionId);
    }
    Ok(vec!["--resume".to_owned(), session_id.to_owned()])
}

/// The directory name Claude Code uses for a working directory: every character outside
/// `[A-Za-z0-9]` becomes `-` (so `/home/u/.claude/x_y` is `-home-u--claude-x-y`).
///
/// Claude hashes very long names; those are not reproduced here and simply find no transcripts.
#[must_use]
pub fn encode_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

/// The transcript directories of a workspace: `cwd` itself (a subdirectory of a repository has
/// its own), the repository's main checkout and every worktree of `overview`. Entries are
/// deduplicated by encoded directory, the main checkout first. Worktrees git reports as prunable
/// (directory gone) are kept only when a transcript directory exists for them.
#[must_use]
pub fn project_dirs_for(
    overview: Option<&RepoOverview>,
    cwd: &Path,
    projects_root: &Path,
) -> Vec<(PathBuf, SessionOrigin)> {
    let mut found: Vec<(PathBuf, SessionOrigin)> = Vec::new();
    let mut push = |origin: SessionOrigin, always: bool| {
        let dir = projects_root.join(encode_project_dir(&origin.worktree));
        if found.iter().any(|(existing, _)| *existing == dir) {
            return;
        }
        if always || dir.is_dir() {
            found.push((dir, origin));
        }
    };
    let Some(overview) = overview else {
        push(
            SessionOrigin {
                worktree: cwd.to_path_buf(),
                branch: None,
                is_main: true,
                is_agent: false,
            },
            true,
        );
        return found;
    };
    for worktree in &overview.worktrees {
        push(
            SessionOrigin {
                worktree: worktree.path.clone(),
                branch: worktree.branch.clone(),
                is_main: worktree.is_main,
                is_agent: worktree.is_agent || is_agent_path(&worktree.path),
            },
            !worktree.prunable,
        );
    }
    // The workspace may sit in a subdirectory of its worktree; Claude keys transcripts by that cwd.
    let container = overview
        .worktrees
        .iter()
        .filter(|worktree| cwd.starts_with(&worktree.path))
        .max_by_key(|worktree| worktree.path.as_os_str().len());
    if let Some(container) = container {
        push(
            SessionOrigin {
                worktree: cwd.to_path_buf(),
                branch: container.branch.clone(),
                is_main: container.is_main,
                is_agent: container.is_agent || is_agent_path(&container.path),
            },
            false,
        );
    }
    found
}

fn is_agent_path(path: &Path) -> bool {
    let parts: Vec<_> = path.components().collect();
    parts.windows(2).any(|pair| {
        pair[0].as_os_str() == std::ffi::OsStr::new(".claude")
            && pair[1].as_os_str() == std::ffi::OsStr::new("worktrees")
    })
}

/// Sets each summary's account from `index`; sessions it does not know keep `implied` (the profile
/// that owns the directory they were found in) or become [`SessionAccount::Unknown`].
pub fn attribute_accounts(sessions: &mut [SessionSummary], index: &AccountIndex) {
    for session in sessions {
        if let Some(entry) = index.lookup(&session.id) {
            session.account = SessionAccount::Profile(entry.profile.clone());
        } else if !matches!(session.account, SessionAccount::Profile(_)) {
            session.account = SessionAccount::Unknown;
        }
    }
}

/// Name of the profile whose expanded config dir is `config_dir` (a trailing `/` is ignored).
#[must_use]
pub fn profile_for_config_dir(profiles: &[ClaudeProfile], home: &Path, config_dir: &Path) -> Option<String> {
    let wanted = config_dir.components().collect::<PathBuf>();
    profiles
        .iter()
        .find(|profile| {
            expand_config_dir(&profile.config_dir, home)
                .components()
                .collect::<PathBuf>()
                == wanted
        })
        .map(|profile| profile.name.clone())
}

/// Removes control characters (and bidirectional overrides, which can reorder what is read) and
/// collapses whitespace, then keeps at most `max_chars` characters. Transcript text is untrusted.
#[must_use]
pub fn sanitize_text(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if ch.is_control() || is_bidi_control(ch) {
            continue;
        }
        if pending_space {
            if count >= max_chars {
                break;
            }
            out.push(' ');
            count += 1;
            pending_space = false;
        }
        if count >= max_chars {
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_overview::WorktreeInfo;

    const ID: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

    fn worktree(path: &str, branch: Option<&str>, is_main: bool, is_agent: bool, prunable: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            branch: branch.map(str::to_owned),
            head_short: None,
            is_main,
            is_current: false,
            locked: false,
            prunable,
            dirty_count: None,
            is_agent,
        }
    }

    fn overview(worktrees: Vec<WorktreeInfo>) -> RepoOverview {
        RepoOverview {
            repo_root: PathBuf::from("/home/u/proj"),
            branch: Some("main".to_owned()),
            detached: false,
            head_short: None,
            dirty_count: 0,
            upstream: None,
            ahead: 0,
            behind: 0,
            worktrees,
        }
    }

    #[test]
    fn encode_matches_real_claude_dirs() {
        assert_eq!(
            encode_project_dir(Path::new("/home/axcel/horizon-fork")),
            "-home-axcel-horizon-fork"
        );
        assert_eq!(encode_project_dir(Path::new("/a/b.c/d_e")), "-a-b-c-d-e");
        assert_eq!(
            encode_project_dir(Path::new("/home/axcel/expia/.superpowers/sdd/x")),
            "-home-axcel-expia--superpowers-sdd-x"
        );
        assert_eq!(
            encode_project_dir(Path::new("/home/axcel/App/.claude/worktrees/agent-a9")),
            "-home-axcel-App--claude-worktrees-agent-a9"
        );
    }

    #[test]
    fn uuid_check_is_strict() {
        assert!(is_session_uuid(ID));
        assert!(is_session_uuid(&ID.to_uppercase()));
        for bad in [
            "",
            "session-1",
            "../etc/passwd",
            &format!("{ID} --dangerously-skip-permissions"),
            &format!("{ID};reboot"),
            &format!("{ID}\n"),
            "f054f32b78404c959f84e61f37db5f5c",
            "g054f32b-7840-4c95-9f84-e61f37db5f5c",
            &format!("{ID}-0"),
        ] {
            assert!(!is_session_uuid(bad), "{bad:?}");
        }
    }

    #[test]
    fn resume_args_reject_non_uuid_and_injection() {
        assert_eq!(
            resume_command_args(ID).expect("uuid").as_slice(),
            ["--resume".to_owned(), ID.to_owned()]
        );
        for bad in ["", "abc", "$(reboot)", "a b", "f054f32b-7840-4c95-9f84-e61f37db5f5c'x"] {
            assert!(matches!(resume_command_args(bad), Err(SessionError::InvalidSessionId)));
        }
    }

    #[test]
    fn project_dirs_include_main_and_all_worktrees() {
        let root = tempfile::tempdir().expect("tempdir");
        let overview = overview(vec![
            worktree("/home/u/proj", Some("main"), true, false, false),
            worktree("/home/u/proj-feat", Some("feat/x"), false, false, false),
            worktree("/home/u/proj/.claude/worktrees/agent-1", Some("wt"), false, true, false),
        ]);

        let dirs = project_dirs_for(Some(&overview), Path::new("/home/u/proj"), root.path());

        let names: Vec<_> = dirs
            .iter()
            .map(|(dir, _)| dir.file_name().map(|n| n.to_owned()))
            .collect();
        assert_eq!(names.len(), 3);
        assert!(dirs[0].1.is_main);
        assert_eq!(dirs[0].0, root.path().join("-home-u-proj"));
        assert_eq!(dirs[1].1.branch.as_deref(), Some("feat/x"));
        assert!(dirs[2].1.is_agent);
    }

    #[test]
    fn project_dirs_dedupe_same_encoded_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        // "/a/b.c" and "/a/b-c" encode to the same directory; the first wins.
        let overview = overview(vec![
            worktree("/a/b.c", Some("one"), true, false, false),
            worktree("/a/b-c", Some("two"), false, false, false),
        ]);

        let dirs = project_dirs_for(Some(&overview), Path::new("/a/b.c"), root.path());

        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].1.branch.as_deref(), Some("one"));
    }

    #[test]
    fn project_dirs_without_repo_is_cwd_only() {
        let root = tempfile::tempdir().expect("tempdir");

        let dirs = project_dirs_for(None, Path::new("/home/u/scratch"), root.path());

        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].0, root.path().join("-home-u-scratch"));
        assert_eq!(dirs[0].1.worktree, PathBuf::from("/home/u/scratch"));
        assert!(dirs[0].1.is_main);
    }

    #[test]
    fn prunable_worktrees_need_an_existing_transcript_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        let overview = overview(vec![
            worktree("/home/u/proj", Some("main"), true, false, false),
            worktree("/home/u/gone", Some("old"), false, false, true),
            worktree("/home/u/kept", Some("kept"), false, false, true),
        ]);
        std::fs::create_dir_all(root.path().join("-home-u-kept")).expect("dir");

        let dirs = project_dirs_for(Some(&overview), Path::new("/home/u/proj"), root.path());

        let branches: Vec<_> = dirs.iter().map(|(_, origin)| origin.branch.as_deref()).collect();
        assert_eq!(branches, [Some("main"), Some("kept")]);
    }

    #[test]
    fn workspace_in_a_subdirectory_adds_its_own_transcript_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        let overview = overview(vec![worktree("/home/u/proj", Some("main"), true, false, false)]);
        std::fs::create_dir_all(root.path().join("-home-u-proj-web")).expect("dir");

        let dirs = project_dirs_for(Some(&overview), Path::new("/home/u/proj/web"), root.path());

        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[1].1.worktree, PathBuf::from("/home/u/proj/web"));
        assert!(dirs[1].1.is_main);
    }

    #[test]
    fn agent_worktree_sessions_flagged() {
        let root = tempfile::tempdir().expect("tempdir");
        // Flagged by path even when the git flag was not set.
        let overview = overview(vec![
            worktree("/home/u/proj", Some("main"), true, false, false),
            worktree("/home/u/proj/.claude/worktrees/agent-7", None, false, false, false),
        ]);

        let dirs = project_dirs_for(Some(&overview), Path::new("/home/u/proj"), root.path());

        assert!(!dirs[0].1.is_agent);
        assert!(dirs[1].1.is_agent);
    }

    #[test]
    fn sanitize_strips_controls_bidi_and_bounds_length() {
        assert_eq!(sanitize_text("  hello\n\tworld\u{1b}[31m  ", 100), "hello world[31m");
        assert_eq!(sanitize_text("a\u{202e}b\u{2066}c", 100), "abc");
        assert_eq!(sanitize_text("abcdefghij", 4), "abcd");
        assert_eq!(sanitize_text("ab cd ef", 5), "ab cd");
        assert_eq!(sanitize_text("\n\n", 5), "");
    }

    #[test]
    fn profile_matches_expanded_config_dir_ignoring_trailing_slash() {
        let profiles = vec![ClaudeProfile {
            name: "conta2".to_owned(),
            config_dir: "~/.claude-conta2".to_owned(),
        }];
        let home = Path::new("/home/u");

        assert_eq!(
            profile_for_config_dir(&profiles, home, Path::new("/home/u/.claude-conta2/")).as_deref(),
            Some("conta2")
        );
        assert_eq!(profile_for_config_dir(&profiles, home, Path::new("/elsewhere")), None);
    }

    #[test]
    fn account_attribution_uses_index_else_unknown() {
        let mut index = AccountIndex::empty(PathBuf::from("/nonexistent/idx.json"));
        index.record(ID, "conta2", "/home/u/.claude-conta2", 10);
        let origin = SessionOrigin {
            worktree: PathBuf::from("/x"),
            branch: None,
            is_main: true,
            is_agent: false,
        };
        let make = |id: &str| SessionSummary {
            origin: origin.clone(),
            id: id.to_owned(),
            title: "t".to_owned(),
            first_prompt: String::new(),
            updated: SystemTime::UNIX_EPOCH,
            size: 0,
            cwd: origin.worktree.clone(),
            account: SessionAccount::Unknown,
        };
        let mut sessions = vec![make(ID), make("11111111-2222-3333-4444-555555555555")];

        attribute_accounts(&mut sessions, &index);

        assert_eq!(sessions[0].account, SessionAccount::Profile("conta2".to_owned()));
        assert_eq!(sessions[1].account, SessionAccount::Unknown);
    }
}
