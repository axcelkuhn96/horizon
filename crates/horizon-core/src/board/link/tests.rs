use std::path::Path;
use std::time::{Duration, Instant};

use crate::link::{LinkErrorCode, LinkOk, LinkRequest, LinkResponse, SendTarget};
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
        target: SendTarget::panel(target),
        text: text.to_owned(),
        enter,
        from: None,
        from_id: None,
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
        LinkResponse::Ok(LinkOk::sent_to(&panel_local_id(&board, id)))
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
        LinkResponse::Ok(LinkOk::sent_to(&panel_local_id(&board, id)))
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
        LinkResponse::Ok(LinkOk::sent_to(&panel_local_id(&board, id)))
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
        LinkResponse::Ok(LinkOk::sent_to(&panel_local_id(&board, id)))
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

// --- workspace broadcast and ask/reply ---------------------------------------

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};

use crate::WorkspaceId;
use crate::link::{AskTable, LinkEffectKind, MAX_ASKS_PER_ASKER};

/// An executable script standing in for an agent CLI: whatever arguments the launcher
/// adds are ignored.
fn agent_script(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path.display().to_string()
}

fn agent_panel(board: &mut Board, workspace: WorkspaceId, name: &str, script: &str) -> PanelId {
    board
        .create_panel(
            PanelOptions {
                name: Some(name.to_owned()),
                kind: PanelKind::Claude,
                command: Some(script.to_owned()),
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("agent panel should spawn")
}

fn sink_agent(board: &mut Board, workspace: WorkspaceId, dir: &Path, name: &str) -> (PanelId, std::path::PathBuf) {
    let file = dir.join(format!("{name}.in"));
    let script = agent_script(dir, &format!("{name}.sh"), &sink_script(&file, true));
    let id = agent_panel(board, workspace, name, &script);
    assert!(screen_contains(board, id, "ready"), "{name} never became ready");
    (id, file)
}

fn broadcast(board: &mut Board, from_id: Option<String>, text: &str) -> LinkResponse {
    board.handle_link_request(LinkRequest::Send {
        target: SendTarget::AllInWorkspace,
        text: text.to_owned(),
        enter: true,
        from: None,
        from_id,
    })
}

#[test]
fn broadcast_excludes_sender_and_non_agent_panels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let (sender, sender_file) = sink_agent(&mut board, workspace, dir.path(), "sender");
    let (first, first_file) = sink_agent(&mut board, workspace, dir.path(), "first");
    let (second, second_file) = sink_agent(&mut board, workspace, dir.path(), "second");
    let shell = command_panel(&mut board, "shell", "exec cat");
    let other_workspace = board.create_workspace("other");
    let (_elsewhere, elsewhere_file) = sink_agent(&mut board, other_workspace, dir.path(), "elsewhere");

    let response = {
        let from_id = panel_local_id(&board, sender);
        broadcast(&mut board, Some(from_id), "heads up")
    };

    let expected = |ids: &[PanelId]| ids.iter().map(|id| panel_local_id(&board, *id)).collect::<Vec<_>>();
    assert_eq!(
        response,
        LinkResponse::Ok(LinkOk::Sent {
            delivered: expected(&[first, second]),
            skipped: expected(&[shell]),
        })
    );
    let bracketed = "\u{1b}[200~heads up\u{1b}[201~\r";
    assert!(file_equals(&first_file, bracketed));
    assert!(file_equals(&second_file, bracketed));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::fs::read(&sender_file).unwrap_or_default().is_empty(),
        "sender got its own broadcast"
    );
    assert!(
        std::fs::read(&elsewhere_file).unwrap_or_default().is_empty(),
        "other workspace got it"
    );
    board.shutdown_terminal_panels();
}

#[test]
fn broadcast_caps_at_16_targets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let script = agent_script(dir.path(), "idle.sh", "exec cat >/dev/null");
    let sender = agent_panel(&mut board, workspace, "sender", &script);
    for index in 0..18 {
        agent_panel(&mut board, workspace, &format!("peer{index}"), &script);
    }

    let response = {
        let from_id = panel_local_id(&board, sender);
        broadcast(&mut board, Some(from_id), "x")
    };

    let LinkResponse::Ok(LinkOk::Sent { delivered, skipped }) = response else {
        panic!("expected sent, got {response:?}");
    };
    assert_eq!(delivered.len(), super::ask::MAX_BROADCAST_TARGETS);
    assert_eq!(skipped.len(), 2);
    board.shutdown_terminal_panels();
}

