use std::path::Path;
use std::time::{Duration, Instant};

use crate::link::{LinkErrorCode, LinkOk, LinkRequest, LinkResponse};
use crate::panel::{PanelId, PanelKind, PanelOptions};
use crate::runtime_state::{PanelState, RuntimeState, WorkspaceState};

use super::super::Board;

fn command_panel(board: &mut Board, name: &str, script: &str) -> PanelId {
    let workspace = board.ensure_workspace();
    board
        .create_panel(
            PanelOptions {
                name: Some(name.to_owned()),
                kind: PanelKind::Command,
                command: Some("sh".to_owned()),
                args: vec!["-c".to_owned(), script.to_owned()],
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("panel should spawn")
}

fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn screen_contains(board: &mut Board, id: PanelId, needle: &str) -> bool {
    wait_until(|| {
        let panel = board.panel_mut(id).expect("panel");
        let _ = panel.process_output();
        panel
            .terminal()
            .is_some_and(|terminal| terminal.last_lines_text(24).contains(needle))
    })
}

fn file_equals(path: &Path, expected: &str) -> bool {
    wait_until(|| std::fs::read(path).is_ok_and(|bytes| bytes == expected.as_bytes()))
}

/// Raw-mode `cat` into `file`, announcing readiness; optionally enables bracketed paste first.
fn sink_script(file: &Path, bracketed: bool) -> String {
    let paste = if bracketed { "printf '\\033[?2004h'; " } else { "" };
    format!("{paste}stty raw -echo; echo ready; exec cat > '{}'", file.display())
}

fn send(board: &mut Board, target: &str, text: &str, enter: bool) -> LinkResponse {
    board.handle_link_request(LinkRequest::Send {
        target: target.to_owned(),
        text: text.to_owned(),
        enter,
        from: None,
    })
}

fn panel_local_id(board: &Board, id: PanelId) -> String {
    board.panel(id).expect("panel").local_id.clone()
}

#[test]
fn board_send_writes_text_and_cr_to_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));

    assert_eq!(
        send(&mut board, "sink", "echo hi", true),
        LinkResponse::Ok(LinkOk::Sent)
    );
    assert!(file_equals(&file, "echo hi\r"), "{:?}", std::fs::read(&file));
}

#[test]
fn board_send_no_enter_omits_cr() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));
    let local_id = panel_local_id(&board, id);

    assert_eq!(
        send(&mut board, &local_id, "partial", false),
        LinkResponse::Ok(LinkOk::Sent)
    );
    assert!(file_equals(&file, "partial"), "{:?}", std::fs::read(&file));
}

#[test]
fn board_send_sanitizes_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));

    let _ = send(&mut board, "sink", "a\u{1b}[31mb\u{3}c\u{7f}d", false);
    assert!(file_equals(&file, "a[31mbcd"), "{:?}", std::fs::read(&file));
}

#[test]
fn board_send_uses_bracketed_paste_when_enabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, true));
    assert!(screen_contains(&mut board, id, "ready"));

    let _ = send(&mut board, "sink", "line1\nline2", true);
    assert!(
        file_equals(&file, "\u{1b}[200~line1\nline2\u{1b}[201~\r"),
        "{:?}",
        std::fs::read(&file)
    );
}

#[test]
fn board_send_refuses_multiline_without_enter_when_paste_is_not_bracketed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));

    match send(&mut board, "sink", "line1\nline2", false) {
        LinkResponse::Err { code, message } => {
            assert_eq!(code, LinkErrorCode::BadRequest);
            assert!(message.contains("multi-line"), "{message}");
        }
        other @ LinkResponse::Ok(_) => panic!("expected bad_request, got {other:?}"),
    }
    // A single line still goes through.
    assert_eq!(
        send(&mut board, "sink", "one line", false),
        LinkResponse::Ok(LinkOk::Sent)
    );
    assert!(file_equals(&file, "one line"), "{:?}", std::fs::read(&file));
}

#[test]
fn board_send_allows_multiline_without_enter_under_bracketed_paste() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, true));
    assert!(screen_contains(&mut board, id, "ready"));

    assert_eq!(
        send(&mut board, "sink", "line1\nline2", false),
        LinkResponse::Ok(LinkOk::Sent)
    );
    assert!(
        file_equals(&file, "\u{1b}[200~line1\nline2\u{1b}[201~"),
        "{:?}",
        std::fs::read(&file)
    );
}

