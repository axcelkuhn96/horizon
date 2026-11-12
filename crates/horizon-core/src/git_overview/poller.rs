use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::compute::{CycleCache, compute_cached};
use super::model::{GitOverviewError, RepoOverview};

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15);

type Cwds<K> = Vec<(K, PathBuf)>;
type OverviewResult = Result<Option<RepoOverview>, GitOverviewError>;

enum Command<K> {
    SetCwds(Cwds<K>),
    Refresh,
    SetDirtyWatch(HashSet<PathBuf>),
    Stop,
}

/// What was last sent for a key, for de-duplication. Errors are not
/// comparable, so they are compared by message.
#[derive(PartialEq)]
enum Sent {
    Overview(Option<RepoOverview>),
    Failure(String),
}

/// Background thread that recomputes the git overview for a set of keyed
/// working directories and delivers only changed results.
pub struct OverviewPoller<K> {
    commands: Sender<Command<K>>,
    results: Mutex<Receiver<(K, OverviewResult)>>,
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl<K: Clone + Eq + Hash + Send + 'static> OverviewPoller<K> {
    /// Start polling `cwds` immediately and then every `interval`.
    #[must_use]
    pub fn start(cwds: Cwds<K>, interval: Duration) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("git-overview-poller".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || {
                    poll_loop(cwds, interval, &command_rx, &result_tx, &stop, &AtomicUsize::new(0));
                }
            })
            .map_err(|err| tracing::warn!("failed to spawn git overview poller: {err}"))
            .ok();
        Self {
            commands,
            results: Mutex::new(results),
            thread,
            stop,
        }
    }

    /// Replace the polled set and refresh right away.
    pub fn set_cwds(&self, cwds: Cwds<K>) {
        let _ = self.commands.send(Command::SetCwds(cwds));
    }

    /// Recompute now instead of waiting for the next tick.
    pub fn refresh_now(&self) {
        let _ = self.commands.send(Command::Refresh);
    }

    /// Replace the set of worktree paths (see [`super::WorktreeInfo::path`])
    /// whose dirty count is kept filled in and recompute now. Paths no longer
    /// in the set report `dirty_count: None` from the next overview on, so
    /// callers pass only what is currently visible (e.g. expanded groups).
    pub fn set_dirty_watch(&self, paths: Vec<PathBuf>) {
        let _ = self.commands.send(Command::SetDirtyWatch(paths.into_iter().collect()));
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<(K, OverviewResult)> {
        self.results.lock().ok()?.try_recv().ok()
    }
}