#[test]
fn broadcast_reports_delivered_and_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let idle = agent_script(dir.path(), "idle.sh", "exec cat >/dev/null");
    let sender = agent_panel(&mut board, workspace, "sender", &idle);
    let live = agent_panel(&mut board, workspace, "live", &idle);
    let dead_script = agent_script(dir.path(), "dead.sh", "exit 0");
    let dead = agent_panel(&mut board, workspace, "dead", &dead_script);
    assert!(wait_until(|| {
        let panel = board.panel_mut(dead).expect("panel");
        let _ = panel.process_output();
        panel.child_exited()
    }));

    let response = {
        let from_id = panel_local_id(&board, sender);
        broadcast(&mut board, Some(from_id), "x")
    };

    assert_eq!(
        response,
        LinkResponse::Ok(LinkOk::Sent {
            delivered: vec![panel_local_id(&board, live)],
            skipped: vec![panel_local_id(&board, dead)],
        })
    );
    // Without a sender (or alone in the workspace) there is nobody to broadcast to.
    for from_id in [None, Some("nope".to_owned())] {
        assert!(matches!(
            broadcast(&mut board, from_id, "x"),
            LinkResponse::Err {
                code: LinkErrorCode::BadRequest,
                ..
            }
        ));
    }
    board.shutdown_terminal_panels();
}

#[test]
fn broadcast_without_another_agent_is_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let idle = agent_script(dir.path(), "idle.sh", "exec cat >/dev/null");
    let sender = agent_panel(&mut board, workspace, "sender", &idle);
    let _shell = command_panel(&mut board, "shell", "exec cat");

    assert!(matches!(
        {
            let from_id = panel_local_id(&board, sender);
            broadcast(&mut board, Some(from_id), "x")
        },
        LinkResponse::Err {
            code: LinkErrorCode::NotFound,
            ..
        }
    ));
    board.shutdown_terminal_panels();
}

struct AskSetup {
    board: Board,
    asks: AskTable,
    asker: PanelId,
    target: PanelId,
    target_file: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn ask_setup() -> AskSetup {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let (asker, _) = sink_agent(&mut board, workspace, dir.path(), "asker");
    let (target, target_file) = sink_agent(&mut board, workspace, dir.path(), "target");
    AskSetup {
        board,
        asks: AskTable::default(),
        asker,
        target,
        target_file,
        _dir: dir,
    }
}

fn serve(
    setup: &mut AskSetup,
    request: LinkRequest,
) -> (mpsc::Receiver<LinkResponse>, Option<crate::link::LinkEffect>) {
    let abandoned = Arc::new(AtomicBool::new(false));
    serve_watched(setup, request, &abandoned)
}

fn serve_watched(
    setup: &mut AskSetup,
    request: LinkRequest,
    abandoned: &Arc<AtomicBool>,
) -> (mpsc::Receiver<LinkResponse>, Option<crate::link::LinkEffect>) {
    let (sender, receiver) = mpsc::channel();
    let effect = setup
        .board
        .serve_link(&mut setup.asks, request, &sender, abandoned, std::time::Instant::now());
    (receiver, effect)
}

fn ask_request(setup: &AskSetup, from: PanelId, target: &str, text: &str) -> LinkRequest {
    LinkRequest::Ask {
        from: panel_local_id(&setup.board, from),
        target: target.to_owned(),
        text: text.to_owned(),
        timeout_secs: 30,
    }
}

#[test]
fn ask_text_is_sanitized_like_send() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "what\u{1b}[31m port\u{7}?");

    let (receiver, effect) = serve(&mut setup, request);

    assert!(receiver.try_recv().is_err(), "an accepted ask is answered later");
    let effect = effect.expect("effect");
    assert_eq!(effect.kind, LinkEffectKind::Ask);
    assert_eq!(effect.from, Some(setup.asker));
    assert_eq!(effect.to, vec![setup.target]);
    assert_eq!(setup.asks.waiting_on(setup.asker), Some(setup.target));
    let written = |expected: &str| {
        wait_until(|| std::fs::read_to_string(&setup.target_file).is_ok_and(|text| text.contains(expected)))
    };
    assert!(written("what[31m port?"), "{:?}", std::fs::read(&setup.target_file));
    let text = std::fs::read_to_string(&setup.target_file).expect("read");
    assert!(text.starts_with("\u{1b}[200~[ask "), "{text:?}");
    assert!(
        text.contains(" from asker] what[31m port?\n(reply with: horizon link reply "),
        "{text:?}"
    );
    assert!(text.ends_with("\"<answer>\")\u{1b}[201~\r"), "{text:?}");
    assert!(!text.contains('\u{7}'));
    setup.board.shutdown_terminal_panels();
}

