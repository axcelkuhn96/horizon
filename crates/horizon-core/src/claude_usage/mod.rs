//! Claude usage domain: data model, credential reader, usage-response parser,
//! HTTP client, on-disk cache and background poller.

mod api;
mod cache;
mod credentials;
mod model;
mod poller;
#[cfg(test)]
mod test_support;

use thiserror::Error;

pub use api::{USAGE_URL, fetch_usage, refresh_profile};
pub use cache::{load_cache, save_cache};
pub use credentials::{AccessToken, Credentials, read_account, read_credentials, token_expired};
pub use model::{
    ProfileUsage, UsageState, UsageWindow, parse_rfc3339_unix, parse_usage_response, remaining_percent,
    used_percent_label,
};
pub use poller::UsagePoller;

/// Failure reading credentials or fetching/parsing usage. Variants carry no
/// secret material, so they are safe to log and display.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UsageError {
    #[error("credentials missing")]
    Missing,
    #[error("malformed data")]
    Malformed,
    #[error("HTTP status {0}")]
    Http(u16),
    #[error("network error")]
    Network,
    #[error("request timed out")]
    Timeout,
}
