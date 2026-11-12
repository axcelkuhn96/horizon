//! Background poller: refreshes every profile's usage on an interval or on demand.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
#[cfg(test)]
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::api::refresh_profile_detailed;
use super::{ProfileUsage, USAGE_URL, load_cache, save_cache};
use crate::Waker;
use crate::config::ClaudeProfile;

/// Floor for the public poll interval: the endpoint is rate limited.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Ceiling for the wait after repeated HTTP 429 answers.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);

enum Command {
    RefreshNow,
    SetProfiles(Vec<ClaudeProfile>),
    Stop,
}

/// Owns a worker thread that polls usage; dropping it stops the thread.
///
/// The thread exits after any refresh already in flight (at most the 10 s HTTP
/// timeout per profile); `Drop` does not wait for it.
pub struct UsagePoller {
    commands: Sender<Command>,
    results: Receiver<Vec<ProfileUsage>>,
    /// Test-only liveness probe: upgrades while the worker thread still runs.
    #[cfg(test)]
    alive: Weak<()>,
}

struct Worker {
    profiles: Vec<ClaudeProfile>,
    home: PathBuf,
    cache_path: PathBuf,
    base_url: String,
    previous: Vec<ProfileUsage>,
    waker: Option<Waker>,
}

impl Worker {
    /// Refreshes every profile; `None` once the receiving side is gone, else
    /// whether any profile was rate limited (HTTP 429).
    fn refresh(&mut self, results: &Sender<Vec<ProfileUsage>>) -> Option<bool> {
        let now_ms = now_ms();
        let mut rate_limited = false;
        let snapshot: Vec<ProfileUsage> = self
            .profiles
            .iter()
            .map(|profile| {
                let previous = self.previous.iter().find(|usage| usage.name == profile.name);
                let (usage, limited) = refresh_profile_detailed(profile, &self.home, &self.base_url, now_ms, previous);
                rate_limited |= limited;
                usage
            })
            .collect();
        if let Err(error) = save_cache(&self.cache_path, &snapshot) {
            tracing::debug!(%error, "failed to write claude usage cache");
        }
        self.previous.clone_from(&snapshot);
        results.send(snapshot).ok()?;
        wake(self.waker.as_ref());
        Some(rate_limited)
    }
}

fn wake(waker: Option<&Waker>) {
    if let Some(waker) = waker {
        waker();
    }
}

/// How long to wait between refreshes: the interval, doubled (up to 30 min) after each 429.
struct Schedule {
    interval: Duration,
    wait: Duration,
    backoff_until: Option<Instant>,
}

impl Schedule {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            wait: interval,
            backoff_until: None,
        }
    }

    fn record(&mut self, rate_limited: bool, now: Instant) {
        if rate_limited {
            self.wait = self.wait.saturating_mul(2).min(MAX_BACKOFF.max(self.interval));
            self.backoff_until = Some(now + self.wait);
        } else {
            self.wait = self.interval;
            self.backoff_until = None;
        }
    }

    fn backing_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| now < until)
    }
}

/// What the queued commands ask for, with duplicates coalesced.
#[derive(Default, PartialEq, Eq, Debug)]
struct Drained {
    refresh: bool,
    profiles_changed: bool,
    stop: bool,
}

fn drain_commands(first: Command, commands: &Receiver<Command>, profiles: &mut Vec<ClaudeProfile>) -> Drained {
    let mut drained = Drained::default();
    for command in std::iter::once(first).chain(commands.try_iter()) {
        match command {
            Command::RefreshNow => drained.refresh = true,
            Command::SetProfiles(new) => {
                if *profiles != new {
                    *profiles = new;
                    drained.profiles_changed = true;
                }
            }
            Command::Stop => drained.stop = true,
        }
    }
    drained
}

