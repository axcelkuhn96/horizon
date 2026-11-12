//! Reader for Claude Code's live-session file, `<config_dir>/sessions/<pid>.json`.
//!
//! Every running Claude Code process keeps one such file with its session id, the session name
//! (the one `ListAgents` shows), and a `busy`/`idle` status. Only that exact file is ever
//! opened: the `<pid>.<hash>.key` files next to it are never listed, opened or read.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use super::{AgentStatus, StatusEvent, is_valid_session_id};

/// Largest session file that is read; a real one is well under 2 KiB.
const MAX_SESSION_FILE_BYTES: u64 = 64 * 1024;
/// Longest session name kept (in chars).
const MAX_NAME_CHARS: usize = 120;
const MAX_FIELD_CHARS: usize = 32;

/// The `status` field of a live-session file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveSessionStatus {
    Busy,
    Idle,
    /// Absent or a value this version does not know.
    Unknown,
}

/// What Horizon uses from a live-session file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSessionInfo {
    pub session_id: String,
    /// Session name as `ListAgents` shows it; control characters removed, bounded.
    pub name: Option<String>,
    /// `"derived"` or `"auto"` (as Claude Code writes it).
    pub name_source: Option<String>,
    pub status: LiveSessionStatus,
    pub status_updated_at_ms: Option<u64>,
    /// `"interactive"` or `"bg"`.
    pub kind: Option<String>,
}

#[derive(Deserialize)]
struct RawSession {
    #[serde(default)]
    pid: Option<u64>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "nameSource")]
    name_source: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "statusUpdatedAt")]
    status_updated_at: Option<u64>,
    #[serde(default)]
    kind: Option<String>,
}

fn session_file(config_dir: &Path, pid: u32) -> PathBuf {
    config_dir.join("sessions").join(format!("{pid}.json"))
}

fn clean_text(raw: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = raw.chars().filter(|ch| !ch.is_control()).take(max_chars).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

fn parse_session(contents: &str, pid: u32) -> Option<LiveSessionInfo> {
    let raw: RawSession = serde_json::from_str(contents).ok()?;
    if raw.pid.is_some_and(|recorded| recorded != u64::from(pid)) {
        return None;
    }
    let session_id = raw.session_id.filter(|id| is_valid_session_id(id))?;
    let status = match raw.status.as_deref() {
        Some("busy") => LiveSessionStatus::Busy,
        Some("idle") => LiveSessionStatus::Idle,
        _ => LiveSessionStatus::Unknown,
    };
    Some(LiveSessionInfo {
        session_id,
        name: raw.name.as_deref().and_then(|name| clean_text(name, MAX_NAME_CHARS)),
        name_source: raw
            .name_source
            .as_deref()
            .and_then(|text| clean_text(text, MAX_FIELD_CHARS)),
        status,
        status_updated_at_ms: raw.status_updated_at,
        kind: raw.kind.as_deref().and_then(|text| clean_text(text, MAX_FIELD_CHARS)),
    })
}

fn read_bounded(path: &Path) -> Option<(String, Option<SystemTime>)> {
    let file = std::fs::File::open(path).ok()?;
    let modified = file.metadata().ok().and_then(|meta| meta.modified().ok());
    let mut contents = String::new();
    file.take(MAX_SESSION_FILE_BYTES).read_to_string(&mut contents).ok()?;
    Some((contents, modified))
}

/// Reads `<config_dir>/sessions/<pid>.json`. `None` when the file is missing, oversized,
/// malformed, names another pid, or carries no valid session id. Never logs the contents.
#[must_use]
pub fn read_live_session(config_dir: &Path, pid: u32) -> Option<LiveSessionInfo> {
    let (contents, _) = read_bounded(&session_file(config_dir, pid))?;
    parse_session(&contents, pid)
}

/// Outcome of [`read_live_session_if_changed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveSessionRead {
    /// The file's modification time is the one already seen, or the new contents could not be
    /// used (a half-written file): keep what is known and ask again later.
    Unchanged,
    /// The file is gone: the process exited.
    Gone,
    Fresh {
        info: LiveSessionInfo,
        modified: Option<SystemTime>,
    },
}

