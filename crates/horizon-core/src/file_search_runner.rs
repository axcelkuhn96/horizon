//! Background runner for the file-content search engine.
//!
//! [`crate::file_search::search_files`] is a synchronous, potentially slow walk
//! of a directory tree. Calling it directly on the egui frame thread would
//! stall the UI. This module wraps that engine in a [`SearchRunner`] that the UI
//! owns in its state: it calls [`SearchRunner::start`] when the query changes
//! and [`SearchRunner::poll`] once per frame to pick up results without ever
//! blocking.
//!
//! ## Concurrency model
//! Each [`SearchRunner::start`] spawns a detached worker thread and bumps a
//! monotonic generation counter. The worker runs the engine, then sends back
//! `(generation, outcome)` over an mpsc channel. [`SearchRunner::poll`] drains
//! the channel non-blocking and keeps ONLY the message whose generation matches
//! the latest `start`; messages from superseded (slower, older) searches are
//! discarded. This guarantees a slow stale search can never clobber a newer
//! result. Worker threads are detached (the [`std::thread::JoinHandle`] is
//! dropped), so a slow search never blocks the runner or the UI; its result is
//! simply ignored once superseded.
//!
//! ## Debounce contract (caller responsibility)
//! Supersession only discards stale *results* — the superseded worker thread
//! still runs its full directory walk to completion. The runner does NOT
//! debounce. Therefore the CALLER (the UI) MUST debounce its calls to
//! [`SearchRunner::start`] (e.g. only fire after ~150ms of input quiescence),
//! otherwise rapid typing spawns one full directory walk per keystroke, which
//! is wasteful on large trees.
//!
//! ## Empty-query behaviour
//! A query that is empty or whitespace-only does no useful work, so
//! [`SearchRunner::start`] does NOT spawn a thread for it. Instead it resets the
//! runner to [`SearchState::Idle`]. The UI treats "Idle" as "nothing to show".

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::file_search::{FileSearchOptions, SearchError, SearchOutcome, search_files};

/// Lifecycle of the runner as observed by the UI.
#[derive(Debug)]
pub enum SearchState {
    /// No search is running and there is no result to show (initial state, or
    /// after [`SearchRunner::clear`] / an empty query).
    Idle,
    /// A query is in flight on a background thread.
    Searching,
    /// The latest search finished. Carries its query and engine result.
    Done(SearchRunOutcome),
}

/// The finished outcome of one background search.
#[derive(Debug)]
pub struct SearchRunOutcome {
    /// The query string that produced this outcome (lets the UI confirm which
    /// search the result belongs to, e.g. after rapid retyping).
    pub query: String,
    /// The engine result: matches on success, or [`SearchError`] (e.g. an
    /// invalid regex) on failure. The UI renders the error inline.
    pub result: Result<SearchOutcome, SearchError>,
}

/// Owns a background file-content search and exposes a non-blocking poll API.
///
/// `SearchRunner` is [`Send`] (it holds only a generation counter, a state
/// enum, and an [`mpsc::Receiver`]), so it can live inside UI state. Worker
/// threads are detached; dropping the runner simply orphans any in-flight
/// search, whose send will fail harmlessly.
pub struct SearchRunner {
    /// Monotonic id of the most recent `start`. Used to discard stale results.
    generation: u64,
    /// Receiver for `(generation, outcome)` messages from worker threads. `None`
    /// until the first `start`.
    rx: Option<Receiver<(u64, SearchRunOutcome)>>,
    /// Latest observed lifecycle state.
    state: SearchState,
}

