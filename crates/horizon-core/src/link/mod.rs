//! Panel link: wire protocol, target resolution and text sanitizing.
//!
//! Pure code, no IO. The unix socket server and the CLI client build on these types.

mod protocol;
mod resolve;
mod text;

pub use protocol::{
    DEFAULT_READ_LINES, LINK_PROTOCOL_VERSION, LinkError, LinkErrorCode, LinkOk, LinkRequest, LinkResponse,
    MAX_READ_LINES, MAX_REQUEST_BYTES, PanelInfo, RequestEnvelope, parse_request_line,
};
pub use resolve::resolve_target;
pub use text::{sanitize_send_text, strip_escapes};
