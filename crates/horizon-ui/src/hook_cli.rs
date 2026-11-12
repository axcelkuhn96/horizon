//! `horizon hook <event>`: the command Claude Code's hooks run to report what the agent is doing.
//!
//! Runs before any GUI start-up. It reads the hook's JSON from stdin (only `session_id` is
//! extracted, nothing else is kept or logged), and writes one OSC title,
//! `ESC ] 2 ; HORIZON_STATUS:<state>[:<session_id>] BEL`, to the controlling terminal, which the
//! panel's terminal parses. Whatever goes wrong it exits 0 without output: a hook must never
//! disturb Claude.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use horizon_core::agent_status::{AgentStatus, is_valid_session_id, status_osc_title};

/// Largest hook payload read from stdin.
const MAX_STDIN_BYTES: u64 = 64 * 1024;

/// Status a hook event reports. A fresh session is ready for input (`Done`); its end clears the
/// status.
fn status_for_event(event: &str) -> Option<AgentStatus> {
    match event {
        "working" => Some(AgentStatus::Working),
        "waiting" => Some(AgentStatus::Waiting),
        "done" | "session-start" => Some(AgentStatus::Done),
        "error" => Some(AgentStatus::Error),
        "session-end" => Some(AgentStatus::Unknown),
        _ => None,
    }
}

/// The `session_id` of a hook payload, when it is JSON with a well-formed one. Everything else
/// in the payload is ignored.
fn session_id_from_stdin(stdin: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(stdin).ok()?;
    let session_id = value.get("session_id")?.as_str()?;
    is_valid_session_id(session_id).then(|| session_id.to_owned())
}

/// The bytes to write to the terminal, or `None` when the hook must do nothing: an unknown
/// event, or no `HORIZON_PANEL_ID` (Claude running outside Horizon).
fn osc_bytes(event: &str, panel_id: Option<&str>, stdin: &[u8]) -> Option<Vec<u8>> {
    if panel_id.is_none_or(str::is_empty) {
        return None;
    }
    let status = status_for_event(event)?;
    let session_id = session_id_from_stdin(stdin);
    let title = status_osc_title(status, session_id.as_deref());
    Some(format!("\x1b]2;{title}\x07").into_bytes())
}

fn read_stdin() -> Vec<u8> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    // A failed read leaves whatever arrived; the status is sent without a session id.
    let _ = stdin.lock().take(MAX_STDIN_BYTES).read_to_end(&mut bytes);
    bytes
}

#[cfg(unix)]
fn write_to_tty(bytes: &[u8]) {
    use std::io::Write;

    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = tty.write_all(bytes);
    }
}

#[cfg(not(unix))]
fn write_to_tty(_bytes: &[u8]) {}

/// Entry point for `horizon hook <event>`; always exits 0.
pub(crate) fn run(args: &[String]) -> ExitCode {
    let Some(event) = args.first() else {
        return ExitCode::SUCCESS;
    };
    let panel_id = std::env::var("HORIZON_PANEL_ID").ok();
    if panel_id.as_deref().is_none_or(str::is_empty) || status_for_event(event).is_none() {
        return ExitCode::SUCCESS;
    }
    if let Some(bytes) = osc_bytes(event, panel_id.as_deref(), &read_stdin()) {
        write_to_tty(&bytes);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";
    const HOOK_EVENTS: [&str; 6] = ["working", "waiting", "done", "error", "session-start", "session-end"];

    #[test]
    fn hook_cli_parses_session_id_from_stdin_json() {
        let payload =
            format!(r#"{{"session_id":"{SESSION}","cwd":"/repo","prompt":"secret text","hook_event_name":"Stop"}}"#);

        assert_eq!(session_id_from_stdin(payload.as_bytes()).as_deref(), Some(SESSION));
    }

    #[test]
    fn hook_cli_ignores_malformed_stdin() {
        for payload in [
            "",
            "not json",
            "[1,2,3]",
            r#"{"session_id":42}"#,
            r#"{"session_id":"short"}"#,
            r#"{"session_id":"../../etc/passwd-and-more"}"#,
            r#"{"sessionId":"f054f32b-7840-4c95-9f84-e61f37db5f5c"}"#,
        ] {
            assert_eq!(session_id_from_stdin(payload.as_bytes()), None, "payload {payload:?}");
        }
        // Invalid UTF-8 and truncated JSON must not panic either.
        assert_eq!(session_id_from_stdin(&[0xff, 0xfe, 0x00]), None);
        assert_eq!(session_id_from_stdin(br#"{"session_id":"f054f32b-78"#), None);
    }

    #[test]
    fn hook_cli_noop_without_panel_id_env() {
        assert_eq!(osc_bytes("working", None, b"{}"), None);
        assert_eq!(osc_bytes("working", Some(""), b"{}"), None);
        assert!(osc_bytes("working", Some("panel-1"), b"{}").is_some());
    }

    #[test]
    fn unknown_events_do_nothing() {
        assert_eq!(osc_bytes("explode", Some("panel-1"), b"{}"), None);
        assert_eq!(osc_bytes("", Some("panel-1"), b"{}"), None);
        for event in HOOK_EVENTS {
            assert!(osc_bytes(event, Some("panel-1"), b"{}").is_some(), "event {event}");
        }
    }

    #[test]
    fn hook_osc_bytes_match_protocol() {
        let payload = format!(r#"{{"session_id":"{SESSION}"}}"#);

        assert_eq!(
            osc_bytes("working", Some("p1"), payload.as_bytes()),
            Some(format!("\x1b]2;HORIZON_STATUS:working:{SESSION}\x07").into_bytes())
        );
        assert_eq!(
            osc_bytes("done", Some("p1"), b"garbage"),
            Some(b"\x1b]2;HORIZON_STATUS:done\x07".to_vec())
        );
        assert_eq!(
            osc_bytes("session-start", Some("p1"), b"{}"),
            Some(b"\x1b]2;HORIZON_STATUS:done\x07".to_vec())
        );
        assert_eq!(
            osc_bytes("session-end", Some("p1"), b"{}"),
            Some(b"\x1b]2;HORIZON_STATUS:end\x07".to_vec())
        );
    }

    #[test]
    fn the_bytes_written_parse_back_to_the_same_status() {
        let payload = format!(r#"{{"session_id":"{SESSION}"}}"#);
        let bytes = osc_bytes("error", Some("p1"), payload.as_bytes()).expect("bytes");
        let text = String::from_utf8(bytes).expect("utf8");
        let title = text
            .strip_prefix("\x1b]2;")
            .and_then(|rest| rest.strip_suffix('\x07'))
            .expect("osc framing");

        let event = horizon_core::agent_status::parse_status_osc(title).expect("parses");

        assert_eq!(event.state, AgentStatus::Error);
        assert_eq!(event.session_id.as_deref(), Some(SESSION));
    }
}
