use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const LINK_PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_READ_LINES: usize = 500;
pub const DEFAULT_READ_LINES: usize = 50;
/// Seconds an `ask` waits for its reply when the client names no timeout.
pub const ASK_DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const ASK_MIN_TIMEOUT_SECS: u64 = 5;
pub const ASK_MAX_TIMEOUT_SECS: u64 = 600;

fn default_read_lines() -> usize {
    DEFAULT_READ_LINES
}

fn default_ask_timeout() -> u64 {
    ASK_DEFAULT_TIMEOUT_SECS
}

/// Who a `send` is addressed to. On the wire a plain string names one panel; the
/// workspace broadcast is `{"scope":"workspace"}` (the workspace is the sender's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "SendTargetWire", into = "SendTargetWire")]
pub enum SendTarget {
    Panel(String),
    AllInWorkspace,
}

impl SendTarget {
    #[must_use]
    pub fn panel(target: impl Into<String>) -> Self {
        Self::Panel(target.into())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SendTargetWire {
    Panel(String),
    Scope { scope: ScopeWire },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopeWire {
    Workspace,
}

impl From<SendTargetWire> for SendTarget {
    fn from(wire: SendTargetWire) -> Self {
        match wire {
            SendTargetWire::Panel(target) => Self::Panel(target),
            SendTargetWire::Scope {
                scope: ScopeWire::Workspace,
            } => Self::AllInWorkspace,
        }
    }
}

impl From<SendTarget> for SendTargetWire {
    fn from(target: SendTarget) -> Self {
        match target {
            SendTarget::Panel(target) => Self::Panel(target),
            SendTarget::AllInWorkspace => Self::Scope {
                scope: ScopeWire::Workspace,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LinkRequest {
    List,
    Send {
        target: SendTarget,
        text: String,
        #[serde(default)]
        enter: bool,
        /// Name of the sending panel, shown on the receiver's badge. Optional so older
        /// clients keep working; the receiver cleans and caps it before use.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// `HORIZON_PANEL_ID` of the sending panel: the workspace of a broadcast and the
        /// origin of the visual link line. Required for `AllInWorkspace`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_id: Option<String>,
    },
    /// Asks an agent panel a question and holds the connection until it answers.
    Ask {
        /// `HORIZON_PANEL_ID` of the asking panel.
        from: String,
        target: String,
        text: String,
        /// Seconds to wait for the reply; clamped to the allowed range when parsed.
        #[serde(default = "default_ask_timeout")]
        timeout_secs: u64,
    },
    /// Answers a pending ask; only the asked panel may.
    Reply {
        /// `HORIZON_PANEL_ID` of the replying panel.
        from: String,
        ask_id: String,
        text: String,
    },
    Read {
        target: String,
        #[serde(default = "default_read_lines")]
        lines: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub v: u32,
    #[serde(flatten)]
    pub request: LinkRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelInfo {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub kind: String,
    pub is_agent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum LinkOk {
    Panels(Vec<PanelInfo>),
    /// Local ids of the panels that got the message and of those that were skipped
    /// (broadcast only; a single send lists its target as delivered).
    Sent {
        delivered: Vec<String>,
        skipped: Vec<String>,
    },
    Text(String),
    /// The answer to an `ask`.
    Reply {
        from: String,
        text: String,
    },
}

impl LinkOk {
    /// `Sent` for one delivered panel.
    #[must_use]
    pub fn sent_to(local_id: &str) -> Self {
        Self::Sent {
            delivered: vec![local_id.to_owned()],
            skipped: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkErrorCode {
    BadRequest,
    VersionMismatch,
    TooLarge,
    NotFound,
    Ambiguous,
    Closed,
    AskTimeout,
    AskTargetClosed,
    AskCancelled,
    UnknownAsk,
    NotAddressee,
    AskExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LinkResponse {
    Ok(LinkOk),
    Err { code: LinkErrorCode, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LinkError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("protocol version mismatch: got {got}, expected {expected}")]
    VersionMismatch { got: u64, expected: u32 },
    #[error("request exceeds {MAX_REQUEST_BYTES} bytes")]
    TooLarge,
    #[error("no panel matches '{0}'")]
    NotFound(String),
    #[error("'{target}' is ambiguous, candidates: {candidates}")]
    Ambiguous { target: String, candidates: String },
    #[error("panel is closed")]
    Closed,
    #[error("no reply within {0} seconds")]
    AskTimeout(u64),
    #[error("the asked panel closed before replying")]
    AskTargetClosed,
    #[error("the asking panel closed; the ask was cancelled")]
    AskCancelled,
    #[error("no pending ask '{0}'")]
    UnknownAsk(String),
    #[error("ask '{0}' was addressed to another panel")]
    NotAddressee(String),
    #[error("ask '{0}' already expired or was resolved")]
    AskExpired(String),
    /// Server-side failure (socket setup, shutdown); reported to clients as `bad_request`.
    #[error("link server error: {0}")]
    Io(String),
    #[error("panel link is not supported on this platform")]
    Unsupported,
}

impl LinkError {
    #[must_use]
    pub fn code(&self) -> LinkErrorCode {
        match self {
            Self::BadRequest(_) | Self::Io(_) | Self::Unsupported => LinkErrorCode::BadRequest,
            Self::VersionMismatch { .. } => LinkErrorCode::VersionMismatch,
            Self::TooLarge => LinkErrorCode::TooLarge,
            Self::NotFound(_) => LinkErrorCode::NotFound,
            Self::Ambiguous { .. } => LinkErrorCode::Ambiguous,
            Self::Closed => LinkErrorCode::Closed,
            Self::AskTimeout(_) => LinkErrorCode::AskTimeout,
            Self::AskTargetClosed => LinkErrorCode::AskTargetClosed,
            Self::AskCancelled => LinkErrorCode::AskCancelled,
            Self::UnknownAsk(_) => LinkErrorCode::UnknownAsk,
            Self::NotAddressee(_) => LinkErrorCode::NotAddressee,
            Self::AskExpired(_) => LinkErrorCode::AskExpired,
        }
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    v: Option<serde_json::Value>,
}

/// Parses one request line: size, UTF-8, protocol version, then the request itself.
///
/// # Errors
///
/// `TooLarge` over [`MAX_REQUEST_BYTES`], `VersionMismatch` for a version other than
/// [`LINK_PROTOCOL_VERSION`], `BadRequest` for invalid UTF-8, JSON or request shape.
pub fn parse_request_line(line: &[u8]) -> Result<LinkRequest, LinkError> {
    if line.len() > MAX_REQUEST_BYTES {
        return Err(LinkError::TooLarge);
    }
    let text = std::str::from_utf8(line).map_err(|_| LinkError::BadRequest("request is not valid UTF-8".to_owned()))?;
    let probe: VersionProbe = serde_json::from_str(text).map_err(|err| LinkError::BadRequest(err.to_string()))?;
    let version = probe
        .v
        .ok_or_else(|| LinkError::BadRequest("missing protocol version 'v'".to_owned()))?;
    let version = version
        .as_u64()
        .ok_or_else(|| LinkError::BadRequest("protocol version 'v' must be an integer".to_owned()))?;
    if version != u64::from(LINK_PROTOCOL_VERSION) {
        return Err(LinkError::VersionMismatch {
            got: version,
            expected: LINK_PROTOCOL_VERSION,
        });
    }
    let envelope: RequestEnvelope = serde_json::from_str(text).map_err(|err| LinkError::BadRequest(err.to_string()))?;
    Ok(match envelope.request {
        LinkRequest::Read { target, lines } => LinkRequest::Read {
            target,
            lines: lines.clamp(1, MAX_READ_LINES),
        },
        LinkRequest::Ask {
            from,
            target,
            text,
            timeout_secs,
        } => LinkRequest::Ask {
            from,
            target,
            text,
            timeout_secs: timeout_secs.clamp(ASK_MIN_TIMEOUT_SECS, ASK_MAX_TIMEOUT_SECS),
        },
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_over_64k() {
        let line = vec![b' '; MAX_REQUEST_BYTES + 1];
        assert_eq!(parse_request_line(&line), Err(LinkError::TooLarge));
        assert_eq!(LinkError::TooLarge.code(), LinkErrorCode::TooLarge);
    }

    #[test]
    fn parse_accepts_exactly_64k() {
        let mut line = br#"{"v":1,"op":"list"}"#.to_vec();
        line.resize(MAX_REQUEST_BYTES, b' ');
        assert_eq!(parse_request_line(&line), Ok(LinkRequest::List));
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let err = parse_request_line(br#"{"v":2,"op":"list"}"#).unwrap_err();
        assert_eq!(err.code(), LinkErrorCode::VersionMismatch);
        let err = parse_request_line(br#"{"v":2,"op":"from_the_future"}"#).unwrap_err();
        assert_eq!(err.code(), LinkErrorCode::VersionMismatch);
    }

    #[test]
    fn parse_rejects_malformed() {
        for line in [
            &b"not json"[..],
            br#"{"op":"list"}"#,
            br#"{"v":"1","op":"list"}"#,
            br#"{"v":1,"op":"explode"}"#,
            br#"{"v":1,"op":"send","target":"a"}"#,
            &[0xff, 0xfe][..],
        ] {
            let err = parse_request_line(line).unwrap_err();
            assert_eq!(err.code(), LinkErrorCode::BadRequest, "{line:?}");
        }
    }

    #[test]
    fn parse_clamps_lines() {
        let read = |lines: &str| {
            let raw = format!(r#"{{"v":1,"op":"read","target":"a","lines":{lines}}}"#);
            parse_request_line(raw.as_bytes())
        };
        let lines_of = |req| match req {
            Ok(LinkRequest::Read { lines, .. }) => lines,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(lines_of(read("0")), 1);
        assert_eq!(lines_of(read("100000")), MAX_READ_LINES);
        assert_eq!(lines_of(read("7")), 7);
        let default = parse_request_line(br#"{"v":1,"op":"read","target":"a"}"#);
        assert_eq!(lines_of(default), DEFAULT_READ_LINES);
    }

    #[test]
    fn parse_send_defaults_enter_false() {
        let req = parse_request_line(br#"{"v":1,"op":"send","target":"a","text":"hi"}"#);
        assert_eq!(
            req,
            Ok(LinkRequest::Send {
                target: SendTarget::panel("a"),
                text: "hi".to_owned(),
                enter: false,
                from: None,
                from_id: None,
            })
        );
    }

    #[test]
    fn parse_send_reads_the_optional_sender_name() {
        let req = parse_request_line(br#"{"v":1,"op":"send","target":"a","text":"hi","from":"backend"}"#);
        assert_eq!(
            req,
            Ok(LinkRequest::Send {
                target: SendTarget::panel("a"),
                text: "hi".to_owned(),
                enter: false,
                from: Some("backend".to_owned()),
                from_id: None,
            })
        );
    }

    #[test]
    fn send_with_a_sender_round_trips_and_without_one_omits_the_field() {
        let with = LinkRequest::Send {
            target: SendTarget::panel("a"),
            text: "x".to_owned(),
            enter: false,
            from: Some("api".to_owned()),
            from_id: None,
        };
        let json = serde_json::to_string(&RequestEnvelope {
            v: LINK_PROTOCOL_VERSION,
            request: with.clone(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"op":"send","target":"a","text":"x","enter":false,"from":"api"}"#
        );
        assert_eq!(parse_request_line(json.as_bytes()), Ok(with));
    }

    #[test]
    fn request_envelope_round_trip() {
        let envelope = RequestEnvelope {
            v: LINK_PROTOCOL_VERSION,
            request: LinkRequest::Send {
                target: SendTarget::panel("a"),
                text: "x\ny".to_owned(),
                enter: true,
                from: None,
                from_id: None,
            },
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(json, r#"{"v":1,"op":"send","target":"a","text":"x\ny","enter":true}"#);
        assert_eq!(parse_request_line(json.as_bytes()), Ok(envelope.request));
    }

    #[test]
    fn response_json_round_trip() {
        let panel = PanelInfo {
            id: "7".to_owned(),
            name: "claude".to_owned(),
            workspace: "w".to_owned(),
            kind: "claude".to_owned(),
            is_agent: true,
        };
        let responses = [
            LinkResponse::Ok(LinkOk::Panels(vec![panel])),
            LinkResponse::Ok(LinkOk::sent_to("7")),
            LinkResponse::Ok(LinkOk::Sent {
                delivered: vec!["1".to_owned(), "2".to_owned()],
                skipped: vec!["3".to_owned()],
            }),
            LinkResponse::Ok(LinkOk::Reply {
                from: "b".to_owned(),
                text: "yes".to_owned(),
            }),
            LinkResponse::Ok(LinkOk::Text("out\nput".to_owned())),
            LinkResponse::Err {
                code: LinkErrorCode::Ambiguous,
                message: "m".to_owned(),
            },
        ];
        for response in responses {
            let json = serde_json::to_string(&response).unwrap();
            let back: LinkResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back, response, "{json}");
        }
        let json = serde_json::to_string(&LinkResponse::Ok(LinkOk::sent_to("7"))).unwrap();
        assert_eq!(
            json,
            r#"{"status":"ok","kind":"sent","data":{"delivered":["7"],"skipped":[]}}"#
        );
    }

    #[test]
    fn parse_ask_clamps_timeout_range() {
        let ask = |extra: &str| {
            let raw = format!(r#"{{"v":1,"op":"ask","from":"1","target":"2","text":"q"{extra}}}"#);
            match parse_request_line(raw.as_bytes()) {
                Ok(LinkRequest::Ask { timeout_secs, .. }) => timeout_secs,
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_eq!(ask(""), ASK_DEFAULT_TIMEOUT_SECS);
        assert_eq!(ask(r#","timeout_secs":0"#), ASK_MIN_TIMEOUT_SECS);
        assert_eq!(ask(r#","timeout_secs":4"#), ASK_MIN_TIMEOUT_SECS);
        assert_eq!(ask(r#","timeout_secs":30"#), 30);
        assert_eq!(ask(r#","timeout_secs":601"#), ASK_MAX_TIMEOUT_SECS);
        assert_eq!(ask(r#","timeout_secs":18446744073709551615"#), ASK_MAX_TIMEOUT_SECS);
    }

    #[test]
    fn parse_ask_and_reply_require_their_fields() {
        for line in [
            &br#"{"v":1,"op":"ask","target":"2","text":"q"}"#[..],
            br#"{"v":1,"op":"ask","from":"1","text":"q"}"#,
            br#"{"v":1,"op":"reply","from":"1","text":"a"}"#,
            br#"{"v":1,"op":"reply","ask_id":"abc123","text":"a"}"#,
        ] {
            let err = parse_request_line(line).unwrap_err();
            assert_eq!(err.code(), LinkErrorCode::BadRequest, "{line:?}");
        }
    }

    #[test]
    fn send_target_wire_is_a_string_or_the_workspace_scope() {
        let all = br#"{"v":1,"op":"send","target":{"scope":"workspace"},"text":"hi","from_id":"4"}"#;
        assert_eq!(
            parse_request_line(all),
            Ok(LinkRequest::Send {
                target: SendTarget::AllInWorkspace,
                text: "hi".to_owned(),
                enter: false,
                from: None,
                from_id: Some("4".to_owned()),
            })
        );
        let json = serde_json::to_string(&SendTarget::AllInWorkspace).unwrap();
        assert_eq!(json, r#"{"scope":"workspace"}"#);
        let bad = br#"{"v":1,"op":"send","target":{"scope":"universe"},"text":"hi"}"#;
        assert_eq!(parse_request_line(bad).unwrap_err().code(), LinkErrorCode::BadRequest);
    }
}
