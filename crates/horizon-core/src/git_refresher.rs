//! Off-thread, single-in-flight git-status refresher for the File Explorer.
//!
//! The shared [`crate::git_watcher::GitWatcher`] only recomputes status when the
//! `.git/index` mtime changes (i.e. on `git add` / `git commit`); a plain
//! working-tree edit or a new untracked file never touches the index, so the
//! explorer's green/changed decorations would otherwise go stale until a commit.
//! The explorer therefore drives its own throttled refresh (see
//! [`crate::file_tree::GIT_REFRESH_INTERVAL`] and
//! [`crate::file_tree::should_refresh_git`]).
//!
//! Computing the status MUST NOT happen on the egui frame thread:
//! [`crate::git_status::compute_status`] uses `git2` with
//! `recurse_untracked_dirs(true)`, which on a large working tree (e.g. a 2GB
//! Laravel repo with thousands of untracked vendor dirs) takes hundreds of
//! milliseconds per call. Running it inline every ~1.5s froze the window
//! ("not responding") and, worse, could block the close-request handler so the
//! graceful shutdown save never ran and session state was lost.
//!
//! This refresher wraps `compute_status` in a detached worker thread (mirroring
//! [`crate::file_search_runner::SearchRunner`]):
//! - [`GitRefresher::request`] spawns at most ONE compute at a time. A
//!   monotonic generation plus an `in_flight` flag guard against spawning a new
//!   walk while a previous one is still running, so a slow compute can never
//!   pile up unboundedly even if `request` is called every frame.
//! - The worker sends `(generation, GitStatus)` over an mpsc channel.
//! - [`GitRefresher::poll`] drains the channel non-blocking, keeping only the
//!   latest generation's result, and returns it for the UI to apply on the next
//!   frame.
//!
//! Dropping the refresher orphans any in-flight worker; its send then fails
//! harmlessly and the thread exits on its own (it is never joined), so a drop —
//! including during shutdown — never blocks.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::git_status::{GitStatus, compute_status};

/// Owns the off-thread git-status refresh for one File Explorer panel.
///
/// `Send` (it holds only a counter, a bool, and an [`mpsc::Receiver`]), so it
/// can live inside UI state alongside the [`crate::file_search_runner::SearchRunner`].
pub struct GitRefresher {
    /// Monotonic id of the most recent `request`. Used to discard stale results.
    generation: u64,
    /// `true` while a worker spawned by the latest `request` has not yet had its
    /// result drained by `poll`. Guards against spawning a second compute while
    /// one is still running (single-in-flight).
    in_flight: bool,
    /// Receiver for `(generation, status)` messages. `None` until the first
    /// `request`.
    rx: Option<Receiver<(u64, GitStatus)>>,
}

impl Default for GitRefresher {
    fn default() -> Self {
        Self::new()
    }
}

