//! Profile list editing, validation and the cached credential facts behind the
//! Claude settings tab. Pure logic: no egui types beyond `Color32`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Color32, Context};
use horizon_core::claude_usage::{read_account, read_credentials, token_expired};
use horizon_core::{ClaudeProfile, expand_config_dir};

use crate::theme;

/// How long credential facts are trusted before they are read again, so a
/// `claude login` in a terminal shows up without reopening the tab.
pub(super) const INFO_TTL: Duration = Duration::from_secs(30);

/// State of a profile's OAuth credentials. Carries no token data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialState {
    Ok,
    Expired,
    Missing,
}

impl CredentialState {
    pub(super) const ALL: [Self; 3] = [Self::Ok, Self::Expired, Self::Missing];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Expired => "expired",
            Self::Missing => "missing",
        }
    }

    pub(super) fn color(self) -> Color32 {
        match self {
            Self::Ok => theme::PALETTE_GREEN(),
            Self::Expired => theme::PALETTE_YELLOW(),
            Self::Missing => theme::PALETTE_RED(),
        }
    }

    pub(super) fn tooltip(self) -> &'static str {
        match self {
            Self::Ok => "Signed in: credentials found and valid",
            Self::Expired => "Token expired: open this profile in a terminal to renew",
            Self::Missing => "No credentials found: run claude in a terminal with this profile to sign in",
        }
    }
}