/// Blocks until the next refresh is due; `false` means stop the worker.
///
/// Any number of queued `RefreshNow` coalesce into one refresh, and are ignored
/// while backing off from a 429. A profile-set change always refreshes.
fn wait_for_refresh(commands: &Receiver<Command>, worker: &mut Worker, schedule: &Schedule) -> bool {
    let deadline = Instant::now() + schedule.wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match commands.recv_timeout(remaining) {
            Err(RecvTimeoutError::Timeout) => return true,
            Err(RecvTimeoutError::Disconnected) | Ok(Command::Stop) => return false,
            Ok(first) => {
                let drained = drain_commands(first, commands, &mut worker.profiles);
                if drained.stop {
                    return false;
                }
                if drained.profiles_changed || (drained.refresh && !schedule.backing_off(Instant::now())) {
                    return true;
                }
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation)] // milliseconds since the epoch fit in i64
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl UsagePoller {
    /// Starts polling `profiles` against the production endpoint. `home` is the
    /// user's home directory (for `~` expansion); the first refresh runs immediately.
    ///
    /// `interval` is clamped to at least [`MIN_POLL_INTERVAL`]. The cached snapshot,
    /// if any, is delivered right away (before the first network refresh finishes),
    /// carrying the state and `fetched_at_ms` it was stored with.
    #[must_use]
    pub fn spawn(profiles: Vec<ClaudeProfile>, home: PathBuf, cache_path: PathBuf, interval: Duration) -> Self {
        Self::spawn_with_waker(profiles, home, cache_path, interval, None)
    }

    /// Like [`Self::spawn`], calling `waker` after each snapshot is delivered
    /// (the cached one included) so an idle UI repaints without polling.
    #[must_use]
    pub fn spawn_with_waker(
        profiles: Vec<ClaudeProfile>,
        home: PathBuf,
        cache_path: PathBuf,
        interval: Duration,
        waker: Option<Waker>,
    ) -> Self {
        Self::start(
            profiles,
            home,
            cache_path,
            interval.max(MIN_POLL_INTERVAL),
            USAGE_URL.to_owned(),
            waker,
        )
    }

    /// Test constructor: any interval, any (loopback stub) endpoint.
    #[cfg(test)]
    pub(crate) fn spawn_with_base(
        profiles: Vec<ClaudeProfile>,
        home: PathBuf,
        cache_path: PathBuf,
        interval: Duration,
        base_url: String,
    ) -> Self {
        Self::start(profiles, home, cache_path, interval, base_url, None)
    }

    /// Test constructor with a waker.
    #[cfg(test)]
    pub(crate) fn spawn_with_base_and_waker(
        profiles: Vec<ClaudeProfile>,
        home: PathBuf,
        cache_path: PathBuf,
        interval: Duration,
        base_url: String,
        waker: Waker,
    ) -> Self {
        Self::start(profiles, home, cache_path, interval, base_url, Some(waker))
    }

    fn start(
        profiles: Vec<ClaudeProfile>,
        home: PathBuf,
        cache_path: PathBuf,
        interval: Duration,
        base_url: String,
        waker: Option<Waker>,
    ) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        #[cfg(test)]
        let alive = Arc::new(());
        #[cfg(test)]
        let worker_alive = Arc::clone(&alive);
        let previous = load_cache(&cache_path);
        if !previous.is_empty() && result_tx.send(previous.clone()).is_ok() {
            wake(waker.as_ref());
        }
        let mut worker = Worker {
            profiles,
            home,
            cache_path,
            base_url,
            previous,
            waker,
        };
        let spawned = thread::Builder::new()
            .name("claude-usage-poller".to_owned())
            .spawn(move || {
                let mut schedule = Schedule::new(interval);
                while let Some(rate_limited) = worker.refresh(&result_tx) {
                    schedule.record(rate_limited, Instant::now());
                    if !wait_for_refresh(&command_rx, &mut worker, &schedule) {
                        break;
                    }
                }
                #[cfg(test)]
                drop(worker_alive);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "failed to start claude usage poller thread");
        }
        Self {
            commands,
            results,
            #[cfg(test)]
            alive: Arc::downgrade(&alive),
        }
    }

    #[cfg(test)]
    pub(crate) fn alive_probe(&self) -> Weak<()> {
        self.alive.clone()
    }

    /// Requests an immediate refresh.
    pub fn refresh_now(&self) {
        let _ = self.commands.send(Command::RefreshNow);
    }

    /// Replaces the polled profiles and refreshes immediately.
    pub fn set_profiles(&self, profiles: Vec<ClaudeProfile>) {
        let _ = self.commands.send(Command::SetProfiles(profiles));
    }

    /// Latest finished snapshot, if a new one arrived since the last call.
    #[must_use]
    pub fn try_recv(&self) -> Option<Vec<ProfileUsage>> {
        self.results.try_iter().last()
    }
}

impl Drop for UsagePoller {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::claude_usage::UsageState;
    use crate::claude_usage::test_support::{FAR_FUTURE_MS, Stub, USAGE_BODY, write_credentials};