fn pending_ask_id(setup: &AskSetup) -> String {
    assert!(wait_until(
        || std::fs::read_to_string(&setup.target_file).is_ok_and(|t| t.contains("(reply with"))
    ));
    let text = std::fs::read_to_string(&setup.target_file).expect("read");
    let start = text.find("[ask ").expect("header") + 5;
    text[start..start + 6].to_owned()
}

#[test]
fn reply_requires_addressee_and_reaches_the_asker() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "ready?");
    let (asker_answer, _) = serve(&mut setup, request);
    let id = pending_ask_id(&setup);
    let reply_from = |setup: &AskSetup, panel: PanelId, text: &str| LinkRequest::Reply {
        from: panel_local_id(&setup.board, panel),
        ask_id: id.clone(),
        text: text.to_owned(),
    };

    // The asker (or anyone else) cannot answer its own question.
    let request = reply_from(&setup, setup.asker, "self-answer");
    let (response, effect) = serve(&mut setup, request);
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::NotAddressee,
            ..
        })
    ));
    assert!(effect.is_none());
    assert!(asker_answer.try_recv().is_err());

    let request = reply_from(&setup, setup.target, "yes\u{1b}[2J");
    let (response, effect) = serve(&mut setup, request);
    assert!(matches!(response.try_recv(), Ok(LinkResponse::Ok(LinkOk::Sent { .. }))));
    let effect = effect.expect("effect");
    assert_eq!(effect.kind, LinkEffectKind::Reply);
    assert_eq!(effect.from, Some(setup.target));
    assert_eq!(effect.to, vec![setup.asker]);
    assert_eq!(
        asker_answer.try_recv().ok(),
        Some(LinkResponse::Ok(LinkOk::Reply {
            from: "target".to_owned(),
            text: "yes".to_owned(),
        }))
    );
    setup.board.shutdown_terminal_panels();
}

#[test]
fn reply_unknown_or_resolved_ask_errors_through_the_board() {
    let mut setup = ask_setup();
    let reply = |setup: &AskSetup, id: &str| LinkRequest::Reply {
        from: panel_local_id(&setup.board, setup.target),
        ask_id: id.to_owned(),
        text: "a".to_owned(),
    };
    let request = reply(&setup, "abcdef");
    let (response, _) = serve(&mut setup, request);
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::UnknownAsk,
            ..
        })
    ));
    setup.board.shutdown_terminal_panels();
}

#[test]
fn ask_to_self_rejected() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "asker", "me?");
    let (response, effect) = serve(&mut setup, request);
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::BadRequest,
            ..
        })
    ));
    assert!(effect.is_none());
    assert!(setup.asks.is_empty());
    setup.board.shutdown_terminal_panels();
}

#[test]
fn ask_to_a_shell_or_from_an_unknown_panel_is_rejected_without_writing() {
    let mut setup = ask_setup();
    let shell = command_panel(&mut setup.board, "shell", "exec cat");
    let request = ask_request(&setup, setup.asker, &panel_local_id(&setup.board, shell), "rm -rf?");
    let (response, _) = serve(&mut setup, request);
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::BadRequest,
            ..
        })
    ));
    let forged = LinkRequest::Ask {
        from: "does-not-exist".to_owned(),
        target: "target".to_owned(),
        text: "hi".to_owned(),
        timeout_secs: 30,
    };
    let (response, _) = serve(&mut setup, forged);
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::BadRequest,
            ..
        })
    ));
    assert!(setup.asks.is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert!(std::fs::read(&setup.target_file).unwrap_or_default().is_empty());
    setup.board.shutdown_terminal_panels();
}

#[test]
fn ask_over_the_per_asker_cap_is_refused() {
    let mut setup = ask_setup();
    let mut waiting = Vec::new();
    for _ in 0..MAX_ASKS_PER_ASKER {
        let request = ask_request(&setup, setup.asker, "target", "q");
        let (receiver, effect) = serve(&mut setup, request);
        assert!(effect.is_some());
        waiting.push(receiver);
    }
    let request = ask_request(&setup, setup.asker, "target", "one too many");
    let (response, effect) = serve(&mut setup, request);
    assert!(effect.is_none());
    assert!(matches!(
        response.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::BadRequest,
            ..
        })
    ));
    setup.board.shutdown_terminal_panels();
}