/// Credential state of `config_dir` at `now_ms` (Unix milliseconds).
/// Unreadable, empty or malformed credential files all count as `Missing`.
pub(crate) fn credential_state(config_dir: &Path, now_ms: i64) -> CredentialState {
    match read_credentials(config_dir) {
        Ok(credentials) if token_expired(credentials.expires_at_ms, now_ms) => CredentialState::Expired,
        Ok(_) => CredentialState::Ok,
        Err(_) => CredentialState::Missing,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

// -- Pure list editing ------------------------------------------------------

fn unique_profile_name(profiles: &[ClaudeProfile]) -> String {
    let taken = |name: &str| profiles.iter().any(|p| p.name.trim().eq_ignore_ascii_case(name));
    let base = "profile";
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_string())
}

/// Appends an empty profile with a unique name and returns its index.
pub(super) fn add_profile(profiles: &mut Vec<ClaudeProfile>) -> usize {
    profiles.push(ClaudeProfile {
        name: unique_profile_name(profiles),
        config_dir: String::new(),
    });
    profiles.len() - 1
}

pub(super) fn remove_profile(profiles: &mut Vec<ClaudeProfile>, index: usize) -> bool {
    if index < profiles.len() {
        profiles.remove(index);
        true
    } else {
        false
    }
}

/// Moves the profile at `from` so it ends up at `to`; out-of-range is a no-op.
pub(super) fn move_profile(profiles: &mut Vec<ClaudeProfile>, from: usize, to: usize) {
    if from == to || from >= profiles.len() || to >= profiles.len() {
        return;
    }
    let profile = profiles.remove(from);
    profiles.insert(to, profile);
}

/// Appends detected profiles whose name (case-insensitive) and directory are
/// both unseen; returns how many were added.
pub(super) fn merge_detected(existing: &mut Vec<ClaudeProfile>, detected: Vec<ClaudeProfile>, home: &Path) -> usize {
    let mut added = 0;
    for profile in detected {
        let name = profile.name.trim().to_lowercase();
        let dir = expand_config_dir(profile.config_dir.trim(), home);
        let known = existing
            .iter()
            .any(|p| p.name.trim().to_lowercase() == name || expand_config_dir(p.config_dir.trim(), home) == dir);
        if !known {
            existing.push(profile);
            added += 1;
        }
    }
    added
}

// -- Validation -------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IssueField {
    Name,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RowIssue {
    pub(super) field: IssueField,
    /// Errors block saving; warnings (missing directory) do not.
    pub(super) blocking: bool,
    pub(super) message: String,
}

/// First validation problem of the profile at `index`. Mirrors
/// `validate_claude_profiles` (which gates saving) with per-row granularity.
pub(super) fn row_issue(profiles: &[ClaudeProfile], index: usize, dir_exists: bool) -> Option<RowIssue> {
    let profile = profiles.get(index)?;
    let name = profile.name.trim();
    let issue = |field, blocking, message: &str| {
        Some(RowIssue {
            field,
            blocking,
            message: message.to_string(),
        })
    };
    if name.is_empty() {
        return issue(IssueField::Name, true, "Name must not be empty");
    }
    let lower = name.to_lowercase();
    if profiles[..index].iter().any(|p| p.name.trim().to_lowercase() == lower) {
        return issue(IssueField::Name, true, "Another profile already uses this name");
    }
    if profile.config_dir.trim().is_empty() {
        return issue(IssueField::Dir, true, "Config directory must not be empty");
    }
    if !dir_exists {
        return issue(IssueField::Dir, false, "Directory does not exist yet");
    }
    None
}

// -- Per-profile facts ---------------------------------------------------------

#[derive(Clone, Debug)]
pub(super) struct ProfileInfo {
    pub(super) credentials: CredentialState,
    pub(super) dir_exists: bool,
    /// `"Name · email"` of the signed-in account, when Claude Code recorded one.
    pub(super) account: Option<String>,
}

/// Reads the filesystem (credentials, account file): never call on the UI thread.
pub(super) fn inspect_profile(raw_dir: &str, home: &Path, now_ms: i64) -> ProfileInfo {
    let raw_dir = raw_dir.trim();
    if raw_dir.is_empty() {
        return ProfileInfo {
            credentials: CredentialState::Missing,
            dir_exists: false,
            account: None,
        };
    }
    let dir = expand_config_dir(raw_dir, home);
    let dir_exists = dir.is_dir();
    let (name, email) = if dir_exists {
        read_account(&dir, home)
    } else {
        (None, None)
    };
    let account = match (name, email) {
        (Some(name), Some(email)) => Some(format!("{name} \u{b7} {email}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    };
    ProfileInfo {
        credentials: credential_state(&dir, now_ms),
        dir_exists,
        account,
    }
}

struct CachedInfo {
    info: ProfileInfo,
    fetched: Instant,
    /// Forced refresh requested (tab reopened, accounts detected); the old
    /// value keeps showing until the new one arrives.
    stale: bool,
}

/// Credential facts per raw `config_dir` string, refreshed off the UI thread:
/// at most one background read in flight, only for dirs currently on screen,
/// re-read after [`INFO_TTL`].
pub(super) struct InfoCache {
    entries: HashMap<String, CachedInfo>,
    in_flight: Option<String>,
    tx: mpsc::Sender<(String, ProfileInfo)>,
    rx: mpsc::Receiver<(String, ProfileInfo)>,
}

impl Default for InfoCache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            entries: HashMap::new(),
            in_flight: None,
            tx,
            rx,
        }
    }
}

impl InfoCache {
    pub(super) fn get(&self, raw_dir: &str) -> Option<&ProfileInfo> {
        self.entries.get(raw_dir).map(|entry| &entry.info)
    }

    /// Marks everything for a re-read while keeping the current values visible.
    pub(super) fn invalidate(&mut self) {
        for entry in self.entries.values_mut() {
            entry.stale = true;
        }
    }

    /// The first of `dirs` that has no value yet or an outdated one, unless a
    /// read is already in flight.
    fn next_to_fetch(&self, dirs: &[&str], now: Instant) -> Option<String> {
        if self.in_flight.is_some() {
            return None;
        }
        dirs.iter()
            .find(|dir| {
                self.entries
                    .get(**dir)
                    .is_none_or(|entry| entry.stale || now.saturating_duration_since(entry.fetched) >= INFO_TTL)
            })
            .map(|dir| (*dir).to_string())
    }

    fn store(&mut self, raw_dir: String, info: ProfileInfo, now: Instant) {
        if self.in_flight.as_deref() == Some(raw_dir.as_str()) {
            self.in_flight = None;
        }
        self.entries.insert(
            raw_dir,
            CachedInfo {
                info,
                fetched: now,
                stale: false,
            },
        );
    }

    /// Drains finished reads, forgets dirs no longer shown, resolves blank dirs
    /// (no I/O) inline and starts at most one background read.
    pub(super) fn refresh(&mut self, ctx: &Context, dirs: &[&str], home: &Path) {
        let now = Instant::now();
        while let Ok((raw_dir, info)) = self.rx.try_recv() {
            self.store(raw_dir, info, now);
        }
        self.entries.retain(|key, _| dirs.contains(&key.as_str()));
        for dir in dirs.iter().filter(|dir| dir.trim().is_empty()) {
            if !self.entries.contains_key(*dir) {
                self.store((*dir).to_string(), inspect_profile(dir, home, 0), now);
            }
        }
        let Some(raw_dir) = self.next_to_fetch(dirs, now) else {
            return;
        };
        self.in_flight = Some(raw_dir.clone());
        let (tx, ctx, home) = (self.tx.clone(), ctx.clone(), home.to_path_buf());
        std::thread::spawn(move || {
            let info = inspect_profile(&raw_dir, &home, now_ms());
            let _ = tx.send((raw_dir, info));
            ctx.request_repaint();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000;

    fn profile(name: &str, dir: &str) -> ClaudeProfile {
        ClaudeProfile {
            name: name.to_string(),
            config_dir: dir.to_string(),
        }
    }

    fn write_credentials(dir: &Path, body: &str) {
        std::fs::write(dir.join(".credentials.json"), body).unwrap();
    }

    fn creds(expires_at: i64) -> String {
        format!(r#"{{"claudeAiOauth":{{"accessToken":"sk-secret-token","expiresAt":{expires_at}}}}}"#)
    }

    fn info(credentials: CredentialState) -> ProfileInfo {
        ProfileInfo {
            credentials,
            dir_exists: true,
            account: None,
        }
    }

    #[test]
    fn credential_state_ok_expired_missing() {
        let ok = tempfile::tempdir().unwrap();
        write_credentials(ok.path(), &creds(NOW + 60_001));
        assert_eq!(credential_state(ok.path(), NOW), CredentialState::Ok);

        let expired = tempfile::tempdir().unwrap();
        write_credentials(expired.path(), &creds(NOW + 60_000));
        assert_eq!(credential_state(expired.path(), NOW), CredentialState::Expired);

        let past = tempfile::tempdir().unwrap();
        write_credentials(past.path(), &creds(NOW - 5_000));
        assert_eq!(credential_state(past.path(), NOW), CredentialState::Expired);

        let missing = tempfile::tempdir().unwrap();
        assert_eq!(credential_state(missing.path(), NOW), CredentialState::Missing);
        assert_eq!(
            credential_state(&missing.path().join("nonexistent"), NOW),
            CredentialState::Missing
        );

        let malformed = tempfile::tempdir().unwrap();
        write_credentials(malformed.path(), "{ not json");
        assert_eq!(credential_state(malformed.path(), NOW), CredentialState::Missing);

        let empty = tempfile::tempdir().unwrap();
        write_credentials(empty.path(), "");
        assert_eq!(credential_state(empty.path(), NOW), CredentialState::Missing);
    }

    #[test]
    fn credential_state_never_exposes_token() {
        let dir = tempfile::tempdir().unwrap();
        write_credentials(dir.path(), &creds(NOW + 600_000));
        let state = credential_state(dir.path(), NOW);
        let rendered = format!("{state:?} {} {}", state.label(), state.tooltip());
        assert!(!rendered.contains("sk-secret-token"));
        // The cached per-profile facts hold no token either.
        let info = inspect_profile(dir.path().to_str().unwrap(), Path::new("/nonexistent-home"), NOW);
        assert_eq!(info.credentials, CredentialState::Ok);
        assert!(info.account.is_none());
    }

    #[test]
    fn inspect_profile_with_empty_dir_does_not_touch_the_filesystem() {
        let info = inspect_profile("  ", Path::new("/nonexistent-home"), NOW);
        assert_eq!(info.credentials, CredentialState::Missing);
        assert!(!info.dir_exists);
    }

    #[test]
    fn merge_detected_skips_known() {
        let home = Path::new("/home/me");
        let mut existing = vec![profile("Work", "/home/me/.claude-work/"), profile("x", "~/.claude-x")];
        let detected = vec![
            profile("work", "~/.claude-w2"),
            profile("other", "~/.claude-work"),
            profile("claude", "~/.claude"),
            profile("x2", "/home/me/.claude-x"),
        ];
        assert_eq!(merge_detected(&mut existing, detected, home), 1);
        assert_eq!(existing.len(), 3);
        assert_eq!(existing[2], profile("claude", "~/.claude"));
        assert_eq!(
            merge_detected(&mut existing, vec![profile("claude", "~/.claude")], home),
            0
        );
    }

    #[test]
    fn move_profile_reorders() {
        let mut list = vec![profile("a", "1"), profile("b", "2"), profile("c", "3")];
        move_profile(&mut list, 0, 1);
        assert_eq!(names(&list), ["b", "a", "c"]);
        move_profile(&mut list, 2, 0);
        assert_eq!(names(&list), ["c", "b", "a"]);
        move_profile(&mut list, 1, 1);
        move_profile(&mut list, 0, 3);
        move_profile(&mut list, 5, 0);
        assert_eq!(names(&list), ["c", "b", "a"]);
    }

    #[test]
    fn add_and_remove_profile() {
        let mut list = vec![profile("profile", "~/.claude-a")];
        assert_eq!(add_profile(&mut list), 1);
        assert_eq!(list[1].name, "profile 2");
        assert!(list[1].config_dir.is_empty());
        assert_eq!(add_profile(&mut list), 2);
        assert_eq!(list[2].name, "profile 3");
        assert!(remove_profile(&mut list, 0));
        assert!(!remove_profile(&mut list, 9));
        assert_eq!(names(&list), ["profile 2", "profile 3"]);
    }

    #[test]
    fn row_issue_covers_name_dir_and_warning() {
        let list = vec![
            profile("a", "~/.claude-a"),
            profile(" A ", "~/.claude-b"),
            profile("", "~/.claude-c"),
            profile("d", "  "),
            profile("e", "~/.claude-e"),
        ];
        assert_eq!(row_issue(&list, 0, true), None);
        let dup = row_issue(&list, 1, true).unwrap();
        assert_eq!((dup.field, dup.blocking), (IssueField::Name, true));
        assert_eq!(row_issue(&list, 2, true).unwrap().field, IssueField::Name);
        let empty_dir = row_issue(&list, 3, true).unwrap();
        assert_eq!((empty_dir.field, empty_dir.blocking), (IssueField::Dir, true));
        let missing_dir = row_issue(&list, 4, false).unwrap();
        assert_eq!((missing_dir.field, missing_dir.blocking), (IssueField::Dir, false));
        assert_eq!(row_issue(&list, 9, true), None);
    }

    #[test]
    fn blocking_row_issues_agree_with_core_validation() {
        let cases = [
            vec![profile("a", "~/.claude-a"), profile("b", "~/.claude-b")],
            vec![profile("a", "~/x"), profile("A", "~/y")],
            vec![profile(" ", "~/x")],
            vec![profile("a", "")],
        ];
        for list in cases {
            let blocking = (0..list.len()).any(|i| row_issue(&list, i, true).is_some_and(|issue| issue.blocking));
            assert_eq!(blocking, horizon_core::validate_claude_profiles(&list).is_err());
        }
    }

    #[test]
    fn cache_fetches_unknown_dirs_one_at_a_time() {
        let mut cache = InfoCache::default();
        let now = Instant::now();
        assert_eq!(cache.next_to_fetch(&["~/a", "~/b"], now).as_deref(), Some("~/a"));
        cache.in_flight = Some("~/a".to_string());
        assert_eq!(cache.next_to_fetch(&["~/a", "~/b"], now), None);
        cache.store("~/a".to_string(), info(CredentialState::Ok), now);
        assert_eq!(cache.in_flight, None);
        assert_eq!(cache.next_to_fetch(&["~/a", "~/b"], now).as_deref(), Some("~/b"));
    }

    #[test]
    fn cache_entry_expires_after_the_ttl() {
        let mut cache = InfoCache::default();
        let start = Instant::now();
        cache.store("~/a".to_string(), info(CredentialState::Expired), start);
        let almost = start + INFO_TTL - Duration::from_millis(1);
        assert_eq!(cache.next_to_fetch(&["~/a"], almost), None);
        assert_eq!(cache.next_to_fetch(&["~/a"], start + INFO_TTL).as_deref(), Some("~/a"));
        // The outdated value keeps showing until the new read lands.
        assert_eq!(cache.get("~/a").unwrap().credentials, CredentialState::Expired);
    }

    #[test]
    fn cache_invalidate_forces_a_reread_and_keeps_the_value() {
        let mut cache = InfoCache::default();
        let now = Instant::now();
        cache.store("~/a".to_string(), info(CredentialState::Ok), now);
        assert_eq!(cache.next_to_fetch(&["~/a"], now), None);
        cache.invalidate();
        assert_eq!(cache.next_to_fetch(&["~/a"], now).as_deref(), Some("~/a"));
        assert!(cache.get("~/a").is_some());
        cache.store("~/a".to_string(), info(CredentialState::Ok), now);
        assert_eq!(cache.next_to_fetch(&["~/a"], now), None);
    }

    fn names(list: &[ClaudeProfile]) -> Vec<&str> {
        list.iter().map(|p| p.name.as_str()).collect()
    }
}
