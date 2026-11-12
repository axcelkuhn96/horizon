use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::compute::compute_repo_overview;
use super::model::{GitOverviewError, RepoOverview};

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15);

type Cwds<K> = Vec<(K, PathBuf)>;
type OverviewResult = Result<Option<RepoOverview>, GitOverviewError>;

enum Command<K> {
    SetCwds(Cwds<K>),
    Refresh,
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
}

impl<K: Clone + Eq + Hash + Send + 'static> OverviewPoller<K> {
    /// Start polling `cwds` immediately and then every `interval`.
    #[must_use]
    pub fn start(cwds: Cwds<K>, interval: Duration) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("git-overview-poller".into())
            .spawn(move || poll_loop(cwds, interval, &command_rx, &result_tx))
            .map_err(|err| tracing::warn!("failed to spawn git overview poller: {err}"))
            .ok();
        Self {
            commands,
            results: Mutex::new(results),
            thread,
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

    #[must_use]
    pub fn try_recv(&self) -> Option<(K, OverviewResult)> {
        self.results.lock().ok()?.try_recv().ok()
    }
}

impl<K> Drop for OverviewPoller<K> {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn poll_loop<K: Clone + Eq + Hash>(
    mut cwds: Cwds<K>,
    interval: Duration,
    commands: &Receiver<Command<K>>,
    results: &Sender<(K, OverviewResult)>,
) {
    let mut last: HashMap<K, Sent> = HashMap::new();
    loop {
        if !poll_once(&cwds, &mut last, results) {
            return;
        }
        // Drain until a command asks for a poll; a timeout is a tick.
        match commands.recv_timeout(interval) {
            Ok(Command::SetCwds(next)) => {
                last.retain(|key, _| next.iter().any(|(k, _)| k == key));
                cwds = next;
            }
            Ok(Command::Refresh) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Returns `false` when the receiving side is gone.
fn poll_once<K: Clone + Eq + Hash>(
    cwds: &Cwds<K>,
    last: &mut HashMap<K, Sent>,
    results: &Sender<(K, OverviewResult)>,
) -> bool {
    for (key, cwd) in cwds {
        let result = compute_repo_overview(cwd);
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
}