#[test]
fn closing_target_resolves_pending_ask_with_error() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "still there?");
    let (answer, _) = serve(&mut setup, request);
    assert_eq!(setup.asks.len(), 1);

    setup.board.close_panel(setup.target);
    let ended = setup
        .board
        .resolve_link_asks(&mut setup.asks, std::time::Instant::now());

    assert_eq!(ended.len(), 1);
    assert!(matches!(
        answer.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::AskTargetClosed,
            ..
        })
    ));
    assert!(setup.asks.is_empty());
    setup.board.shutdown_terminal_panels();
}

#[test]
fn closing_the_asker_cancels_its_pending_ask() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "hello?");
    let (answer, _) = serve(&mut setup, request);

    setup.board.close_panel(setup.asker);
    setup
        .board
        .resolve_link_asks(&mut setup.asks, std::time::Instant::now());

    assert!(matches!(
        answer.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::AskCancelled,
            ..
        })
    ));
    setup.board.shutdown_terminal_panels();
}

#[test]
fn an_ask_past_its_deadline_times_out_in_the_sweep() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "slow?");
    let (answer, _) = serve(&mut setup, request);

    let late = std::time::Instant::now() + Duration::from_secs(31);
    let ended = setup.board.resolve_link_asks(&mut setup.asks, late);

    assert_eq!(ended.len(), 1);
    assert!(matches!(
        answer.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::AskTimeout,
            ..
        })
    ));
    setup.board.shutdown_terminal_panels();
}

#[test]
fn agent_panels_in_workspace_lists_live_agent_terminals_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let idle = agent_script(dir.path(), "idle.sh", "exec cat >/dev/null");
    let agent = agent_panel(&mut board, workspace, "agent", &idle);
    let _shell = command_panel(&mut board, "shell", "exec cat");

    assert_eq!(board.agent_panels_in_workspace(workspace), vec![agent]);
    board.shutdown_terminal_panels();
}

#[test]
fn an_ask_whose_client_vanished_is_cancelled_by_the_sweep() {
    let mut setup = ask_setup();
    let request = ask_request(&setup, setup.asker, "target", "anyone?");
    let gone = Arc::new(AtomicBool::new(false));
    let (answer, effect) = serve_watched(&mut setup, request, &gone);
    assert!(effect.is_some());
    assert!(
        setup
            .board
            .resolve_link_asks(&mut setup.asks, std::time::Instant::now())
            .is_empty()
    );
    assert_eq!(setup.asks.len(), 1, "a client that is still waiting keeps its ask");

    gone.store(true, std::sync::atomic::Ordering::Release);
    let ended = setup
        .board
        .resolve_link_asks(&mut setup.asks, std::time::Instant::now());

    assert_eq!(ended.len(), 1);
    assert_eq!(setup.asks.waiting_on(setup.asker), None, "the 'asking...' line is gone");
    assert!(matches!(
        answer.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::AskCancelled,
            ..
        })
    ));
    setup.board.shutdown_terminal_panels();
}

#[test]
fn a_broadcast_without_text_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let (sender, _) = sink_agent(&mut board, workspace, dir.path(), "sender");
    let (_peer, peer_file) = sink_agent(&mut board, workspace, dir.path(), "peer");

    let response = {
        let from_id = panel_local_id(&board, sender);
        broadcast(&mut board, Some(from_id), "  \n ")
    };

    assert!(matches!(
        response,
        LinkResponse::Err {
            code: LinkErrorCode::BadRequest,
            ..
        }
    ));
    std::thread::sleep(Duration::from_millis(200));
    assert!(std::fs::read(&peer_file).unwrap_or_default().is_empty());
    board.shutdown_terminal_panels();
}

// --- agents started by hand inside shell panels (Linux: process-tree detection) ----------