/// Like [`read_live_session`], but skips reading the contents when the file's modification time
/// equals `last_modified` (one `stat` per check).
#[must_use]
pub fn read_live_session_if_changed(config_dir: &Path, pid: u32, last_modified: Option<SystemTime>) -> LiveSessionRead {
    let path = session_file(config_dir, pid);
    let modified = match std::fs::metadata(&path) {
        Ok(meta) => meta.modified().ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LiveSessionRead::Gone,
        Err(_) => return LiveSessionRead::Unchanged,
    };
    if modified.is_some() && modified == last_modified {
        return LiveSessionRead::Unchanged;
    }
    match read_bounded(&path).and_then(|(contents, _)| parse_session(&contents, pid)) {
        Some(info) => LiveSessionRead::Fresh { info, modified },
        None => LiveSessionRead::Unchanged,
    }
}

/// Turns the `busy`/`idle` file status into status events, once per change.
#[derive(Clone, Debug, Default)]
pub struct LiveSessionTracker {
    last: Option<LiveSessionStatus>,
}

impl LiveSessionTracker {
    /// `Busy` is `Working` and `Idle` is `Done`; an unchanged or unknown status yields nothing,
    /// so a hook-reported `Waiting` is not overwritten while the file still says `busy`.
    pub fn observe(&mut self, info: &LiveSessionInfo) -> Option<StatusEvent> {
        if self.last == Some(info.status) {
            return None;
        }
        self.last = Some(info.status);
        let state = match info.status {
            LiveSessionStatus::Busy => AgentStatus::Working,
            LiveSessionStatus::Idle => AgentStatus::Done,
            LiveSessionStatus::Unknown => return None,
        };
        Some(StatusEvent {
            state,
            session_id: Some(info.session_id.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

    fn write_session(dir: &Path, pid: u32, contents: &str) {
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::write(sessions.join(format!("{pid}.json")), contents).expect("write session");
    }

    fn full(pid: u32, status: &str) -> String {
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"/repo","kind":"interactive","name":"expia-11","nameSource":"derived","status":"{status}","statusUpdatedAt":1700000000000,"updatedAt":1,"messagingSocketPath":"/tmp/x","peerProtocol":2,"unknownField":[1,2]}}"#
        )
    }

    #[test]
    fn reads_name_status_and_session_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_session(dir.path(), 4242, &full(4242, "busy"));

        let info = read_live_session(dir.path(), 4242).expect("session");

        assert_eq!(info.session_id, SESSION);
        assert_eq!(info.name.as_deref(), Some("expia-11"));
        assert_eq!(info.name_source.as_deref(), Some("derived"));
        assert_eq!(info.status, LiveSessionStatus::Busy);
        assert_eq!(info.status_updated_at_ms, Some(1_700_000_000_000));
        assert_eq!(info.kind.as_deref(), Some("interactive"));
    }

    #[test]
    fn absent_status_is_unknown_and_idle_is_idle() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_session(dir.path(), 1, &format!(r#"{{"pid":1,"sessionId":"{SESSION}"}}"#));
        write_session(dir.path(), 2, &full(2, "idle"));

        assert_eq!(
            read_live_session(dir.path(), 1).expect("session").status,
            LiveSessionStatus::Unknown
        );
        assert_eq!(
            read_live_session(dir.path(), 2).expect("session").status,
            LiveSessionStatus::Idle
        );
    }

    #[test]
    fn missing_malformed_oversized_and_foreign_files_are_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_live_session(dir.path(), 7).is_none());
        write_session(dir.path(), 7, "not json");
        assert!(read_live_session(dir.path(), 7).is_none());
        write_session(dir.path(), 8, &full(9, "idle"));
        assert!(
            read_live_session(dir.path(), 8).is_none(),
            "a file naming another pid is not trusted"
        );
        write_session(dir.path(), 10, r#"{"pid":10,"sessionId":"../../etc"}"#);
        assert!(read_live_session(dir.path(), 10).is_none());
        let padding = " ".repeat(usize::try_from(MAX_SESSION_FILE_BYTES).unwrap_or(70_000) + 10);
        write_session(dir.path(), 11, &format!("{padding}{}", full(11, "idle")));
        assert!(
            read_live_session(dir.path(), 11).is_none(),
            "the size cap cuts the file before the JSON starts"
        );
    }

