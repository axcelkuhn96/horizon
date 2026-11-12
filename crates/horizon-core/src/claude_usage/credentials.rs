//! Read-only access to a Claude config dir's OAuth credentials and account info.
//! Nothing here logs, prints or serializes the token or file contents.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;

use super::UsageError;

const CREDENTIALS_FILE: &str = ".credentials.json";
const ACCOUNT_FILE: &str = ".claude.json";
const EXPIRY_MARGIN_MS: i64 = 60_000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// OAuth access token. Opaque: redacted `Debug`, no `Display`, no `Serialize`.
pub struct AccessToken(String);

impl AccessToken {
    #[allow(dead_code)] // consumed by the usage API client (Task 8)
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

#[derive(Debug)]
pub struct Credentials {
    pub token: AccessToken,
    /// Expiry in Unix milliseconds; `0` when the token is empty or has no expiry.
    pub expires_at_ms: i64,
}

#[derive(Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<OauthEntry>,
}

#[derive(Deserialize)]
struct OauthEntry {
    #[serde(rename = "accessToken", default)]
    access_token: Option<String>,
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<i64>,
}

fn read_small_file(path: &Path) -> Option<String> {
    let mut buf = String::new();
    File::open(path)
        .ok()?
        .take(MAX_FILE_BYTES)
        .read_to_string(&mut buf)
        .ok()?;
    Some(buf)
}

/// Reads `<config_dir>/.credentials.json` (opened read-only).
///
/// An empty or absent token yields `Ok` with an empty token and
/// `expires_at_ms == 0`, so callers treat it as expired.
///
/// # Errors
/// `Missing` when the file is absent, unreadable, a dangling symlink or empty;
/// `Malformed` when the content is not the expected JSON shape.
pub fn read_credentials(config_dir: &Path) -> Result<Credentials, UsageError> {
    let raw = read_small_file(&config_dir.join(CREDENTIALS_FILE)).ok_or(UsageError::Missing)?;
    if raw.trim().is_empty() {
        return Err(UsageError::Missing);
    }
    let file: CredentialsFile = serde_json::from_str(&raw).map_err(|_| UsageError::Malformed)?;
    let entry = file.oauth.ok_or(UsageError::Malformed)?;
    let token = entry.access_token.unwrap_or_default();
    let expires_at_ms = if token.is_empty() {
        0
    } else {
        entry.expires_at.unwrap_or(0)
    };
    Ok(Credentials {
        token: AccessToken(token),
        expires_at_ms,
    })
}

/// True when the token expires within 60 s of `now_ms` (or already has).
#[must_use]
pub fn token_expired(expires_at_ms: i64, now_ms: i64) -> bool {
    expires_at_ms <= now_ms.saturating_add(EXPIRY_MARGIN_MS)
}

#[derive(Deserialize)]
struct AccountFile {
    #[serde(rename = "oauthAccount")]
    account: Option<Account>,
}

#[derive(Deserialize)]
struct Account {
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "emailAddress", default)]
    email_address: Option<String>,
}

/// `(display_name, email)` from `<config_dir>/.claude.json`; `(None, None)` on any problem.
#[must_use]
pub fn read_account(config_dir: &Path) -> (Option<String>, Option<String>) {
    let parsed = read_small_file(&config_dir.join(ACCOUNT_FILE))
        .and_then(|raw| serde_json::from_str::<AccountFile>(&raw).ok())
        .and_then(|file| file.account);
    parsed.map_or((None, None), |a| {
        (
            a.display_name.filter(|s| !s.is_empty()),
            a.email_address.filter(|s| !s.is_empty()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn expired_when_within_60s() {
        assert!(token_expired(1_000_000 + 60_000, 1_000_000));
        assert!(token_expired(999_999, 1_000_000));
        assert!(!token_expired(1_000_000 + 60_001, 1_000_000));
    }

    #[test]
    fn read_credentials_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_credentials(dir.path()).unwrap_err(), UsageError::Missing);
        assert_eq!(
            read_credentials(&dir.path().join("nonexistent")).unwrap_err(),
            UsageError::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_credentials_dangling_symlink_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join(CREDENTIALS_FILE)).unwrap();
        assert_eq!(read_credentials(dir.path()).unwrap_err(), UsageError::Missing);
    }

    #[test]
    fn read_credentials_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), CREDENTIALS_FILE, "{not json");
        assert_eq!(read_credentials(dir.path()).unwrap_err(), UsageError::Malformed);
        write(dir.path(), CREDENTIALS_FILE, r#"{"other":1}"#);
        assert_eq!(read_credentials(dir.path()).unwrap_err(), UsageError::Malformed);
        write(dir.path(), CREDENTIALS_FILE, "");
        assert_eq!(read_credentials(dir.path()).unwrap_err(), UsageError::Missing);
    }

    #[test]
    fn read_credentials_empty_token_is_expired_state_input() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            CREDENTIALS_FILE,
            r#"{"claudeAiOauth":{"accessToken":"","expiresAt":9999999999999}}"#,
        );
        let creds = read_credentials(dir.path()).unwrap();
        assert!(creds.token.is_empty());
        assert!(token_expired(creds.expires_at_ms, 1_000));
    }

    #[test]
    fn read_credentials_valid_returns_token_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            CREDENTIALS_FILE,
            r#"{"claudeAiOauth":{"accessToken":"synthetic-token","expiresAt":42,"refreshToken":"x"}}"#,
        );
        let creds = read_credentials(dir.path()).unwrap();
        assert_eq!(creds.token.expose(), "synthetic-token");
        assert_eq!(creds.expires_at_ms, 42);
    }

    #[test]
    fn access_token_debug_is_redacted() {
        let token = AccessToken("synthetic-secret".to_owned());
        assert_eq!(format!("{token:?}"), "AccessToken(<redacted>)");
        let creds = Credentials {
            token,
            expires_at_ms: 1,
        };
        assert!(!format!("{creds:?}").contains("synthetic-secret"));
    }

    #[test]
    fn read_account_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_account(dir.path()), (None, None));
        write(dir.path(), ACCOUNT_FILE, "{bad");
        assert_eq!(read_account(dir.path()), (None, None));
    }

    #[test]
    fn read_account_reads_oauth_account() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            ACCOUNT_FILE,
            r#"{"oauthAccount":{"displayName":"Test User","emailAddress":"t@example.com"}}"#,
        );
        assert_eq!(
            read_account(dir.path()),
            (Some("Test User".to_owned()), Some("t@example.com".to_owned()))
        );
    }

    #[test]
    fn read_credentials_does_not_modify_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        write(
            dir.path(),
            CREDENTIALS_FILE,
            r#"{"claudeAiOauth":{"accessToken":"t","expiresAt":1}}"#,
        );
        let fixed = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(fixed)).unwrap();
        read_credentials(dir.path()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), fixed);
    }
}