    fn wait_for(poller: &UsagePoller, mut accept: impl FnMut(&[ProfileUsage]) -> bool) -> Vec<ProfileUsage> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(snapshot) = poller.try_recv()
                && accept(&snapshot)
            {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("poller delivered no matching snapshot within 5 s");
    }

    fn profile(home: &std::path::Path, name: &str) -> ClaudeProfile {
        write_credentials(
            &home.join(format!(".claude-{name}")),
            "sk-test-SECRET-token-123",
            FAR_FUTURE_MS,
        );
        ClaudeProfile {
            name: name.to_owned(),
            config_dir: format!("~/.claude-{name}"),
        }
    }

    fn sample_cached(name: &str) -> ProfileUsage {
        ProfileUsage {
            name: name.to_owned(),
            display_name: Some("Ada".to_owned()),
            email: None,
            five_hour: crate::claude_usage::UsageWindow {
                percent: Some(42.0),
                resets_at: None,
            },
            seven_day: crate::claude_usage::UsageWindow::default(),
            severity: None,
            state: UsageState::Stale,
            fetched_at_ms: Some(1_000),
        }
    }

    #[test]
    fn spawn_with_tiny_interval_does_not_poll_repeatedly() {
        let home = tempfile::tempdir().unwrap();
        // No credentials: the public constructor never reaches the network here.
        let ghost = ClaudeProfile {
            name: "ghost".to_owned(),
            config_dir: "~/.claude-ghost".to_owned(),
        };
        let poller = UsagePoller::spawn(
            vec![ghost],
            home.path().to_path_buf(),
            home.path().join("cache.json"),
            Duration::from_millis(5),
        );
        wait_for(&poller, |s| s.len() == 1);
        std::thread::sleep(Duration::from_millis(300));
        assert!(poller.try_recv().is_none(), "interval must be clamped to >= 60 s");
    }

