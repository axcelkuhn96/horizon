//! Status transitions through the public API: OSC parsing, the state machine and the
//! live-session file feeding the same machine.

use horizon_core::AttentionSeverity;
use horizon_core::agent_status::{
    AgentStatus, AgentStatusState, LiveSessionRead, LiveSessionStatus, LiveSessionTracker, StatusAttention,
    StatusEvent, parse_status_osc, read_live_session_if_changed,
};

const SESSION: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";

fn apply(state: &mut AgentStatusState, osc: &str, focused: bool) -> StatusAttention {
    let event = parse_status_osc(osc).expect("valid status osc");
    state.apply(event, focused)
}

#[test]
fn a_prompt_cycle_from_hooks_notifies_once_when_unfocused() {
    let mut state = AgentStatusState::default();

    assert_eq!(apply(&mut state, "HORIZON_STATUS:idle", false), StatusAttention::None);
    assert_eq!(
        apply(&mut state, "HORIZON_STATUS:working", false),
        StatusAttention::None
    );
    // PreToolUse repeats `working` on every tool call.
    assert_eq!(
        apply(&mut state, "HORIZON_STATUS:working", false),
        StatusAttention::None
    );
    assert_eq!(
        apply(&mut state, "HORIZON_STATUS:done", false),
        StatusAttention::Notify {
            severity: AttentionSeverity::Medium
        }
    );
    assert_eq!(apply(&mut state, "HORIZON_STATUS:done", false), StatusAttention::None);
    assert!(!state.seen());
    assert_eq!(apply(&mut state, "HORIZON_STATUS:end", false), StatusAttention::None);
    assert_eq!(state.status(), AgentStatus::Unknown);
}

#[test]
fn the_live_session_file_drives_the_same_machine_without_clobbering_waiting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    let write = |status: &str| {
        std::fs::write(
            sessions.join("77.json"),
            format!(r#"{{"pid":77,"sessionId":"{SESSION}","name":"expia-11","status":"{status}"}}"#),
        )
        .expect("write session file");
    };
    let mut tracker = LiveSessionTracker::default();
    let mut state = AgentStatusState::default();

    write("busy");
    let LiveSessionRead::Fresh { info, .. } = read_live_session_if_changed(dir.path(), 77, None) else {
        panic!("session file must be readable");
    };
    assert_eq!(info.status, LiveSessionStatus::Busy);
    state.apply(tracker.observe(&info).expect("working"), false);
    assert_eq!(state.status(), AgentStatus::Working);
    assert_eq!(state.session_id(), Some(SESSION));

    // The permission hook says Waiting; the file still says busy: nothing overwrites it.
    state.apply(parse_status_osc("HORIZON_STATUS:waiting").expect("waiting"), false);
    assert!(tracker.observe(&info).is_none());
    assert_eq!(state.status(), AgentStatus::Waiting);

    write("idle");
    let LiveSessionRead::Fresh { info, .. } = read_live_session_if_changed(dir.path(), 77, None) else {
        panic!("session file must be readable");
    };
    let done: StatusEvent = tracker.observe(&info).expect("done");
    state.apply(done, false);
    assert_eq!(state.status(), AgentStatus::Done);
}