#[test]
fn board_read_returns_plain_last_lines() {
    let mut board = Board::new();
    let id = command_panel(
        &mut board,
        "reader",
        "printf 'l1\\nl2\\n\\033[31mred\\033[0m l3\\nl4\\nl5\\n'; exec cat",
    );
    assert!(screen_contains(&mut board, id, "l5"));

    let response = board.handle_link_request(LinkRequest::Read {
        target: "reader".to_owned(),
        lines: 3,
    });
    assert_eq!(response, LinkResponse::Ok(LinkOk::Text("red l3\nl4\nl5".to_owned())));
}

#[test]
fn board_closed_panel_returns_closed_error() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "gone", "exit 0");
    assert!(wait_until(|| {
        let panel = board.panel_mut(id).expect("panel");
        let _ = panel.process_output();
        panel.child_exited()
    }));

    match send(&mut board, "gone", "x", true) {
        LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::Closed),
        other @ LinkResponse::Ok(_) => panic!("expected closed error, got {other:?}"),
    }
}

#[test]
fn board_send_rejects_text_over_the_request_cap_without_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));

    let big = "a".repeat(crate::link::MAX_REQUEST_BYTES + 1);
    match send(&mut board, "sink", &big, true) {
        LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::TooLarge),
        other @ LinkResponse::Ok(_) => panic!("expected too_large, got {other:?}"),
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::fs::read(&file).unwrap_or_default().is_empty(),
        "oversized text must not reach the PTY"
    );
}

#[test]
fn board_unknown_target_returns_not_found() {
    let mut board = Board::new();
    let _ = command_panel(&mut board, "one", "exec cat");
    match send(&mut board, "nope", "x", true) {
        LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::NotFound),
        other @ LinkResponse::Ok(_) => panic!("expected not_found, got {other:?}"),
    }
}

#[test]
fn board_list_reports_terminal_panels_only() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "lister", "exec cat");
    let workspace = board.ensure_workspace();
    board
        .create_panel(
            PanelOptions {
                kind: PanelKind::Editor,
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("editor");

    let LinkResponse::Ok(LinkOk::Panels(panels)) = board.handle_link_request(LinkRequest::List) else {
        panic!("expected panels");
    };
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].id, panel_local_id(&board, id));
    assert_eq!(panels[0].name, "lister");
    assert_eq!(panels[0].kind, "command");
    assert!(!panels[0].is_agent);
}

#[test]
fn spawn_and_restart_inject_horizon_env() {
    let mut board = Board::new();
    board.set_link_socket(Some("/run/test/horizon/9.sock".into()));
    let script = "echo \"V:$HORIZON_PANEL_ID|$HORIZON_PANEL_NAME|$HORIZON_SOCKET\"; exec cat";
    let id = command_panel(&mut board, "envcheck", script);
    let expected = format!("V:{}|envcheck|/run/test/horizon/9.sock", panel_local_id(&board, id));
    assert!(screen_contains(&mut board, id, &expected));

    board.set_link_socket(Some("/run/test/horizon/10.sock".into()));
    board.restart_panel(id).expect("restart");
    let expected = format!("V:{}|envcheck|/run/test/horizon/10.sock", panel_local_id(&board, id));
    assert!(screen_contains(&mut board, id, &expected));
}

#[test]
fn restored_panels_get_horizon_socket() {
    let script = "echo \"R:$HORIZON_SOCKET\"; exec cat";
    let state = RuntimeState {
        workspaces: vec![WorkspaceState {
            local_id: "w".to_owned(),
            name: "W".to_owned(),
            panels: vec![PanelState {
                local_id: "p".to_owned(),
                name: "restored".to_owned(),
                kind: PanelKind::Command,
                command: Some("sh".to_owned()),
                args: vec!["-c".to_owned(), script.to_owned()],
                ..PanelState::default()
            }],
            ..WorkspaceState::default()
        }],
        ..RuntimeState::default()
    };
    let socket = Path::new("/run/test/horizon/11.sock");
    let mut board = Board::from_runtime_state_with_link_socket(&state, None, Some(socket)).expect("restore");
    assert_eq!(board.link_socket(), Some(socket));
    let id = board.panels.first().expect("panel").id;
    assert!(screen_contains(&mut board, id, "R:/run/test/horizon/11.sock"));
}

#[test]
fn board_list_strips_escapes_from_names() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "plain", "exec cat");
    board.panel_mut(id).expect("panel").title = "ti\u{1b}[31mtle\u{1b}]0;x\u{7}".to_owned();
    let workspace = board.panel(id).expect("panel").workspace_id;
    board.workspace_mut(workspace).expect("workspace").name = "ws\u{1b}[2Kname".to_owned();

    let LinkResponse::Ok(LinkOk::Panels(panels)) = board.handle_link_request(LinkRequest::List) else {
        panic!("expected panels");
    };
    assert_eq!(panels[0].name, "title");
    assert_eq!(panels[0].workspace, "wsname");
}
