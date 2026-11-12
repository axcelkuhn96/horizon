use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::PendingRequest;
use crate::Waker;
use crate::link::{LinkError, LinkResponse, MAX_REQUEST_BYTES, parse_request_line};

const DIR_MODE: u32 = 0o700;
const SOCKET_MODE: u32 = 0o600;
/// Whole-request deadline (not per read), so trickling bytes cannot hold a thread.
const READ_DEADLINE: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const REPLY_POLL: Duration = Duration::from_millis(50);
/// Connections serving a request at once. Connections parked on an `ask` do not count
/// (see [`MAX_HELD_ASKS`]), so waiting asks cannot starve `list`/`send`/`read`.
const MAX_CONNECTIONS: usize = 16;
/// Extra connections that may sit waiting for an ask reply (matches the ask table's cap).
const MAX_HELD_ASKS: usize = crate::link::MAX_ASKS_TOTAL;
/// Slack past an ask's own timeout before its connection gives up by itself, so the UI's
/// timeout error normally wins.
const ASK_WAIT_SLACK: Duration = Duration::from_secs(2);
const DRAIN_LIMIT: usize = 4 * MAX_REQUEST_BYTES;
/// Whole-drain deadline after the answer is written, so a client that keeps sending
/// cannot hold a connection slot (nor block shutdown).
const DRAIN_DEADLINE: Duration = Duration::from_millis(500);
/// Longest single blocking read, so the `stop` flag is observed promptly.
const STOP_POLL: Duration = Duration::from_millis(100);

fn io_error(context: &str, error: &io::Error) -> LinkError {
    LinkError::Io(format!("{context}: {error}"))
}

