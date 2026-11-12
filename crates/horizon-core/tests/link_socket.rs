#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use horizon_core::link::{LinkOk, LinkResponse, LinkServer};
use horizon_core::{Board, PanelKind, PanelOptions};

fn request(path: PathBuf, line: &'static str) -> thread::JoinHandle<LinkResponse> {
    thread::spawn(move || {
        let mut stream = UnixStream::connect(path).expect("connect");
        stream.write_all(line.as_bytes()).expect("write");
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).expect("read");
        serde_json::from_str(&reply).expect("response json")
    })
}

fn serve_until_done(server: &LinkServer, board: &mut Board, client: &thread::JoinHandle<LinkResponse>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_finished() && Instant::now() < deadline {
        if let Some(pending) = server.try_recv() {
            let response = board.handle_link_request(pending.request);
            pending.reply.send(response).expect("reply");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn link_socket_roundtrip_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = LinkServer::start(dir.path().join("run").join("1.sock")).expect("start");
    let mut board = Board::new();
    let workspace = board.ensure_workspace();
    board
        .create_panel(
            PanelOptions {
                name: Some("worker".to_owned()),
                kind: PanelKind::Command,
                command: Some("sh".to_owned()),
                args: vec!["-c".to_owned(), "exec cat".to_owned()],
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("panel");

    let client = request(server.path().to_path_buf(), "{\"v\":2,\"op\":\"list\"}\n");
    serve_until_done(&server, &mut board, &client);
    let response = client.join().expect("client thread");

    let LinkResponse::Ok(LinkOk::Panels(panels)) = response else {
        panic!("expected panels, got {response:?}");
    };
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].name, "worker");
    assert_eq!(panels[0].kind, "command");
    board.shutdown_terminal_panels();
}

#[test]
fn link_socket_replies_bad_request_to_garbage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = LinkServer::start(dir.path().join("2.sock")).expect("start");
    let mut board = Board::new();
    let client = request(server.path().to_path_buf(), "not json\n");
    serve_until_done(&server, &mut board, &client);
    let response = client.join().expect("client thread");
    assert!(matches!(response, LinkResponse::Err { .. }), "{response:?}");
}

fn command_agent(board: &mut Board, workspace: horizon_core::WorkspaceId, name: &str, script: &std::path::Path) {
    board
        .create_panel(
            PanelOptions {
                name: Some(name.to_owned()),
                kind: PanelKind::Claude,
                command: Some(script.display().to_string()),
                ..PanelOptions::default()
            },
            workspace,
        )
        .expect("agent panel");
}

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// The UI loop in miniature: drain the socket, serve each request, sweep the ask table.
fn serve_asks_until_done(
    server: &LinkServer,
    board: &mut Board,
    asks: &mut horizon_core::link::AskTable,
    clients: &[&thread::JoinHandle<LinkResponse>],
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while clients.iter().any(|client| !client.is_finished()) && Instant::now() < deadline {
        if let Some(pending) = server.try_recv() {
            let abandoned = pending.abandoned_flag();
            let _ = board.serve_link(asks, pending.request, &pending.reply, &abandoned, Instant::now());
        }
        let _ = board.resolve_link_asks(asks, Instant::now());
        thread::sleep(Duration::from_millis(5));
    }
}

fn panel_id_of(board: &Board, name: &str) -> String {
    board
        .panels
        .iter()
        .find(|panel| panel.title == name)
        .map(|panel| panel.local_id.clone())
        .expect("panel by name")
}

fn owned_request(path: PathBuf, line: String) -> thread::JoinHandle<LinkResponse> {
    thread::spawn(move || {
        let mut stream = UnixStream::connect(path).expect("connect");
        stream.write_all(line.as_bytes()).expect("write");
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).expect("read");
        serde_json::from_str(&reply).expect("response json")
    })
}

