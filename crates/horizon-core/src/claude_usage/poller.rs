//! Background poller: refreshes every profile's usage on an interval or on demand.

use std::path::PathBuf;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Weak;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{ProfileUsage, USAGE_URL, load_cache, refresh_profile, save_cache};
use crate::config::ClaudeProfile;

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
}

impl Worker {
    fn refresh(&mut self, results: &Sender<Vec<ProfileUsage>>) -> bool {
        let now_ms = now_ms();
        let snapshot: Vec<ProfileUsage> = self
            .profiles
            .iter()
            .map(|profile| {
                let previous = self.previous.iter().find(|usage| usage.name == profile.name);
                refresh_profile(profile, &self.home, &self.base_url, now_ms, previous)
            })
            .collect();
        if let Err(error) = save_cache(&self.cache_path, &snapshot) {
            tracing::debug!(%error, "failed to write claude usage cache");
        }
        self.previous.clone_from(&snapshot);
        results.send(snapshot).is_ok()
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
    #[must_use]
    pub fn spawn(profiles: Vec<ClaudeProfile>, home: PathBuf, cache_path: PathBuf, interval: Duration) -> Self {
        Self::spawn_with_base(profiles, home, cache_path, interval, USAGE_URL.to_owned())
    }

    pub(crate) fn spawn_with_base(
        profiles: Vec<ClaudeProfile>,
        home: PathBuf,
        cache_path: PathBuf,
        interval: Duration,
        base_url: String,
    ) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let alive = Arc::new(());
        let worker_alive = Arc::clone(&alive);
        let previous = load_cache(&cache_path);
        let mut worker = Worker {
            profiles,
            home,
            cache_path,
            base_url,
            previous,
        };
        let spawned = thread::Builder::new()
            .name("claude-usage-poller".to_owned())
            .spawn(move || {
                let _alive = worker_alive;
                loop {
                    if !worker.refresh(&result_tx) {
                        break;
                    }
                    match command_rx.recv_timeout(interval) {
                        Ok(Command::RefreshNow) | Err(RecvTimeoutError::Timeout) => {}
                        Ok(Command::SetProfiles(profiles)) => worker.profiles = profiles,
                        Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
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
