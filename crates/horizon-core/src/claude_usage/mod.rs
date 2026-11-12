//! Claude usage domain: data model, credential reader and usage-response parser.

mod credentials;
mod model;

use thiserror::Error;

pub use credentials::{AccessToken, Credentials, read_account, read_credentials, token_expired};
pub use model::{
    ProfileUsage, UsageState, UsageWindow, parse_rfc3339_unix, parse_usage_response, remaining_percent,
    used_percent_label,
};

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
