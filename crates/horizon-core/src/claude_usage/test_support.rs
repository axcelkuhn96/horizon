//! Test helpers: a loopback HTTP stub and credential-file fixtures.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub(crate) const TEST_TOKEN: &str = "sk-test-SECRET-token-123";
pub(crate) const FAR_FUTURE_MS: i64 = 4_102_444_800_000;
pub(crate) const USAGE_BODY: &str = r#"{"five_hour":{"utilization":37.0,"resets_at":"2026-11-12T18:00:00Z"},
    "seven_day":{"utilization":12,"resets_at":"2026-11-15T00:00:00Z"}}"#;

/// Loopback server answering every request with a fixed response and recording the raw requests.
pub(crate) struct Stub {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Stub {
    pub(crate) fn new(status: u16, extra_headers: &str, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}/api/oauth/usage", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let response = format!(
            "HTTP/1.1 {status} X\r\n{extra_headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (reqs, flag) = (Arc::clone(&requests), Arc::clone(&stop));
        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &reqs, &response),
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("lock").clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(mut stream: TcpStream, requests: &Mutex<Vec<String>>, response: &str) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
    }
    requests
        .lock()
        .expect("lock")
        .push(String::from_utf8_lossy(&raw).into_owned());
    let _ = stream.write_all(response.as_bytes());
}

/// URL of a loopback port with nothing listening (connection refused).
pub(crate) fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/api/oauth/usage", listener.local_addr().expect("addr"));
    drop(listener);
    url
}

pub(crate) fn write_credentials(dir: &Path, token: &str, expires_at_ms: i64) {
    fs::create_dir_all(dir).expect("mkdir");
    let body = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"sk-refresh-SECRET","expiresAt":{expires_at_ms}}}}}"#
    );
    fs::write(dir.join(".credentials.json"), body).expect("write credentials");
}