/// Shell panel running `script` through bash (so `exec -a claude ...` can name a process).
#[cfg(target_os = "linux")]
fn shell_panel(board: &mut Board, workspace: WorkspaceId, name: &str, script: &str) -> PanelId {
    board
        .create_panel(
            PanelOptions {
                name: Some(name.to_owned()),
                kind: PanelKind::Shell,
                command: Some("bash".to_owned()),
                args: vec!["-c".to_owned(), script.to_owned()],
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("shell panel should spawn")
}

#[cfg(target_os = "linux")]
fn runs_agent(board: &Board, id: PanelId) -> bool {
    Board::panel_runs_agent(board.panel(id).expect("panel"))
}

#[cfg(target_os = "linux")]
#[test]
fn a_shell_running_claude_is_an_agent_and_a_bare_shell_is_not() {
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    let with_claude = shell_panel(&mut board, workspace, "hand", "exec -a claude cat");
    let bare = shell_panel(&mut board, workspace, "bare", "exec cat");

    assert!(
        wait_until(|| runs_agent(&board, with_claude)),
        "claude was never detected"
    );
    assert!(!runs_agent(&board, bare));
    let LinkResponse::Ok(LinkOk::Panels(panels)) = board.handle_link_request(LinkRequest::List) else {
        panic!("expected panels");
    };
    let flag = |name: &str| {
        panels
            .iter()
            .find(|panel| panel.name == name)
            .map(|panel| panel.is_agent)
    };
    assert_eq!(flag("hand"), Some(true));
    assert_eq!(flag("bare"), Some(false));
    assert_eq!(board.agent_panels_in_workspace(workspace), vec![with_claude]);
    board.shutdown_terminal_panels();
}

#[cfg(target_os = "linux")]
#[test]
fn broadcast_and_ask_reach_claude_started_in_a_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut setup = ask_setup();
    let workspace = setup.board.panel(setup.asker).expect("asker").workspace_id;
    let file = dir.path().join("hand.in");
    let script = format!(
        "printf '\\033[?2004h'; stty raw -echo; echo ready; exec -a claude cat > '{}'",
        file.display()
    );
    let hand = shell_panel(&mut setup.board, workspace, "hand", &script);
    assert!(screen_contains(&mut setup.board, hand, "ready"));
    assert!(
        wait_until(|| runs_agent(&setup.board, hand)),
        "claude was never detected"
    );

    let from_id = panel_local_id(&setup.board, setup.asker);
    let response = broadcast(&mut setup.board, Some(from_id), "to everyone");
    let hand_id = panel_local_id(&setup.board, hand);
    let target_id = panel_local_id(&setup.board, setup.target);
    let LinkResponse::Ok(LinkOk::Sent { delivered, skipped }) = response else {
        panic!("expected sent, got {response:?}");
    };
    assert!(
        delivered.contains(&hand_id) && delivered.contains(&target_id),
        "{delivered:?}"
    );
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(file_contains(&file, "to everyone"), "{:?}", std::fs::read(&file));

    let request = ask_request(&setup, setup.asker, &hand_id, "are you there?");
    let (_answer, effect) = serve(&mut setup, request);
    assert!(effect.is_some(), "an ask to claude in a shell is delivered");
    assert!(file_contains(&file, "are you there?"), "{:?}", std::fs::read(&file));
    setup.board.shutdown_terminal_panels();
}

#[cfg(target_os = "linux")]
fn file_contains(path: &Path, needle: &str) -> bool {
    wait_until(|| std::fs::read_to_string(path).is_ok_and(|text| text.contains(needle)))
}

#[cfg(target_os = "linux")]
#[test]
fn a_shell_that_stopped_running_claude_is_refused_and_ends_a_pending_ask() {
    let mut setup = ask_setup();
    let workspace = setup.board.panel(setup.asker).expect("asker").workspace_id;
    // claude runs for 3 s as a child, then the shell is back at (a stand-in for) its prompt.
    let script = "printf '\\033[?2004h'; stty raw -echo; echo ready; \
                  bash -c 'exec -a claude sleep 3'; exec cat >/dev/null";
    let hand = shell_panel(&mut setup.board, workspace, "hand", script);
    assert!(screen_contains(&mut setup.board, hand, "ready"));
    assert!(
        wait_until(|| runs_agent(&setup.board, hand)),
        "claude was never detected"
    );
    let hand_id = panel_local_id(&setup.board, hand);
    let request = ask_request(&setup, setup.asker, &hand_id, "still there?");
    let (answer, effect) = serve(&mut setup, request);
    assert!(effect.is_some());

    let mut ended = Vec::new();
    assert!(wait_until(|| {
        ended.extend(
            setup
                .board
                .resolve_link_asks(&mut setup.asks, std::time::Instant::now()),
        );
        !ended.is_empty()
    }));

    assert!(matches!(
        answer.try_recv(),
        Ok(LinkResponse::Err {
            code: LinkErrorCode::AskTargetClosed,
            ..
        })
    ));
    assert!(!runs_agent(&setup.board, hand));
    let request = ask_request(&setup, setup.asker, &hand_id, "and now?");
    let (refusal, effect) = serve(&mut setup, request);
    assert!(effect.is_none());
    let Ok(LinkResponse::Err { code, message }) = refusal.try_recv() else {
        panic!("expected a refusal");
    };
    assert_eq!(code, LinkErrorCode::BadRequest);
    assert!(
        !message.contains("send"),
        "the refusal must not point at typing into shells: {message}"
    );
    setup.board.shutdown_terminal_panels();
}

#[test]
fn deliver_text_matches_link_send_behaviour() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, true));
    assert!(screen_contains(&mut board, id, "ready"));
    assert!(board.bracketed_paste_enabled(id));

    board.deliver_text(id, "line1\nline2", true).expect("deliver");

    assert!(
        file_equals(&file, "\u{1b}[200~line1\nline2\u{1b}[201~\r"),
        "{:?}",
        std::fs::read(&file)
    );
}

