//! Panel link: wire protocol, target resolution and text sanitizing.
//!
//! Pure code, no IO. The unix socket server and the CLI client build on these types.

mod ask;
mod protocol;
mod resolve;
mod server;
mod text;

pub use ask::{
    AskEnd, AskTable, LinkEffect, LinkEffectKind, MAX_ASKS_PER_ASKER, MAX_ASKS_TOTAL, TargetState, ask_message,
};
pub use protocol::{
    ASK_DEFAULT_TIMEOUT_SECS, ASK_MAX_TIMEOUT_SECS, ASK_MIN_TIMEOUT_SECS, DEFAULT_READ_LINES, LINK_PROTOCOL_VERSION,
    LinkError, LinkErrorCode, LinkOk, LinkRequest, LinkResponse, MAX_READ_LINES, MAX_REQUEST_BYTES, PanelInfo,
    RequestEnvelope, SendTarget, parse_request_line,
};
pub use resolve::resolve_target;
pub use server::{LinkServer, PendingRequest, socket_path};
pub use text::{sanitize_send_text, strip_escapes};