    #[test]
    fn name_loses_control_characters_and_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let long = "n".repeat(500);
        write_session(
            dir.path(),
            5,
            &format!(r#"{{"pid":5,"sessionId":"{SESSION}","name":"a\u001b[31mb\n{long}"}}"#),
        );

        let name = read_live_session(dir.path(), 5).expect("session").name.expect("name");

        assert!(!name.chars().any(char::is_control));
        assert!(name.starts_with("a[31mb"));
        assert_eq!(name.chars().count(), MAX_NAME_CHARS);

        write_session(
            dir.path(),
            6,
            &format!(r#"{{"pid":6,"sessionId":"{SESSION}","name":"  \u0007 "}}"#),
        );
        assert_eq!(read_live_session(dir.path(), 6).expect("session").name, None);
    }

    #[test]
    fn key_files_are_never_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions dir");
        std::fs::write(sessions.join("9.abcdef.key"), "SECRET").expect("key file");

        assert!(read_live_session(dir.path(), 9).is_none());
        assert_eq!(read_live_session_if_changed(dir.path(), 9, None), LiveSessionRead::Gone);
    }

    #[test]
    fn unchanged_modification_time_skips_the_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_session(dir.path(), 3, &full(3, "busy"));

        let LiveSessionRead::Fresh { info, modified } = read_live_session_if_changed(dir.path(), 3, None) else {
            panic!("first read must be fresh");
        };
        assert_eq!(info.status, LiveSessionStatus::Busy);
        assert_eq!(
            read_live_session_if_changed(dir.path(), 3, modified),
            LiveSessionRead::Unchanged
        );
        std::fs::remove_file(dir.path().join("sessions/3.json")).expect("remove");
        assert_eq!(
            read_live_session_if_changed(dir.path(), 3, modified),
            LiveSessionRead::Gone
        );
    }

    #[test]
    fn a_half_written_file_keeps_the_last_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_session(dir.path(), 4, "{\"pid\":4,\"sessi");

        assert_eq!(
            read_live_session_if_changed(dir.path(), 4, None),
            LiveSessionRead::Unchanged
        );
    }

    #[test]
    fn tracker_emits_once_per_change_and_never_for_unknown() {
        let mut tracker = LiveSessionTracker::default();
        let info = |status| LiveSessionInfo {
            session_id: SESSION.to_owned(),
            name: None,
            name_source: None,
            status,
            status_updated_at_ms: None,
            kind: None,
        };

        let first = tracker.observe(&info(LiveSessionStatus::Busy)).expect("working");
        assert_eq!(first.state, AgentStatus::Working);
        assert_eq!(first.session_id.as_deref(), Some(SESSION));
        assert_eq!(tracker.observe(&info(LiveSessionStatus::Busy)), None);
        let done = tracker.observe(&info(LiveSessionStatus::Idle)).expect("done");
        assert_eq!(done.state, AgentStatus::Done);
        assert_eq!(tracker.observe(&info(LiveSessionStatus::Unknown)), None);
        assert_eq!(
            tracker.observe(&info(LiveSessionStatus::Idle)).map(|event| event.state),
            Some(AgentStatus::Done),
            "a status that went through Unknown is a change again"
        );
    }
}