impl GitRefresher {
    /// Create an idle refresher with no compute in flight.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: 0,
            in_flight: false,
            rx: None,
        }
    }

    /// `true` when a background compute is currently running (spawned but not yet
    /// drained by [`poll`]). Used by the throttle to avoid stamping a refresh
    /// time for a request that was suppressed.
    ///
    /// [`poll`]: GitRefresher::poll
    #[must_use]
    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    /// Request a fresh git-status compute for `root` on a background thread.
    ///
    /// Single-in-flight: if a previous compute is still running (`in_flight`),
    /// this is a no-op and returns `false`, so calling it every frame can never
    /// spawn more than one walk at a time. Otherwise it spawns a detached worker,
    /// marks `in_flight`, and returns `true`.
    ///
    /// The worker never blocks the caller; its result is collected later by
    /// [`poll`]. A failed `compute_status` sends nothing (the previous snapshot
    /// is kept by the caller), but still clears `in_flight` via the dropped
    /// sender so the next `request` can proceed.
    ///
    /// [`poll`]: GitRefresher::poll
    pub fn request(&mut self, root: PathBuf) -> bool {
        if self.in_flight {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (tx, rx) = mpsc::channel::<(u64, GitStatus)>();
        self.rx = Some(rx);
        self.in_flight = true;

        // Detached worker: never joined, so a slow compute can't block the UI
        // thread or shutdown. On error we simply send nothing; the dropped
        // sender disconnects the channel and `poll` clears `in_flight`.
        let _handle = thread::spawn(move || {
            if let Ok(status) = compute_status(&root) {
                // Send may fail if the refresher (and receiver) was dropped on
                // shutdown — that's expected, ignore it.
                let _ = tx.send((generation, status));
            }
        });
        true
    }

    /// Non-blocking drain of the result channel.
    ///
    /// Cheap to call every frame. Returns the freshly computed status (wrapped in
    /// an [`Arc`] for cheap sharing into panels) when the latest-generation
    /// worker has finished, else `None`. Clears `in_flight` once the in-flight
    /// worker's channel yields a result or disconnects (so a failed compute
    /// doesn't wedge the single-in-flight guard).
    pub fn poll(&mut self) -> Option<Arc<GitStatus>> {
        let rx = self.rx.take()?;

        let mut latest: Option<GitStatus> = None;
        let mut disconnected = false;

        loop {
            match rx.try_recv() {
                Ok((generation, status)) => {
                    if generation == self.generation {
                        latest = Some(status);
                    }
                    // else: stale generation — drop it.
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        // A delivered result OR a disconnect (worker finished/errored and dropped
        // its sender) both mean the in-flight compute is done.
        if latest.is_some() || disconnected {
            self.in_flight = false;
        }

        // Keep listening unless fully drained AND closed.
        if !disconnected {
            self.rx = Some(rx);
        }

        latest.map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn assert_send<T: Send>() {}

    #[test]
    fn refresher_is_send() {
        assert_send::<GitRefresher>();
    }

    #[test]
    fn new_is_idle_not_in_flight() {
        let r = GitRefresher::new();
        assert!(!r.is_in_flight());
    }

    #[test]
    fn request_marks_in_flight_and_second_request_is_suppressed() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("new.txt"), b"hi").expect("write");

        let mut r = GitRefresher::new();
        assert!(r.request(dir.path().to_path_buf()), "first request spawns");
        assert!(r.is_in_flight());
        // A second request while the first is in flight must NOT spawn another
        // walk (single-in-flight guard).
        assert!(!r.request(dir.path().to_path_buf()), "second request suppressed");
    }

    /// Poll until a result arrives or the in-flight flag clears, bounded so a
    /// wedged refresher fails the test instead of hanging.
    fn poll_until_settled(r: &mut GitRefresher) -> Option<Arc<GitStatus>> {
        for _ in 0..500 {
            if let Some(status) = r.poll() {
                return Some(status);
            }
            if !r.is_in_flight() {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
        r.poll()
    }

    #[test]
    fn request_then_poll_yields_status_for_untracked_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("new.txt"), b"hello").expect("write");

        let mut r = GitRefresher::new();
        r.request(dir.path().to_path_buf());
        let status = poll_until_settled(&mut r).expect("status delivered");
        assert!(
            status.changes.iter().any(|c| c.path == "new.txt"),
            "untracked file must appear in the off-thread snapshot"
        );
        // Once drained, the guard is cleared so a new request can proceed.
        assert!(!r.is_in_flight());
        assert!(r.request(dir.path().to_path_buf()), "request allowed after drain");
    }

    #[test]
    fn poll_returns_none_when_no_request_made() {
        let mut r = GitRefresher::new();
        assert!(r.poll().is_none());
    }

    #[test]
    fn request_does_not_block_caller() {
        // The whole point: request returns promptly even on a real repo, because
        // the compute runs on the worker thread.
        let dir = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");
        std::fs::write(dir.path().join("a.txt"), b"x").expect("write");

        let mut r = GitRefresher::new();
        let t = Instant::now();
        r.request(dir.path().to_path_buf());
        assert!(t.elapsed() < Duration::from_secs(1), "request must not block");
        let _ = poll_until_settled(&mut r);
    }
}