#[test]
fn deliver_text_without_enter_leaves_the_text_unsent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));
    assert!(!board.bracketed_paste_enabled(id));

    board.deliver_text(id, "a\u{1b}[31mb", false).expect("deliver");
    assert!(
        matches!(
            board.deliver_text(id, "x\ny", false),
            Err(crate::link::LinkError::BadRequest(_))
        ),
        "multi-line text without bracketed paste must not be typed"
    );

    assert!(file_equals(&file, "a[31mb"), "{:?}", std::fs::read(&file));
}

#[test]
fn deliver_text_closed_panel_errors() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "gone", "exit 0");
    assert!(wait_until(|| {
        let panel = board.panel_mut(id).expect("panel");
        let _ = panel.process_output();
        panel.child_exited()
    }));

    assert_eq!(board.deliver_text(id, "x", true), Err(crate::link::LinkError::Closed));
    assert_eq!(
        board.deliver_text(PanelId(9_999), "x", true),
        Err(crate::link::LinkError::Closed)
    );
    assert!(!board.bracketed_paste_enabled(id));
}

fn listed_names(board: &mut Board) -> Vec<String> {
    match board.handle_link_request(LinkRequest::List) {
        LinkResponse::Ok(LinkOk::Panels(panels)) => panels.into_iter().map(|panel| panel.name).collect(),
        other => panic!("expected panels, got {other:?}"),
    }
}

#[test]
fn link_list_reports_current_custom_name() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "spawn name", "exec cat");

    assert!(board.rename_panel(id, "  reviewer  "));

    assert_eq!(listed_names(&mut board), vec!["reviewer".to_owned()]);
    board.shutdown_terminal_panels();
}

#[test]
fn link_resolves_target_by_custom_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("in");
    let mut board = Board::new();
    let id = command_panel(&mut board, "sink", &sink_script(&file, false));
    assert!(screen_contains(&mut board, id, "ready"));
    assert!(board.rename_panel(id, "Reviewer"));

    assert_eq!(
        send(&mut board, "reviewer", "hi", true),
        LinkResponse::Ok(LinkOk::sent_to(&panel_local_id(&board, id)))
    );
    assert!(file_equals(&file, "hi\r"), "{:?}", std::fs::read(&file));
    match send(&mut board, "sink", "nope", true) {
        LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::NotFound),
        other => panic!("the spawn name must stop resolving after a rename, got {other:?}"),
    }
}

#[test]
fn link_ambiguous_custom_name_is_typed_error() {
    let mut board = Board::new();
    let first = command_panel(&mut board, "one", "exec cat");
    let second = command_panel(&mut board, "two", "exec cat");
    assert!(board.rename_panel(first, "worker"));
    assert!(board.rename_panel(second, "Worker"));

    match send(&mut board, "WORKER", "hi", true) {
        LinkResponse::Err { code, message } => {
            assert_eq!(code, LinkErrorCode::Ambiguous);
            assert!(message.contains(&panel_local_id(&board, first)), "{message}");
            assert!(message.contains(&panel_local_id(&board, second)), "{message}");
        }
        other => panic!("expected an ambiguity error, got {other:?}"),
    }
    board.shutdown_terminal_panels();
}

#[test]
fn rejected_rename_keeps_the_name_link_reports() {
    let mut board = Board::new();
    let id = command_panel(&mut board, "stable", "exec cat");

    assert!(!board.rename_panel(id, "   "));
    assert!(!board.rename_panel(id, &"x".repeat(81)));
    assert!(!board.rename_panel(id, "two\nlines"));

    assert_eq!(listed_names(&mut board), vec!["stable".to_owned()]);
    board.shutdown_terminal_panels();
}
