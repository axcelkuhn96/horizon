use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::git_status::{GitStatus, compute_status};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Shared shutdown signal for a watcher thread.
///
/// The flag plus condvar let `watcher_loop` sleep between polls *and* wake
/// instantly when shutdown is requested, instead of sleeping the full
/// [`POLL_INTERVAL`]. This keeps teardown (and any join) near-instant rather
/// than blocking up to the poll interval.
struct ShutdownSignal {
    flag: AtomicBool,
    waker: Mutex<()>,
    condvar: Condvar,
}

impl ShutdownSignal {
    fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
            waker: Mutex::new(()),
            condvar: Condvar::new(),
        }
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Request shutdown and wake any thread sleeping in [`Self::wait`].
    fn signal(&self) {
        self.flag.store(true, Ordering::Relaxed);
        // Hold the mutex while notifying so a thread that is about to wait
        // cannot miss the notification (lost-wakeup guard).
        let _guard = self.waker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.condvar.notify_all();
    }

    /// Sleep up to `timeout`, returning early if shutdown was requested.
    /// Returns `true` if shutdown is set (caller should stop).
    fn wait(&self, timeout: Duration) -> bool {
        if self.is_set() {
            return true;
        }
        let guard = self.waker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // `wait_timeout_while` re-checks the predicate, so spurious wakeups and
        // the lost-wakeup race are both handled.
        let _ = self
            .condvar
            .wait_timeout_while(guard, timeout, |()| !self.flag.load(Ordering::Relaxed));
        self.is_set()
    }
}

pub struct GitWatcher {
    receiver: mpsc::Receiver<Arc<GitStatus>>,
    shutdown: Arc<ShutdownSignal>,
    thread: Option<JoinHandle<()>>,
}

impl GitWatcher {
    /// Start a background git watcher for the given repo path.
    /// Polls `.git/index` mtime every ~2 seconds.
    #[must_use]
    pub fn start(repo_path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let shutdown = Arc::new(ShutdownSignal::new());
        let shutdown_flag = Arc::clone(&shutdown);

        let thread = thread::Builder::new()
            .name(format!("git-watcher-{}", short_path(&repo_path)))
            .spawn(move || watcher_loop(&repo_path, &sender, &shutdown_flag))
            .ok();

        Self {
            receiver,
            shutdown,
            thread,
        }
    }

    /// Non-blocking receive. Returns the latest status if available.
    #[must_use]
    pub fn try_recv(&self) -> Option<Arc<GitStatus>> {
        let mut latest = None;
        // Drain to get the most recent status.
        while let Ok(status) = self.receiver.try_recv() {
            latest = Some(status);
        }
        latest
    }

    /// Signal the watcher thread to stop **without** waiting for it to finish.
    ///
    /// This is the close-path entry point: the UI thread must never block
    /// joining a watcher that is mid-`git status`. The thread is detached and
    /// observes the shutdown flag promptly (it sleeps on a condvar that this
    /// call wakes), then exits on its own. Any in-flight `compute_status`
    /// finishes in the background and the process exit reaps the thread.
    pub fn begin_stop(&mut self) {
        self.shutdown.signal();
        // Drop the join handle without joining: detach the thread.
        self.thread.take();
    }

    /// Signal the watcher thread to stop and wait for it to finish.
    ///
    /// Blocking. Used in non-UI contexts (e.g. tests). The condvar wake means
    /// this returns as soon as the thread finishes its current iteration rather
    /// than after the full poll interval.
    pub fn stop(&mut self) {
        self.shutdown.signal();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for GitWatcher {
    fn drop(&mut self) {
        // Detach rather than join: dropping a watcher (including via
        // `HashMap::clear` on the close path) must not block the caller while a
        // `git status` finishes. The thread sees the shutdown signal and exits.
        self.begin_stop();
    }
}

fn watcher_loop(repo_path: &Path, sender: &mpsc::Sender<Arc<GitStatus>>, shutdown: &ShutdownSignal) {
    let index_path = resolve_git_index_path(repo_path);
    let mut last_mtime: Option<SystemTime> = None;

    // Bail before the initial (potentially expensive) scan if we are already
    // shutting down — avoids a wasted `git status` when the watcher is created
    // and torn down in quick succession.
    if shutdown.is_set() {
        return;
    }

    // Always do an initial scan.
    if let Some(status) = try_compute_status(repo_path) {
        last_mtime = index_path.as_deref().and_then(file_mtime);
        let _ = sender.send(Arc::new(status));
    }

    loop {
        // Sleep up to POLL_INTERVAL, waking immediately on shutdown.
        if shutdown.wait(POLL_INTERVAL) {
            break;
        }

        let current_mtime = index_path.as_deref().and_then(file_mtime);

        // The index mtime changes on every `git add` or `git commit`.
        let changed = match (last_mtime, current_mtime) {
            (Some(prev), Some(curr)) => prev != curr,
            (None, Some(_)) => true,
            _ => false,
        };

        if !changed {
            continue;
        }

        if let Some(status) = try_compute_status(repo_path) {
            last_mtime = current_mtime;
            if sender.send(Arc::new(status)).is_err() {
                break;
            }
        }
    }
}

fn try_compute_status(repo_path: &Path) -> Option<GitStatus> {
    match compute_status(repo_path) {
        Ok(status) => Some(status),
        Err(error) => {
            tracing::warn!(path = %repo_path.display(), %error, "git status failed");
            None
        }
    }
}

fn resolve_git_index_path(repo_path: &Path) -> Option<PathBuf> {
    if let Ok(repo) = git2::Repository::discover(repo_path) {
        Some(repo.path().join("index"))
    } else {
        let candidate = repo_path.join(".git/index");
        candidate.exists().then_some(candidate)
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn short_path(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "unknown".to_string(), |n| n.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn shutdown_signal_wait_returns_early_when_signalled() {
        let signal = Arc::new(ShutdownSignal::new());
        let waker = Arc::clone(&signal);
        let started = Instant::now();
        let handle = thread::spawn(move || {
            // Sleep up to 10s, but should be woken almost immediately.
            signal.wait(Duration::from_secs(10))
        });
        // Give the thread a moment to enter wait, then signal.
        thread::sleep(Duration::from_millis(20));
        waker.signal();
        let stopped = handle.join().expect("thread joins");
        assert!(stopped, "wait should report shutdown set");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "wait should return promptly after signal, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn shutdown_signal_wait_returns_immediately_when_already_set() {
        let signal = ShutdownSignal::new();
        signal.signal();
        let started = Instant::now();
        assert!(signal.wait(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn begin_stop_does_not_block_and_signals_thread() {
        // A watcher over a temp non-repo dir: watcher_loop's initial scan fails
        // fast (no repo), then it waits on the condvar. begin_stop must return
        // promptly and leave no join handle.
        let dir = tempfile::tempdir().expect("temp dir");
        let mut watcher = GitWatcher::start(dir.path().to_path_buf());
        let started = Instant::now();
        watcher.begin_stop();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "begin_stop must not block, took {:?}",
            started.elapsed()
        );
        assert!(watcher.thread.is_none(), "begin_stop detaches the thread");
    }
}