impl Default for SearchRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchRunner {
    /// Create an idle runner with no search in flight.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: 0,
            rx: None,
            state: SearchState::Idle,
        }
    }

    /// Spawn a background search for `query` under `root`.
    ///
    /// Supersedes any in-flight search: its generation is bumped, so when the
    /// older search eventually finishes its result is dropped by [`poll`].
    ///
    /// Note that supersession only discards the stale *result*: the superseded
    /// worker thread still runs its full directory walk to completion. This
    /// method does NOT debounce, so the CALLER (the UI) MUST debounce its calls
    /// (e.g. only fire after ~150ms of input quiescence) to avoid spawning one
    /// full directory walk per keystroke on large trees.
    ///
    /// An empty or whitespace-only `query` does no work — the runner resets to
    /// [`SearchState::Idle`] and no thread is spawned.
    ///
    /// [`poll`]: SearchRunner::poll
    pub fn start(&mut self, root: PathBuf, query: String, opts: FileSearchOptions) {
        // Bump the generation first so any thread spawned by a *previous* start
        // is now stale even before this one (if any) begins.
        self.generation = self.generation.wrapping_add(1);

        if query.trim().is_empty() {
            // Nothing useful to search. Drop any pending receiver so stale
            // results can't surface later, and go Idle.
            self.rx = None;
            self.state = SearchState::Idle;
            return;
        }

        let generation = self.generation;
        let (tx, rx) = mpsc::channel::<(u64, SearchRunOutcome)>();
        self.rx = Some(rx);
        self.state = SearchState::Searching;

        // Detached worker: we never join, so a slow search can't block the UI.
        // The JoinHandle is dropped immediately.
        let _handle = thread::spawn(move || {
            let result = search_files(&root, &query, &opts);
            let outcome = SearchRunOutcome { query, result };
            // Send may fail if the runner (and thus the receiver) was dropped.
            // That's expected on shutdown — ignore it, never unwrap.
            let _ = tx.send((generation, outcome));
        });
    }

    /// Non-blocking drain of the result channel.
    ///
    /// Cheap to call every frame. Reads every pending message, keeping only the
    /// one whose generation equals the current generation; messages from
    /// superseded searches are discarded. Returns the (possibly updated) state.
    pub fn poll(&mut self) -> &SearchState {
        // Take the receiver out so we can borrow `self` mutably for state
        // updates while iterating, then put it back unless it disconnected.
        let Some(rx) = self.rx.take() else {
            return &self.state;
        };

        let mut latest: Option<SearchRunOutcome> = None;
        let mut disconnected = false;

        loop {
            match rx.try_recv() {
                Ok((generation, outcome)) => {
                    if generation == self.generation {
                        // Newer message for the current generation wins; older
                        // ones in the same drain are overwritten.
                        latest = Some(outcome);
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

        if let Some(outcome) = latest {
            self.state = SearchState::Done(outcome);
        }

        // Keep listening unless the channel is fully drained AND closed. Once
        // disconnected with nothing left, drop the receiver so future polls
        // short-circuit.
        if !disconnected {
            self.rx = Some(rx);
        }

        &self.state
    }

    /// The last observed state without draining the channel.
    #[must_use]
    pub fn state(&self) -> &SearchState {
        &self.state
    }

    /// Cancel any in-flight search and reset to [`SearchState::Idle`].
    ///
    /// Bumps the generation (so a running worker's result is discarded) and
    /// drops the receiver. The detached worker keeps running to completion but
    /// its send is ignored.
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.rx = None;
        self.state = SearchState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Write `contents` to `dir/rel`, creating parents as needed.
    fn write(dir: &Path, rel: &str, contents: &[u8]) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write file");
    }

    /// Poll the runner until it leaves `Searching`, bounded so a wedged runner
    /// fails the test instead of hanging forever. Returns the final state ref's
    /// cloned discriminant info via a closure on `&SearchState`.
    fn poll_until_done(runner: &mut SearchRunner) -> &SearchState {
        const MAX_ITERS: u32 = 500; // 500 * 10ms = up to 5s budget
        for _ in 0..MAX_ITERS {
            if !matches!(runner.poll(), SearchState::Searching) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Final poll to return a stable borrow.
        runner.poll()
    }

    #[test]
    fn start_then_poll_reaches_done_with_results() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"hello needle here\n");

        let mut runner = SearchRunner::new();
        runner.start(
            dir.path().to_path_buf(),
            "needle".to_string(),
            FileSearchOptions::default(),
        );

        match poll_until_done(&mut runner) {
            SearchState::Done(outcome) => {
                assert_eq!(outcome.query, "needle");
                let res = outcome.result.as_ref().expect("engine ok");
                assert_eq!(res.results.len(), 1);
                assert_eq!(res.results[0].matches.len(), 1);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn supersede_keeps_only_latest_query() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"alpha\nbravo\n");

        let mut runner = SearchRunner::new();
        // Start A, then immediately start B before polling. B's generation wins.
        runner.start(
            dir.path().to_path_buf(),
            "alpha".to_string(),
            FileSearchOptions::default(),
        );
        runner.start(
            dir.path().to_path_buf(),
            "bravo".to_string(),
            FileSearchOptions::default(),
        );

        match poll_until_done(&mut runner) {
            SearchState::Done(outcome) => {
                assert_eq!(outcome.query, "bravo", "latest start must win");
                let res = outcome.result.as_ref().expect("engine ok");
                assert_eq!(res.results.len(), 1);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn stale_generation_message_is_discarded_by_poll() {
        // Unit-test the discard logic deterministically with a hand-fed channel,
        // independent of thread timing. We construct a runner, feed it both a
        // stale and a current message, and confirm only the current survives.
        let (tx, rx) = mpsc::channel::<(u64, SearchRunOutcome)>();
        let mut runner = SearchRunner {
            generation: 7,
            rx: Some(rx),
            state: SearchState::Searching,
        };

        // Stale (old generation) message — must be dropped.
        tx.send((
            6,
            SearchRunOutcome {
                query: "old".to_string(),
                result: Ok(SearchOutcome {
                    results: Vec::new(),
                    truncated: false,
                }),
            },
        ))
        .expect("send stale");
        // Current generation message — must win.
        tx.send((
            7,
            SearchRunOutcome {
                query: "new".to_string(),
                result: Ok(SearchOutcome {
                    results: Vec::new(),
                    truncated: false,
                }),
            },
        ))
        .expect("send current");

        match runner.poll() {
            SearchState::Done(outcome) => assert_eq!(outcome.query, "new"),
            other => panic!("expected Done(new), got {other:?}"),
        }
    }

    #[test]
    fn invalid_regex_yields_done_with_error_not_panic() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"anything\n");

        let opts = FileSearchOptions {
            regex: true,
            ..FileSearchOptions::default()
        };
        let mut runner = SearchRunner::new();
        runner.start(dir.path().to_path_buf(), "(unclosed".to_string(), opts);

        match poll_until_done(&mut runner) {
            SearchState::Done(outcome) => {
                assert!(outcome.result.is_err(), "invalid regex must be an Err");
                match outcome.result.as_ref().expect_err("err") {
                    SearchError::InvalidRegex(msg) => assert!(!msg.is_empty()),
                }
            }
            other => panic!("expected Done(Err), got {other:?}"),
        }
    }

    #[test]
    fn empty_query_stays_idle_without_searching() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"needle\n");

        let mut runner = SearchRunner::new();
        runner.start(
            dir.path().to_path_buf(),
            "   ".to_string(),
            FileSearchOptions::default(),
        );
        assert!(matches!(runner.state(), SearchState::Idle));
        assert!(matches!(runner.poll(), SearchState::Idle));
    }

    #[test]
    fn clear_returns_to_idle() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"needle\n");

        let mut runner = SearchRunner::new();
        runner.start(
            dir.path().to_path_buf(),
            "needle".to_string(),
            FileSearchOptions::default(),
        );
        // Let it finish (or not) — either way clear must reset to Idle.
        let _ = poll_until_done(&mut runner);
        runner.clear();
        assert!(matches!(runner.state(), SearchState::Idle));
        assert!(matches!(runner.poll(), SearchState::Idle));
    }

    #[test]
    fn start_sets_searching_immediately_for_nonempty_query() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "a.txt", b"needle\n");

        let mut runner = SearchRunner::new();
        let before = Instant::now();
        runner.start(
            dir.path().to_path_buf(),
            "needle".to_string(),
            FileSearchOptions::default(),
        );
        // Right after start (before any poll) the state must be Searching.
        assert!(matches!(runner.state(), SearchState::Searching));
        assert!(before.elapsed() < Duration::from_secs(1));
        let _ = poll_until_done(&mut runner);
    }

    #[test]
    fn runner_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SearchRunner>();
    }
}
