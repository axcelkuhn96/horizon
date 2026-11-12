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

    let client = request(server.path().to_path_buf(), "{\"v\":1,\"op\":\"list\"}\n");
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