/// Accepts connections on a private unix socket and forwards parsed requests.
///
/// Dropping the server stops the accept loop, joins every connection thread and removes
/// the socket file.
pub struct LinkServer {
    path: PathBuf,
    requests: mpsc::Receiver<PendingRequest>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl LinkServer {
    /// Binds `path` (parent directory `0700`, socket `0600`), replacing a stale socket
    /// left by a dead process.
    ///
    /// # Errors
    ///
    /// `Io` when the directory or socket cannot be prepared, or a live server already
    /// owns `path`.
    pub fn start(path: PathBuf) -> Result<Self, LinkError> {
        Self::start_with_waker(path, None)
    }

    /// Like [`Self::start`], calling `waker` after each request is forwarded to the UI
    /// channel so an idle UI drains it at once instead of on its next frame.
    ///
    /// # Errors
    ///
    /// Same as [`Self::start`].
    pub fn start_with_waker(path: PathBuf, waker: Option<Waker>) -> Result<Self, LinkError> {
        let dir = path
            .parent()
            .ok_or_else(|| LinkError::Io("socket path has no parent directory".to_owned()))?;
        prepare_dir(dir)?;
        sweep_stale_sockets(dir);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.file_type().is_socket() || !is_stale(&path) {
                return Err(LinkError::Io(format!("{} is already in use", path.display())));
            }
            fs::remove_file(&path).map_err(|error| io_error("remove stale socket", &error))?;
        }
        let listener = UnixListener::bind(&path).map_err(|error| io_error("bind socket", &error))?;
        let setup = fs::set_permissions(&path, fs::Permissions::from_mode(SOCKET_MODE))
            .and_then(|()| listener.set_nonblocking(true));
        if let Err(error) = setup {
            let _ = fs::remove_file(&path);
            return Err(io_error("configure socket", &error));
        }
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let accept = thread::Builder::new().name("horizon-link-accept".to_owned()).spawn({
            let stop = Arc::clone(&stop);
            move || accept_loop(&listener, &sender, &stop, waker.as_ref())
        });
        let accept = match accept {
            Ok(handle) => handle,
            Err(error) => {
                let _ = fs::remove_file(&path);
                return Err(io_error("spawn accept thread", &error));
            }
        };
        Ok(Self {
            path,
            requests,
            stop,
            accept: Some(accept),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Next request waiting for the UI thread, if any.
    #[must_use]
    pub fn try_recv(&self) -> Option<PendingRequest> {
        self.requests.try_recv().ok()
    }
}

impl Drop for LinkServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// Creates `dir` if needed and forces mode `0700`; refuses a symlinked directory.
fn prepare_dir(dir: &Path) -> Result<(), LinkError> {
    fs::create_dir_all(dir).map_err(|error| io_error("create socket directory", &error))?;
    let meta = fs::symlink_metadata(dir).map_err(|error| io_error("inspect socket directory", &error))?;
    if !meta.is_dir() {
        return Err(LinkError::Io(format!("{} is not a directory", dir.display())));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE))
        .map_err(|error| io_error("restrict socket directory", &error))
}

/// A socket file nobody listens on: connecting is refused.
fn is_stale(path: &Path) -> bool {
    matches!(
        UnixStream::connect(path),
        Err(ref error) if error.kind() == io::ErrorKind::ConnectionRefused
    )
}

/// Removes `<pid>.sock` files of dead instances from `dir`.
fn sweep_stale_sockets(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_pid_socket = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".sock"))
            .is_some_and(|stem| !stem.is_empty() && stem.bytes().all(|byte| byte.is_ascii_digit()));
        let is_socket = entry.file_type().is_ok_and(|kind| kind.is_socket());
        if is_pid_socket && is_socket && is_stale(&path) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn accept_loop(
    listener: &UnixListener,
    sender: &mpsc::Sender<PendingRequest>,
    stop: &Arc<AtomicBool>,
    waker: Option<&Waker>,
) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let held = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        let (finished, running): (Vec<_>, Vec<_>) = workers.into_iter().partition(JoinHandle::is_finished);
        workers = running;
        for worker in finished {
            let _ = worker.join();
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let parked = held.load(Ordering::Acquire);
                if workers.len().saturating_sub(parked) >= MAX_CONNECTIONS
                    || workers.len() >= MAX_CONNECTIONS + MAX_HELD_ASKS
                {
                    let busy = error_response(&LinkError::BadRequest("server busy".to_owned()));
                    write_response(&mut stream, &busy);
                    continue;
                }
                let sender = sender.clone();
                let stop = Arc::clone(stop);
                let waker = waker.cloned();
                let held = Arc::clone(&held);
                match thread::Builder::new()
                    .name("horizon-link-conn".to_owned())
                    .spawn(move || handle_connection(stream, &sender, &stop, waker.as_ref(), &held))
                {
                    Ok(worker) => workers.push(worker),
                    Err(error) => tracing::warn!("link: cannot spawn connection thread: {error}"),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL_INTERVAL),
            Err(error) => {
                tracing::debug!("link: accept failed: {error}");
                thread::sleep(POLL_INTERVAL);
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn error_response(error: &LinkError) -> LinkResponse {
    LinkResponse::Err {
        code: error.code(),
        message: error.to_string(),
    }
}

fn handle_connection(
    mut stream: UnixStream,
    sender: &mpsc::Sender<PendingRequest>,
    stop: &AtomicBool,
    waker: Option<&Waker>,
    held: &AtomicUsize,
) {
    let response = match read_request_line(&mut stream, stop) {
        Ok(None) => return,
        Ok(Some(line)) => match parse_request_line(&line) {
            Ok(request) => forward(request, sender, stop, waker, held),
            Err(error) => error_response(&error),
        },
        Err(error) => error_response(&error),
    };
    write_response(&mut stream, &response);
    // Let the client read the answer even when it is still sending (e.g. an oversized
    // line): close our write side, then discard what is left instead of resetting.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    drain(&mut stream, stop);
}

/// Reads one `\n`-terminated line of at most [`MAX_REQUEST_BYTES`] bytes, bounded in size
/// and in total time, and abandoned when `stop` is set. `None` when the peer closed
/// without sending anything; a bare `\n` is an (empty) line and gets a parse error.
fn read_request_line(stream: &mut UnixStream, stop: &AtomicBool) -> Result<Option<Vec<u8>>, LinkError> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut line = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut terminated = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(LinkError::Io("link server is shutting down".to_owned()));
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| LinkError::BadRequest("timed out reading request".to_owned()))?;
        stream
            .set_read_timeout(Some(remaining.min(STOP_POLL)))
            .map_err(|error| io_error("set read timeout", &error))?;
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let data = &chunk[..read];
                let newline = data.iter().position(|&byte| byte == b'\n');
                line.extend_from_slice(&data[..newline.unwrap_or(read)]);
                if line.len() > MAX_REQUEST_BYTES {
                    return Err(LinkError::TooLarge);
                }
                if newline.is_some() {
                    terminated = true;
                    break;
                }
            }
            // Poll slice elapsed: loop to re-check `stop` and the overall deadline.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(io_error("read request", &error)),
        }
    }
    Ok((terminated || !line.is_empty()).then_some(line))
}

