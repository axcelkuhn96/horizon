//! Blocking HTTP client for the Claude OAuth usage endpoint.

use std::path::Path;
use std::time::Duration;

use ureq::Agent;

use super::{
    AccessToken, ProfileUsage, UsageError, UsageState, UsageWindow, parse_usage_response, read_account,
    read_credentials, token_expired,
};
use crate::config::{ClaudeProfile, expand_config_dir};

/// Production usage endpoint. Tests pass a loopback stub URL instead.
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

const BETA_HEADER: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: u64 = 1024 * 1024;

/// GETs `base_url` with the OAuth token and returns the raw response body.
///
/// Never follows redirects, so the bearer token only ever reaches the host in
/// `base_url`. Only `https://` is accepted, plus plain `http://` to a loopback
/// host (local test stub); URLs carrying userinfo are refused.
///
/// # Errors
/// `Http(code)` for any non-2xx status (including 3xx); `Timeout` / `Network`
/// for transport failures; `Network` for a refused URL or an oversized body.
pub fn fetch_usage(base_url: &str, token: &AccessToken) -> Result<String, UsageError> {
    if !is_permitted_url(base_url) {
        return Err(UsageError::Network);
    }
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(base_url)
        .header("Authorization", format!("Bearer {}", token.expose()))
        .header("anthropic-beta", BETA_HEADER)
        .call()
        .map_err(map_transport_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(UsageError::Http(status));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_string()
        .map_err(map_transport_error)
}

fn map_transport_error(error: ureq::Error) -> UsageError {
    match error {
        ureq::Error::Timeout(_) => UsageError::Timeout,
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::TimedOut => UsageError::Timeout,
        _ => UsageError::Network,
    }
}

/// `https://` anywhere, or `http://` to a loopback host only; never userinfo.
fn is_permitted_url(url: &str) -> bool {
    let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if secure {
        return true;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(host, _)| host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Refreshes one profile's usage.
///
/// No valid token (missing/malformed credentials file, empty or expired token)
/// never touches the network. 401/403 marks the profile `Expired`, any other
/// failure marks it `Stale`; both keep the previous values. Credentials are
/// only ever read, never written.
#[must_use]
pub fn refresh_profile(
    profile: &ClaudeProfile,
    home: &Path,
    base_url: &str,
    now_ms: i64,
    previous: Option<&ProfileUsage>,
) -> ProfileUsage {
    let config_dir = expand_config_dir(&profile.config_dir, home);
    let (display_name, email) = read_account(&config_dir, home);
    let kept = |state: UsageState| ProfileUsage {
        name: profile.name.clone(),
        display_name: display_name.clone(),
        email: email.clone(),
        five_hour: previous.map(|p| p.five_hour.clone()).unwrap_or_default(),
        seven_day: previous.map(|p| p.seven_day.clone()).unwrap_or_default(),
        severity: previous.and_then(|p| p.severity.clone()),
        state,
        fetched_at_ms: previous.and_then(|p| p.fetched_at_ms),
    };

    let credentials = match read_credentials(&config_dir) {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::debug!(profile = %profile.name, %error, "claude credentials unavailable");
            return ProfileUsage {
                five_hour: UsageWindow::default(),
                seven_day: UsageWindow::default(),
                severity: None,
                fetched_at_ms: None,
                ..kept(UsageState::Missing)
            };
        }
    };
    if credentials.token.is_empty() || token_expired(credentials.expires_at_ms, now_ms) {
        return kept(UsageState::Expired);
    }

    let parsed = fetch_usage(base_url, &credentials.token).and_then(|body| parse_usage_response(&body));
    match parsed {
        Ok((five_hour, seven_day, severity)) => ProfileUsage {
            five_hour,
            seven_day,
            severity,
            fetched_at_ms: Some(now_ms),
            ..kept(UsageState::Ok)
        },
        Err(UsageError::Http(401 | 403)) => kept(UsageState::Expired),
        Err(error) => {
            tracing::debug!(profile = %profile.name, %error, "claude usage refresh failed");
            kept(UsageState::Stale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_usage::read_credentials;
    use crate::claude_usage::test_support::{FAR_FUTURE_MS, Stub, TEST_TOKEN, USAGE_BODY, write_credentials};

    use crate::claude_usage::test_support::closed_port_url;
    use std::fs;
    use std::path::PathBuf;

    const NOW_MS: i64 = 1_000_000;

    struct Fixture {
        home: tempfile::TempDir,
        profile: ClaudeProfile,
    }

    fn fixture(token: &str, expires_at_ms: i64) -> Fixture {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude-work");
        write_credentials(&dir, token, expires_at_ms);
        fs::write(
            dir.join(".claude.json"),
            r#"{"oauthAccount":{"displayName":"Ada","emailAddress":"ada@example.com"}}"#,
        )
        .unwrap();
        let profile = ClaudeProfile {
            name: "work".to_owned(),
            config_dir: "~/.claude-work".to_owned(),
        };
        Fixture { home, profile }
    }

    fn previous() -> ProfileUsage {
        ProfileUsage {
            name: "work".to_owned(),
            display_name: Some("Ada".to_owned()),
            email: Some("ada@example.com".to_owned()),
            five_hour: UsageWindow {
                percent: Some(50.0),
                resets_at: Some("2026-11-12T17:00:00Z".to_owned()),
            },
            seven_day: UsageWindow {
                percent: Some(20.0),
                resets_at: None,
            },
            severity: Some("warning".to_owned()),
            state: UsageState::Ok,
            fetched_at_ms: Some(500),
        }
    }

    #[test]
    fn refresh_ok_parses_body_and_account() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let stub = Stub::new(200, "", USAGE_BODY);
        let usage = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, None);
        assert_eq!(usage.state, UsageState::Ok);
        assert_eq!(usage.name, "work");
        assert_eq!(usage.display_name.as_deref(), Some("Ada"));
        assert_eq!(usage.email.as_deref(), Some("ada@example.com"));
        assert_eq!(usage.five_hour.percent, Some(37.0));
        assert_eq!(usage.seven_day.percent, Some(12.0));
        assert_eq!(usage.fetched_at_ms, Some(NOW_MS));
    }

    #[test]
    fn refresh_expired_token_skips_http() {
        let fx = fixture(TEST_TOKEN, NOW_MS + 30_000);
        let stub = Stub::new(200, "", USAGE_BODY);
        let usage = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, Some(&previous()));
        assert_eq!(usage.state, UsageState::Expired);
        assert_eq!(usage.five_hour.percent, Some(50.0));
        assert!(stub.requests().is_empty());
    }

    #[test]
    fn refresh_missing_or_malformed_credentials_skip_http() {
        let stub = Stub::new(200, "", USAGE_BODY);
        let home = tempfile::tempdir().unwrap();
        let profile = ClaudeProfile {
            name: "ghost".to_owned(),
            config_dir: "~/.claude-ghost".to_owned(),
        };
        let usage = refresh_profile(&profile, home.path(), &stub.url, NOW_MS, None);
        assert_eq!(usage.state, UsageState::Missing);
        assert_eq!(usage.five_hour, UsageWindow::default());

        let dir: PathBuf = home.path().join(".claude-ghost");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".credentials.json"), "{not json").unwrap();
        let usage = refresh_profile(&profile, home.path(), &stub.url, NOW_MS, None);
        assert_eq!(usage.state, UsageState::Missing);
        assert!(stub.requests().is_empty());
    }

    #[test]
    fn refresh_network_error_keeps_previous_as_stale() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let usage = refresh_profile(
            &fx.profile,
            fx.home.path(),
            &closed_port_url(),
            NOW_MS,
            Some(&previous()),
        );
        assert_eq!(usage.state, UsageState::Stale);
        assert_eq!(usage.five_hour.percent, Some(50.0));
        assert_eq!(usage.severity.as_deref(), Some("warning"));
        assert_eq!(usage.fetched_at_ms, Some(500));
    }

    #[test]
    fn refresh_401_marks_expired_keeps_values() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let stub = Stub::new(401, "", "{}");
        let usage = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, Some(&previous()));
        assert_eq!(usage.state, UsageState::Expired);
        assert_eq!(usage.five_hour.percent, Some(50.0));
        assert_eq!(usage.seven_day.percent, Some(20.0));
        assert_eq!(usage.fetched_at_ms, Some(500));
    }

    #[test]
    fn refresh_server_error_and_garbage_body_are_stale() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        for stub in [Stub::new(500, "", "{}"), Stub::new(200, "", "not json")] {
            let usage = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, Some(&previous()));
            assert_eq!(usage.state, UsageState::Stale);
            assert_eq!(usage.five_hour.percent, Some(50.0));
        }
    }

    #[test]
    fn refresh_without_previous_has_empty_windows_when_stale() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let usage = refresh_profile(&fx.profile, fx.home.path(), &closed_port_url(), NOW_MS, None);
        assert_eq!(usage.state, UsageState::Stale);
        assert_eq!(usage.five_hour, UsageWindow::default());
        assert_eq!(usage.fetched_at_ms, None);
    }

    #[test]
    fn cache_round_trip_has_no_token_strings() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let stub = Stub::new(200, "", USAGE_BODY);
        let usage = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, None);
        let path = fx.home.path().join("claude-usage-cache.json");
        crate::claude_usage::save_cache(&path, &[usage.clone()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        for forbidden in ["accessToken", "refreshToken", TEST_TOKEN, "sk-refresh-SECRET"] {
            assert!(!raw.contains(forbidden), "cache leaked {forbidden}");
        }
        assert_eq!(crate::claude_usage::load_cache(&path), vec![usage]);
    }

    #[test]
    fn credential_files_mtime_unchanged_after_two_refreshes() {
        let fx = fixture(TEST_TOKEN, FAR_FUTURE_MS);
        let stub = Stub::new(200, "", USAGE_BODY);
        let dir = fx.home.path().join(".claude-work");
        let old = filetime::FileTime::from_unix_time(1_600_000_000, 0);
        let files = [dir.join(".credentials.json"), dir.join(".claude.json")];
        for file in &files {
            filetime::set_file_mtime(file, old).unwrap();
        }
        for _ in 0..2 {
            let _ = refresh_profile(&fx.profile, fx.home.path(), &stub.url, NOW_MS, None);
        }
        for file in &files {
            let mtime = filetime::FileTime::from_last_modification_time(&fs::metadata(file).unwrap());
            assert_eq!(mtime, old, "{} was modified", file.display());
        }
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2, "no new files in the config dir");
    }

    fn token() -> AccessToken {
        let dir = tempfile::tempdir().unwrap();
        write_credentials(dir.path(), TEST_TOKEN, FAR_FUTURE_MS);
        read_credentials(dir.path()).unwrap().token
    }

    #[test]
    fn fetch_sends_required_headers() {
        let stub = Stub::new(200, "", USAGE_BODY);
        let body = fetch_usage(&stub.url, &token()).unwrap();
        assert_eq!(body, USAGE_BODY);
        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        let request = requests[0].to_ascii_lowercase();
        assert!(request.starts_with("get /api/oauth/usage "), "{request}");
        assert!(request.contains(&format!("authorization: bearer {}", TEST_TOKEN.to_ascii_lowercase())));
        assert!(request.contains("anthropic-beta: oauth-2025-04-20"));
    }

    #[test]
    fn fetch_401_maps_to_http_error() {
        let stub = Stub::new(401, "", "{}");
        assert_eq!(fetch_usage(&stub.url, &token()), Err(UsageError::Http(401)));
        let stub = Stub::new(403, "", "{}");
        assert_eq!(fetch_usage(&stub.url, &token()), Err(UsageError::Http(403)));
        let stub = Stub::new(500, "", "{}");
        assert_eq!(fetch_usage(&stub.url, &token()), Err(UsageError::Http(500)));
    }

    #[test]
    fn fetch_does_not_follow_redirects() {
        let target = Stub::new(200, "", USAGE_BODY);
        let redirect = Stub::new(302, &format!("Location: {}\r\n", target.url), "");
        assert_eq!(fetch_usage(&redirect.url, &token()), Err(UsageError::Http(302)));
        assert!(target.requests().is_empty(), "token must not reach the redirect target");
    }

    #[test]
    fn fetch_refuses_cleartext_remote_and_userinfo_urls() {
        let token = token();
        for url in [
            "http://example.com/api/oauth/usage",
            "http://127.0.0.1@example.com/api/oauth/usage",
            "https://user:pw@example.com/api/oauth/usage",
            "ftp://example.com/",
            "",
        ] {
            assert_eq!(fetch_usage(url, &token), Err(UsageError::Network), "{url}");
        }
    }

    #[test]
    fn fetch_connection_refused_is_network_error() {
        let url = crate::claude_usage::test_support::closed_port_url();
        assert_eq!(fetch_usage(&url, &token()), Err(UsageError::Network));
    }
}