    #[test]
    fn queued_refresh_now_commands_coalesce() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..5 {
            tx.send(Command::RefreshNow).unwrap();
        }
        let mut profiles = Vec::new();
        let first = rx.recv().unwrap();
        let drained = drain_commands(first, &rx, &mut profiles);
        assert_eq!(
            drained,
            Drained {
                refresh: true,
                profiles_changed: false,
                stop: false
            }
        );
        assert!(rx.try_recv().is_err(), "all queued commands drained");
    }

    #[test]
    fn set_profiles_marks_changed_only_when_the_list_differs() {
        let (tx, rx) = mpsc::channel();
        let named = |name: &str| ClaudeProfile {
            name: name.to_owned(),
            config_dir: format!("~/.claude-{name}"),
        };
        let mut profiles = vec![named("a")];
        tx.send(Command::SetProfiles(vec![named("a")])).unwrap();
        let first = rx.recv().unwrap();
        assert!(!drain_commands(first, &rx, &mut profiles).profiles_changed);
        assert_eq!(profiles, vec![named("a")]);
        tx.send(Command::SetProfiles(vec![named("a"), named("b")])).unwrap();
        let first = rx.recv().unwrap();
        assert!(drain_commands(first, &rx, &mut profiles).profiles_changed);
        assert_eq!(profiles, vec![named("a"), named("b")]);
    }

    #[test]
    fn waker_is_called_once_per_delivered_snapshot() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let home = tempfile::tempdir().unwrap();
        let stub = Stub::new(200, "", USAGE_BODY);
        let calls = Arc::new(AtomicUsize::new(0));
        let waker: Waker = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let poller = UsagePoller::spawn_with_base_and_waker(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            home.path().join("cache.json"),
            Duration::from_secs(3600),
            stub.url.clone(),
            waker,
        );
        // The wake follows the send, so wait for it instead of racing it.
        let wait_calls = |expected: usize| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while calls.load(Ordering::SeqCst) < expected && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(100));
            calls.load(Ordering::SeqCst)
        };
        wait_for(&poller, |_| true);
        assert_eq!(wait_calls(1), 1, "no cache: one snapshot, one wake");
        poller.refresh_now();
        wait_for(&poller, |_| true);
        assert_eq!(wait_calls(2), 2);
    }

    #[test]
    fn schedule_doubles_on_429_caps_at_30_min_and_resets_on_success() {
        let now = Instant::now();
        let mut schedule = Schedule::new(Duration::from_secs(300));
        schedule.record(true, now);
        assert_eq!(schedule.wait, Duration::from_secs(600));
        assert!(schedule.backing_off(now));
        schedule.record(true, now);
        assert_eq!(schedule.wait, Duration::from_secs(1200));
        schedule.record(true, now);
        assert_eq!(schedule.wait, MAX_BACKOFF);
        schedule.record(true, now);
        assert_eq!(schedule.wait, MAX_BACKOFF);
        schedule.record(false, now);
        assert_eq!(schedule.wait, Duration::from_secs(300));
        assert!(!schedule.backing_off(now));
    }

    #[test]
    fn poller_on_429_keeps_previous_values_as_stale_and_skips_refresh_now() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("cache.json");
        let mut cached = sample_cached("a");
        cached.state = UsageState::Ok;
        save_cache(&cache, &[cached]).unwrap();
        let stub = Stub::new(429, "", "{}");
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            cache,
            Duration::from_secs(3600),
            stub.url.clone(),
        );
        let snapshot = wait_for(&poller, |s| s[0].state == UsageState::Stale);
        assert_eq!(snapshot[0].five_hour.percent, Some(42.0));
        poller.refresh_now();
        poller.refresh_now();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            stub.requests().len(),
            1,
            "RefreshNow must not hammer a rate-limited endpoint"
        );
    }

    #[test]
    fn spawn_delivers_cached_snapshot_before_first_refresh() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("cache.json");
        save_cache(&cache, &[sample_cached("a")]).unwrap();
        // Accepts connections (kernel backlog) but never answers: the first network
        // refresh cannot finish, so the only snapshot available is the cached one.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/oauth/usage", silent.local_addr().unwrap());
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            cache,
            Duration::from_secs(3600),
            url,
        );
        let first = poller.try_recv().expect("cached snapshot delivered at spawn");
        assert_eq!(first, vec![sample_cached("a")]);
        assert_eq!(first[0].fetched_at_ms, Some(1_000));
    }

    #[test]
    fn poller_refresh_now_delivers_result() {
        let home = tempfile::tempdir().unwrap();
        let stub = Stub::new(200, "", USAGE_BODY);
        let cache = home.path().join("cache.json");
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            cache.clone(),
            Duration::from_secs(3600),
            stub.url.clone(),
        );
        let first = wait_for(&poller, |_| true);
        assert_eq!(first[0].state, UsageState::Ok);
        poller.refresh_now();
        let again = wait_for(&poller, |_| true);
        assert_eq!(again[0].five_hour.percent, Some(37.0));
        assert_eq!(stub.requests().len(), 2);
        assert_eq!(crate::claude_usage::load_cache(&cache), again);
    }

    #[test]
    fn poller_set_profiles_refreshes_new_set() {
        let home = tempfile::tempdir().unwrap();
        let stub = Stub::new(200, "", USAGE_BODY);
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            home.path().join("cache.json"),
            Duration::from_secs(3600),
            stub.url.clone(),
        );
        wait_for(&poller, |s| s.len() == 1);
        poller.set_profiles(vec![profile(home.path(), "a"), profile(home.path(), "b")]);
        let snapshot = wait_for(&poller, |s| s.len() == 2);
        assert_eq!(snapshot[1].name, "b");
    }

    #[test]
    fn poller_interval_triggers_refresh_without_commands() {
        let home = tempfile::tempdir().unwrap();
        let stub = Stub::new(200, "", USAGE_BODY);
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            home.path().join("cache.json"),
            Duration::from_millis(50),
            stub.url.clone(),
        );
        wait_for(&poller, |_| true);
        wait_for(&poller, |_| true);
        assert!(stub.requests().len() >= 2);
    }

    #[test]
    fn poller_drop_stops_thread() {
        let home = tempfile::tempdir().unwrap();
        let stub = Stub::new(200, "", USAGE_BODY);
        let poller = UsagePoller::spawn_with_base(
            vec![profile(home.path(), "a")],
            home.path().to_path_buf(),
            home.path().join("cache.json"),
            Duration::from_secs(3600),
            stub.url.clone(),
        );
        wait_for(&poller, |_| true);
        let alive = poller.alive_probe();
        assert!(alive.upgrade().is_some());
        drop(poller);
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive.upgrade().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(alive.upgrade().is_none(), "worker thread still running after drop");
    }
}