/// Counts a connection as parked on an ask for as long as it lives.
struct HeldGuard<'a>(Option<&'a AtomicUsize>);

impl<'a> HeldGuard<'a> {
    fn new(held: &'a AtomicUsize, parked: bool) -> Self {
        if parked {
            held.fetch_add(1, Ordering::AcqRel);
        }
        Self(parked.then_some(held))
    }
}

impl Drop for HeldGuard<'_> {
    fn drop(&mut self) {
        if let Some(held) = self.0 {
            held.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// How long the connection waits for the UI's answer: a short bound for ordinary requests,
/// the ask's own timeout (plus slack) for an `ask`, which the UI answers only on reply.
fn reply_wait(request: &crate::link::LinkRequest) -> Duration {
    match request {
        crate::link::LinkRequest::Ask { timeout_secs, .. } => Duration::from_secs(*timeout_secs) + ASK_WAIT_SLACK,
        _ => REPLY_TIMEOUT,
    }
}

/// Hands the request to the UI thread and waits (bounded) for its answer.
fn forward(
    request: crate::link::LinkRequest,
    sender: &mpsc::Sender<PendingRequest>,
    stop: &AtomicBool,
    waker: Option<&Waker>,
    held: &AtomicUsize,
) -> LinkResponse {
    let wait = reply_wait(&request);
    let ask_secs = match &request {
        crate::link::LinkRequest::Ask { timeout_secs, .. } => Some(*timeout_secs),
        _ => None,
    };
    let _parked = HeldGuard::new(held, matches!(request, crate::link::LinkRequest::Ask { .. }));
    let (reply, answer) = mpsc::channel();
    let abandoned = Arc::new(AtomicBool::new(false));
    let pending = PendingRequest {
        request,
        reply,
        abandoned: Arc::clone(&abandoned),
    };
    if sender.send(pending).is_err() {
        return error_response(&LinkError::Io("link server is shutting down".to_owned()));
    }
    if let Some(waker) = waker {
        waker();
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        match answer.recv_timeout(REPLY_POLL) {
            Ok(response) => return response,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    // The client is about to be told "no answer": the UI side must not run it later.
    abandoned.store(true, Ordering::Release);
    match ask_secs {
        Some(secs) => error_response(&LinkError::AskTimeout(secs)),
        None => error_response(&LinkError::Io("no answer from Horizon".to_owned())),
    }
}

fn write_response(stream: &mut UnixStream, response: &LinkResponse) {
    let Ok(mut bytes) = serde_json::to_vec(response) else {
        return;
    };
    bytes.push(b'\n');
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    if let Err(error) = stream.write_all(&bytes) {
        tracing::debug!("link: failed to write response: {error}");
    }
}

fn drain(stream: &mut UnixStream, stop: &AtomicBool) {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    let mut remaining = DRAIN_LIMIT;
    let mut chunk = [0_u8; 4096];
    while remaining > 0 && !stop.load(Ordering::Relaxed) {
        let Some(left) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        else {
            return;
        };
        if stream.set_read_timeout(Some(left.min(STOP_POLL))).is_err() {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(read) => remaining = remaining.saturating_sub(read),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use super::*;
    use crate::link::{LinkErrorCode, LinkOk, LinkRequest};

    fn temp_socket(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("horizon").join(name);
        (dir, path)
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    fn exchange(path: &Path, bytes: &[u8]) -> LinkResponse {
        let mut stream = UnixStream::connect(path).expect("connect");
        let _ = stream.write_all(bytes);
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).expect("read reply");
        serde_json::from_str(&reply).expect("response json")
    }

    #[test]
    fn server_sets_modes_0700_0600() {
        let (_guard, path) = temp_socket("1.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        assert_eq!(mode(path.parent().expect("parent")), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(server.path(), path);
    }

    #[test]
    fn server_removes_socket_on_drop() {
        let (_guard, path) = temp_socket("2.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn server_replaces_stale_socket_but_not_live_or_regular_files() {
        let (_guard, path) = temp_socket("3.sock");
        let dead = UnixListener::bind({
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            &path
        })
        .expect("bind");
        drop(dead);
        assert!(path.exists(), "dropped listener leaves the socket file");
        let server = LinkServer::start(path.clone()).expect("replaces stale");
        assert!(
            LinkServer::start(path.clone()).is_err(),
            "live server must not be replaced"
        );
        drop(server);

        fs::write(&path, b"not a socket").expect("write file");
        assert!(LinkServer::start(path.clone()).is_err());
        assert!(path.exists(), "regular file must be left alone");
    }

    #[test]
    fn server_rejects_oversized_line() {
        let (_guard, path) = temp_socket("4.sock");
        let _server = LinkServer::start(path.clone()).expect("start");
        let mut big = vec![b'a'; MAX_REQUEST_BYTES + 100];
        big.push(b'\n');
        match exchange(&path, &big) {
            LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::TooLarge),
            other @ LinkResponse::Ok(_) => panic!("expected too_large, got {other:?}"),
        }
    }

    #[test]
    fn server_rejects_oversized_line_without_newline() {
        let (_guard, path) = temp_socket("5.sock");
        let _server = LinkServer::start(path.clone()).expect("start");
        let big = vec![b'a'; MAX_REQUEST_BYTES + 1];
        match exchange(&path, &big) {
            LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::TooLarge),
            other @ LinkResponse::Ok(_) => panic!("expected too_large, got {other:?}"),
        }
    }

    #[test]
    fn waker_is_called_once_per_forwarded_request_and_not_for_rejected_ones() {
        use std::sync::atomic::AtomicUsize;

        let (_guard, path) = temp_socket("20.sock");
        let calls = Arc::new(AtomicUsize::new(0));
        let waker: Waker = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let server = LinkServer::start_with_waker(path.clone(), Some(waker)).expect("start");
        assert!(matches!(
            exchange(&path, b"{\"v\":9,\"op\":\"list\"}\n"),
            LinkResponse::Err { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "rejected requests never reach the UI");

        let client_path = path.clone();
        let client = thread::spawn(move || exchange(&client_path, b"{\"v\":1,\"op\":\"list\"}\n"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let pending = server.try_recv().expect("request was forwarded before the wake");
        pending
            .reply
            .send(LinkResponse::Ok(LinkOk::Panels(Vec::new())))
            .expect("reply");
        client.join().expect("client");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn server_answers_bad_request_and_forwards_valid_ones() {
        let (_guard, path) = temp_socket("6.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        match exchange(&path, b"{\"v\":9,\"op\":\"list\"}\n") {
            LinkResponse::Err { code, .. } => assert_eq!(code, LinkErrorCode::VersionMismatch),
            other @ LinkResponse::Ok(_) => panic!("expected version_mismatch, got {other:?}"),
        }

        let client_path = path.clone();
        let client = thread::spawn(move || exchange(&client_path, b"{\"v\":1,\"op\":\"list\"}\n"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(pending) = server.try_recv() {
                assert_eq!(pending.request, LinkRequest::List);
                pending
                    .reply
                    .send(LinkResponse::Ok(LinkOk::Panels(Vec::new())))
                    .expect("reply");
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            client.join().expect("client"),
            LinkResponse::Ok(LinkOk::Panels(Vec::new()))
        );
    }

    #[test]
    fn server_times_out_silent_clients() {
        let (_guard, path) = temp_socket("7.sock");
        let _server = LinkServer::start(path.clone()).expect("start");
        let stream = UnixStream::connect(&path).expect("connect");
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).expect("read reply");
        let response: LinkResponse = serde_json::from_str(&reply).expect("response json");
        assert!(matches!(
            response,
            LinkResponse::Err {
                code: LinkErrorCode::BadRequest,
                ..
            }
        ));
    }

    /// Connects, sends `prefix`, then keeps trickling one byte every 50 ms for up to 8 s.
    fn trickle(path: PathBuf, prefix: Vec<u8>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let Ok(mut stream) = UnixStream::connect(&path) else {
                return;
            };
            if stream.write_all(&prefix).is_err() {
                return;
            }
            let end = Instant::now() + Duration::from_secs(8);
            while Instant::now() < end {
                if stream.write_all(b"a").is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
    }

    fn drop_duration(server: LinkServer) -> Duration {
        let started = Instant::now();
        drop(server);
        started.elapsed()
    }

    #[test]
    fn drop_does_not_wait_for_a_client_trickling_after_the_answer() {
        let (_guard, path) = temp_socket("8.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        let mut oversized = vec![b'a'; MAX_REQUEST_BYTES + 100];
        oversized.push(b'\n');
        let client = trickle(path, oversized);
        thread::sleep(Duration::from_millis(300));
        let elapsed = drop_duration(server);
        assert!(elapsed < Duration::from_secs(2), "drop blocked for {elapsed:?}");
        client.join().expect("client");
    }

    #[test]
    fn drop_does_not_wait_for_a_client_trickling_a_request() {
        let (_guard, path) = temp_socket("9.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        let client = trickle(path, b"{".to_vec());
        thread::sleep(Duration::from_millis(300));
        let elapsed = drop_duration(server);
        assert!(elapsed < Duration::from_secs(1), "drop blocked for {elapsed:?}");
        client.join().expect("client");
    }

    #[test]
    fn pending_request_is_abandoned_once_the_server_stops_waiting() {
        let (_guard, path) = temp_socket("10.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        let client_path = path.clone();
        let client = thread::spawn(move || exchange(&client_path, b"{\"v\":1,\"op\":\"list\"}\n"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let pending = loop {
            if let Some(pending) = server.try_recv() {
                break pending;
            }
            assert!(Instant::now() < deadline, "request never arrived");
            thread::sleep(Duration::from_millis(5));
        };
        assert!(!pending.is_abandoned(), "still waiting for the answer");
        drop(server);
        assert!(pending.is_abandoned(), "client was told 'no answer'");
        assert!(matches!(
            client.join().expect("client"),
            LinkResponse::Err {
                code: LinkErrorCode::BadRequest,
                ..
            }
        ));
    }

    #[test]
    fn connections_over_the_cap_get_a_busy_reply() {
        let (_guard, path) = temp_socket("11.sock");
        let _server = LinkServer::start(path.clone()).expect("start");
        let holders: Vec<UnixStream> = (0..MAX_CONNECTIONS)
            .map(|_| UnixStream::connect(&path).expect("connect"))
            .collect();
        match exchange(&path, b"") {
            LinkResponse::Err { code, message } => {
                assert_eq!(code, LinkErrorCode::BadRequest);
                assert!(message.contains("busy"), "{message}");
            }
            other @ LinkResponse::Ok(_) => panic!("expected busy error, got {other:?}"),
        }
        drop(holders);
    }

    #[test]
    fn server_limits_concurrent_connections_but_parked_asks_do_not_count() {
        let (_guard, path) = temp_socket("ask-cap.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        let ask_path = path.clone();
        let ask = thread::spawn(move || {
            exchange(
                &ask_path,
                b"{\"v\":1,\"op\":\"ask\",\"from\":\"1\",\"target\":\"2\",\"text\":\"q\",\"timeout_secs\":5}\n",
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let pending = loop {
            if let Some(pending) = server.try_recv() {
                break pending;
            }
            assert!(Instant::now() < deadline, "ask never arrived");
            thread::sleep(Duration::from_millis(5));
        };
        // The ask is parked: MAX_CONNECTIONS idle holders still all get a slot.
        let holders: Vec<UnixStream> = (0..MAX_CONNECTIONS)
            .map(|_| UnixStream::connect(&path).expect("connect"))
            .collect();
        thread::sleep(Duration::from_millis(200));
        // The last holder was accepted: it got no "busy" line.
        let mut last = holders.last().expect("holder").try_clone().expect("clone");
        last.set_read_timeout(Some(Duration::from_millis(300)))
            .expect("timeout");
        let mut byte = [0_u8; 1];
        let read = io::Read::read(&mut last, &mut byte);
        assert!(
            matches!(&read, Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
            "the holder behind a parked ask was refused: {read:?}"
        );
        let extra = UnixStream::connect(&path).expect("connect");
        extra.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
        let mut reply = String::new();
        std::io::BufRead::read_line(&mut BufReader::new(&extra), &mut reply).expect("busy reply");
        assert!(reply.contains("server busy"), "{reply}");
        drop(holders);
        pending
            .reply
            .send(LinkResponse::Ok(LinkOk::Text("done".to_owned())))
            .expect("answer");
        assert_eq!(
            ask.join().expect("ask client"),
            LinkResponse::Ok(LinkOk::Text("done".to_owned()))
        );
    }

    #[test]
    fn ask_connection_that_gets_no_answer_reports_a_timeout() {
        let (_guard, path) = temp_socket("ask-timeout.sock");
        let server = LinkServer::start(path.clone()).expect("start");
        let line = b"{\"v\":1,\"op\":\"ask\",\"from\":\"1\",\"target\":\"2\",\"text\":\"q\",\"timeout_secs\":5}\n";
        let client_path = path.clone();
        let client = thread::spawn(move || exchange(&client_path, line));
        let started = Instant::now();
        let response = client.join().expect("client");
        assert!(
            matches!(
                response,
                LinkResponse::Err {
                    code: LinkErrorCode::AskTimeout,
                    ..
                }
            ),
            "{response:?}"
        );
        assert!(
            started.elapsed() >= Duration::from_secs(6),
            "waited only {:?}",
            started.elapsed()
        );
        let pending = server.try_recv().expect("the ask reached the UI side");
        assert!(pending.is_abandoned());
    }

    #[test]
    fn lone_newline_gets_a_bad_request_reply() {
        let (_guard, path) = temp_socket("12.sock");
        let _server = LinkServer::start(path.clone()).expect("start");
        assert!(matches!(
            exchange(&path, b"\n"),
            LinkResponse::Err {
                code: LinkErrorCode::BadRequest,
                ..
            }
        ));
    }

    #[test]
    fn symlinked_socket_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        fs::create_dir(&real).expect("mkdir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(LinkServer::start(link.join("13.sock")).is_err());
        assert!(
            !real.join("13.sock").exists(),
            "nothing may be bound through the symlink"
        );
    }
}
