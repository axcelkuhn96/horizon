use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const LINK_PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_READ_LINES: usize = 500;
pub const DEFAULT_READ_LINES: usize = 50;

fn default_read_lines() -> usize {
    DEFAULT_READ_LINES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LinkRequest {
    List,
    Send {
        target: String,
        text: String,
        #[serde(default)]
        enter: bool,
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
    Sent,
    Text(String),
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
}

impl LinkError {
    #[must_use]
    pub fn code(&self) -> LinkErrorCode {
        match self {
            Self::BadRequest(_) => LinkErrorCode::BadRequest,
            Self::VersionMismatch { .. } => LinkErrorCode::VersionMismatch,
            Self::TooLarge => LinkErrorCode::TooLarge,
            Self::NotFound(_) => LinkErrorCode::NotFound,
            Self::Ambiguous { .. } => LinkErrorCode::Ambiguous,
            Self::Closed => LinkErrorCode::Closed,
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
                target: "a".to_owned(),
                text: "hi".to_owned(),
                enter: false
            })
        );
    }

    #[test]
    fn request_envelope_round_trip() {
        let envelope = RequestEnvelope {
            v: LINK_PROTOCOL_VERSION,
            request: LinkRequest::Send {
                target: "a".to_owned(),
                text: "x\ny".to_owned(),
                enter: true,
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
            LinkResponse::Ok(LinkOk::Sent),
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
        let json = serde_json::to_string(&LinkResponse::Ok(LinkOk::Sent)).unwrap();
        assert_eq!(json, r#"{"status":"ok","kind":"sent"}"#);
    }
}