/// An agent stand-in that enables bracketed paste and records everything typed into it.
fn sink_agent(dir: &std::path::Path, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let file = dir.join(format!("{name}.in"));
    let script = write_script(
        dir,
        &format!("{name}.sh"),
        &format!("printf '\\033[?2004h'; stty raw -echo; exec cat > '{}'", file.display()),
    );
    (script, file)
}

fn read_ask_id(file: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(file)
            && let Some(start) = text.find("[ask ")
            && text.contains("(reply with")
        {
            return text[start + 5..start + 11].to_owned();
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("the ask never reached the target");
}

#[test]
fn link_socket_ask_reply_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = LinkServer::start(dir.path().join("run").join("3.sock")).expect("start");
    let mut board = Board::new();
    let mut asks = horizon_core::link::AskTable::default();
    let workspace = board.ensure_workspace();
    let (alpha_script, _) = sink_agent(dir.path(), "alpha");
    let (beta_script, beta_file) = sink_agent(dir.path(), "beta");
    command_agent(&mut board, workspace, "alpha", &alpha_script);
    command_agent(&mut board, workspace, "beta", &beta_script);
    let alpha = panel_id_of(&board, "alpha");
    let beta = panel_id_of(&board, "beta");

    let asker = owned_request(
        server.path().to_path_buf(),
        format!(
            "{{\"v\":2,\"op\":\"ask\",\"from\":\"{alpha}\",\"target\":\"beta\",\"text\":\"port?\",\"timeout_secs\":30}}\n"
        ),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while asks.is_empty() && Instant::now() < deadline {
        if let Some(pending) = server.try_recv() {
            let abandoned = pending.abandoned_flag();
            let _ = board.serve_link(&mut asks, pending.request, &pending.reply, &abandoned, Instant::now());
        }
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(asks.len(), 1, "the ask never reached the table");
    let ask_id = read_ask_id(&beta_file);
    thread::sleep(Duration::from_millis(200));
    assert!(!asker.is_finished(), "an ask must wait for its reply");

    let replier = owned_request(
        server.path().to_path_buf(),
        format!("{{\"v\":2,\"op\":\"reply\",\"from\":\"{beta}\",\"ask_id\":\"{ask_id}\",\"text\":\"8080\"}}\n"),
    );
    serve_asks_until_done(&server, &mut board, &mut asks, &[&asker, &replier]);

    assert!(matches!(
        replier.join().expect("replier"),
        LinkResponse::Ok(LinkOk::Sent { .. })
    ));
    assert_eq!(
        asker.join().expect("asker"),
        LinkResponse::Ok(LinkOk::Reply {
            from: "beta".to_owned(),
            text: "8080".to_owned(),
        })
    );
    assert!(asks.is_empty());
    board.shutdown_terminal_panels();
}

#[test]
fn link_socket_ask_times_out_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = LinkServer::start(dir.path().join("run").join("4.sock")).expect("start");
    let mut board = Board::new();
    let mut asks = horizon_core::link::AskTable::default();
    let workspace = board.ensure_workspace();
    let (alpha_script, _) = sink_agent(dir.path(), "alpha");
    let (beta_script, _) = sink_agent(dir.path(), "beta");
    command_agent(&mut board, workspace, "alpha", &alpha_script);
    command_agent(&mut board, workspace, "beta", &beta_script);
    let alpha = panel_id_of(&board, "alpha");

    let asker = owned_request(
        server.path().to_path_buf(),
        format!(
            "{{\"v\":2,\"op\":\"ask\",\"from\":\"{alpha}\",\"target\":\"beta\",\"text\":\"anyone?\",\"timeout_secs\":5}}\n"
        ),
    );
    let started = Instant::now();
    serve_asks_until_done(&server, &mut board, &mut asks, &[&asker]);

    let response = asker.join().expect("asker");
    assert!(
        matches!(
            response,
            LinkResponse::Err {
                code: horizon_core::link::LinkErrorCode::AskTimeout,
                ..
            }
        ),
        "{response:?}"
    );
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(asks.is_empty(), "the timed-out ask must leave the table");
    board.shutdown_terminal_panels();
}