impl<K> Drop for OverviewPoller<K> {
    /// Never blocks the caller (usually the UI thread): the worker notices the
    /// flag between worktrees and exits on its own. It is joined only when it
    /// has already finished.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}

/// Commands that arrived together, folded into one poll.
struct Batch<K> {
    cwds: Option<Cwds<K>>,
    dirty: Option<HashSet<PathBuf>>,
    stop: bool,
}

/// Fold `first` and everything already queued behind it: the last `SetCwds`
/// wins, the last `SetDirtyWatch` wins and `Stop` ends the batch.
fn collect_batch<K>(first: Command<K>, commands: &Receiver<Command<K>>) -> Batch<K> {
    let mut batch = Batch {
        cwds: None,
        dirty: None,
        stop: false,
    };
    let mut next = Some(first);
    while let Some(command) = next.take() {
        match command {
            Command::SetCwds(cwds) => batch.cwds = Some(cwds),
            Command::Refresh => {}
            Command::SetDirtyWatch(paths) => batch.dirty = Some(paths),
            Command::Stop => {
                batch.stop = true;
                break;
            }
        }
        next = commands.try_recv().ok();
    }
    batch
}

fn poll_loop<K: Clone + Eq + Hash>(
    mut cwds: Cwds<K>,
    interval: Duration,
    commands: &Receiver<Command<K>>,
    results: &Sender<(K, OverviewResult)>,
    stop: &AtomicBool,
    cycles: &AtomicUsize,
) {
    let mut last: HashMap<K, Sent> = HashMap::new();
    let mut requested: HashSet<PathBuf> = HashSet::new();
    loop {
        if !poll_once(&cwds, &mut last, &requested, results, stop) {
            return;
        }
        cycles.fetch_add(1, Ordering::Relaxed);
        // A timeout is a tick; a command is folded with whatever else is
        // already queued so a burst costs a single poll.
        match commands.recv_timeout(interval) {
            Ok(first) => {
                let batch = collect_batch(first, commands);
                if batch.stop {
                    return;
                }
                if let Some(next) = batch.cwds {
                    last.retain(|key, _| next.iter().any(|(k, _)| k == key));
                    cwds = next;
                }
                if let Some(paths) = batch.dirty {
                    requested = paths;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Returns `false` when the receiving side is gone or a stop was requested.
fn poll_once<K: Clone + Eq + Hash>(
    cwds: &Cwds<K>,
    last: &mut HashMap<K, Sent>,
    requested: &HashSet<PathBuf>,
    results: &Sender<(K, OverviewResult)>,
    stop: &AtomicBool,
) -> bool {
    let mut cache = CycleCache::default();
    for (key, cwd) in cwds {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let result = compute_cached(cwd, &mut cache, requested, stop);
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let sent = match &result {
            Ok(overview) => Sent::Overview(overview.clone()),
            Err(err) => {
                tracing::debug!("git overview for {} failed: {err}", cwd.display());
                Sent::Failure(err.to_string())
            }
        };
        if last.get(key) == Some(&sent) {
            continue;
        }
        last.insert(key.clone(), sent);
        if results.send((key.clone(), result)).is_err() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use super::super::compute::test_repo::Fixture;
    use super::*;

    type Item = (u32, Result<Option<RepoOverview>, GitOverviewError>);

    fn wait_for(poller: &OverviewPoller<u32>, timeout: Duration) -> Option<Item> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(item) = poller.try_recv() {
                return Some(item);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    fn dirty(item: Item) -> usize {
        item.1.expect("ok").expect("some").dirty_count
    }

    fn many_keys(fx: &Fixture, count: u32) -> Vec<(u32, std::path::PathBuf)> {
        (0..count).map(|key| (key, fx.root())).collect()
    }

    #[test]
    fn poller_delivers_result_and_dedups_identical() {
        let fx = Fixture::with_commit();
        let poller = OverviewPoller::start(vec![(7u32, fx.root())], Duration::from_millis(40));
        let first = wait_for(&poller, Duration::from_secs(5)).expect("first result");
        assert_eq!(first.0, 7);
        assert_eq!(dirty(first), 0);
        std::thread::sleep(Duration::from_millis(300));
        assert!(poller.try_recv().is_none(), "identical results must not be re-sent");
    }

    #[test]
    fn poller_refresh_now_wakes_before_interval() {
        let fx = Fixture::with_commit();
        let poller = OverviewPoller::start(vec![(1u32, fx.root())], Duration::from_secs(3600));
        assert_eq!(dirty(wait_for(&poller, Duration::from_secs(5)).expect("initial")), 0);
        std::fs::write(fx.root().join("new.txt"), "x\n").expect("write");
        poller.refresh_now();
        assert_eq!(dirty(wait_for(&poller, Duration::from_secs(5)).expect("refreshed")), 1);
    }

    #[test]
    fn poller_set_cwds_polls_new_key() {
        let fx = Fixture::with_commit();
        let poller = OverviewPoller::start(Vec::new(), Duration::from_secs(3600));
        poller.set_cwds(vec![(9u32, fx.root())]);
        let item = wait_for(&poller, Duration::from_secs(5)).expect("result for new key");
        assert_eq!(item.0, 9);
    }

    #[test]
    fn poller_drop_stops_thread() {
        let fx = Fixture::with_commit();
        let poller = OverviewPoller::start(vec![(1u32, fx.root())], Duration::from_secs(3600));
        let started = Instant::now();
        drop(poller);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn poller_drop_does_not_block_during_a_long_poll() {
        let fx = Fixture::with_commit();
        let poller = OverviewPoller::start(many_keys(&fx, 3000), Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        drop(poller);
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn stop_flag_aborts_poll_between_keys() {
        let fx = Fixture::with_commit();
        let (command_tx, command_rx) = mpsc::channel::<Command<u32>>();
        let (result_tx, result_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let cycles = Arc::new(AtomicUsize::new(0));
        let handle = {
            let (stop, cycles) = (Arc::clone(&stop), Arc::clone(&cycles));
            let cwds = many_keys(&fx, 3000);
            thread::spawn(move || {
                poll_loop(cwds, Duration::from_secs(3600), &command_rx, &result_tx, &stop, &cycles);
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        stop.store(true, Ordering::Relaxed);
        handle.join().expect("poll thread");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(result_rx.try_iter().count() < 3000, "poll must stop early");
        drop(command_tx);
    }

    #[test]
    fn queued_commands_coalesce_into_one_poll() {
        let fx = Fixture::with_commit();
        let other = Fixture::with_commit();
        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        command_tx
            .send(Command::SetCwds(vec![(1u32, other.root())]))
            .expect("send");
        command_tx.send(Command::Refresh).expect("send");
        command_tx
            .send(Command::SetCwds(vec![(2u32, other.root())]))
            .expect("send");
        command_tx.send(Command::Refresh).expect("send");
        drop(command_tx);
        let cycles = AtomicUsize::new(0);
        poll_loop(
            vec![(0u32, fx.root())],
            Duration::from_secs(3600),
            &command_rx,
            &result_tx,
            &AtomicBool::new(false),
            &cycles,
        );
        // Initial poll + one poll for the whole batch; the last SetCwds wins.
        assert_eq!(cycles.load(Ordering::Relaxed), 2);
        let keys: Vec<u32> = result_rx.try_iter().map(|(key, _)| key).collect();
        assert_eq!(keys, [0, 2]);
    }

    #[test]
    fn stop_command_in_batch_ends_without_polling() {
        let fx = Fixture::with_commit();
        let (command_tx, command_rx) = mpsc::channel();
        let (result_tx, _result_rx) = mpsc::channel();
        command_tx.send(Command::Refresh).expect("send");
        command_tx.send(Command::Stop).expect("send");
        let cycles = AtomicUsize::new(0);
        poll_loop(
            vec![(0u32, fx.root())],
            Duration::from_secs(3600),
            &command_rx,
            &result_tx,
            &AtomicBool::new(false),
            &cycles,
        );
        assert_eq!(cycles.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dirty_watch_fills_the_count_and_replacing_it_clears_it() {
        let fx = Fixture::with_commit();
        let wt = fx.dir.path().join("wts/feature");
        std::fs::create_dir_all(wt.parent().expect("parent")).expect("mkdir");
        fx.repo.worktree("feature", &wt, None).expect("worktree");
        std::fs::write(wt.join("scratch.txt"), "x\n").expect("untracked");

        let poller = OverviewPoller::start(vec![(1u32, fx.root())], Duration::from_secs(3600));
        let first = wait_for(&poller, Duration::from_secs(5)).expect("initial");
        let overview = first.1.expect("ok").expect("some");
        assert_eq!(overview.worktrees[1].dirty_count, None);

        poller.set_dirty_watch(vec![overview.worktrees[1].path.clone()]);
        let second = wait_for(&poller, Duration::from_secs(5)).expect("refreshed");
        let overview = second.1.expect("ok").expect("some");
        assert_eq!(overview.worktrees[1].dirty_count, Some(1));

        poller.set_dirty_watch(Vec::new());
        let third = wait_for(&poller, Duration::from_secs(5)).expect("unwatched");
        let overview = third.1.expect("ok").expect("some");
        assert_eq!(overview.worktrees[1].dirty_count, None);
    }

    #[test]
    fn last_dirty_watch_in_a_batch_wins() {
        let (command_tx, command_rx) = mpsc::channel::<Command<u32>>();
        command_tx
            .send(Command::SetDirtyWatch(HashSet::from([PathBuf::from("/a")])))
            .expect("send");
        command_tx
            .send(Command::SetDirtyWatch(HashSet::from([PathBuf::from("/b")])))
            .expect("send");
        let first = command_rx.try_recv().expect("first");
        let batch = collect_batch(first, &command_rx);
        assert_eq!(batch.dirty, Some(HashSet::from([PathBuf::from("/b")])));
    }
}
