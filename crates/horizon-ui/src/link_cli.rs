//! `horizon link ...`: command-line client for the panel link socket.
//!
//! Runs before any GUI start-up. Talks to the running instance through the unix socket named
//! by `HORIZON_SOCKET`: one JSON request line out, one JSON response line back.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use horizon_core::link::{
    ASK_DEFAULT_TIMEOUT_SECS, ASK_MAX_TIMEOUT_SECS, ASK_MIN_TIMEOUT_SECS, DEFAULT_READ_LINES, LINK_PROTOCOL_VERSION,
    LinkErrorCode, LinkOk, LinkRequest, LinkResponse, MAX_REQUEST_BYTES, PanelInfo, RequestEnvelope, SendTarget,
    strip_escapes,
};

const USAGE: &str = "usage:\n  horizon link list\n  horizon link send <target> <text>... [--no-enter]\n  horizon link send --all-in-workspace <text>... [--no-enter]\n  horizon link ask <target> <question>... [--timeout 120]\n  horizon link reply <ask-id> <answer>...\n  horizon link read <target> [--lines N]";

/// Exit code of a failed `ask` that got no answer in time (other failures exit 1).
const EXIT_ASK_TIMEOUT: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkCommand {
    List,
    Send {
        target: SendTarget,
        text: String,
        enter: bool,
    },
    Read {
        target: String,
        lines: usize,
    },
    Ask {
        target: String,
        text: String,
        timeout_secs: u64,
    },
    Reply {
        ask_id: String,
        text: String,
    },
}

/// Parses the arguments that follow `horizon link`.
pub(crate) fn parse_link_args(args: &[String]) -> Result<LinkCommand, String> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(USAGE.to_owned());
    };
    match subcommand.as_str() {
        "list" if rest.is_empty() => Ok(LinkCommand::List),
        "list" => Err(format!("'list' takes no arguments\n{USAGE}")),
        "send" => parse_send(rest),
        "ask" => parse_ask(rest),
        "reply" => parse_reply(rest),
        "read" => parse_read(rest),
        other => Err(format!("unknown subcommand '{other}'\n{USAGE}")),
    }
}

fn parse_send(args: &[String]) -> Result<LinkCommand, String> {
    let mut enter = true;
    let mut all_in_workspace = false;
    let mut words: Vec<&str> = Vec::new();
    let mut literal = false;
    for arg in args {
        if literal {
            words.push(arg);
        } else if arg == "--" {
            literal = true;
        } else if arg == "--no-enter" {
            enter = false;
        } else if arg == "--all-in-workspace" {
            all_in_workspace = true;
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'\n{USAGE}"));
        } else {
            words.push(arg);
        }
    }
    if all_in_workspace {
        if words.is_empty() {
            return Err(format!("'send --all-in-workspace' needs a text\n{USAGE}"));
        }
        return Ok(LinkCommand::Send {
            target: SendTarget::AllInWorkspace,
            text: words.join(" "),
            enter,
        });
    }
    let Some((target, text)) = words.split_first() else {
        return Err(format!("'send' needs a target and a text\n{USAGE}"));
    };
    if text.is_empty() {
        return Err(format!("'send' needs a text\n{USAGE}"));
    }
    Ok(LinkCommand::Send {
        target: SendTarget::panel(*target),
        text: text.join(" "),
        enter,
    })
}

fn parse_ask(args: &[String]) -> Result<LinkCommand, String> {
    let mut timeout_secs = ASK_DEFAULT_TIMEOUT_SECS;
    let mut words: Vec<&str> = Vec::new();
    let mut literal = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if literal {
            words.push(arg);
        } else if arg == "--" {
            literal = true;
        } else if arg == "--timeout" {
            let value = iter
                .next()
                .ok_or_else(|| format!("'--timeout' needs a number of seconds\n{USAGE}"))?;
            timeout_secs = value
                .parse::<u64>()
                .ok()
                .filter(|secs| (ASK_MIN_TIMEOUT_SECS..=ASK_MAX_TIMEOUT_SECS).contains(secs))
                .ok_or_else(|| {
                    format!("'--timeout' must be between {ASK_MIN_TIMEOUT_SECS} and {ASK_MAX_TIMEOUT_SECS} seconds, got '{value}'")
                })?;
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'\n{USAGE}"));
        } else {
            words.push(arg);
        }
    }
    let Some((target, text)) = words.split_first() else {
        return Err(format!("'ask' needs a target and a question\n{USAGE}"));
    };
    if text.is_empty() {
        return Err(format!("'ask' needs a question\n{USAGE}"));
    }
    Ok(LinkCommand::Ask {
        target: (*target).to_owned(),
        text: text.join(" "),
        timeout_secs,
    })
}

fn parse_reply(args: &[String]) -> Result<LinkCommand, String> {
    let mut words: Vec<&str> = Vec::new();
    let mut literal = false;
    for arg in args {
        if literal {
            words.push(arg);
        } else if arg == "--" {
            literal = true;
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'\n{USAGE}"));
        } else {
            words.push(arg);
        }
    }
    let Some((ask_id, text)) = words.split_first() else {
        return Err(format!("'reply' needs an ask id and an answer\n{USAGE}"));
    };
    if text.is_empty() {
        return Err(format!("'reply' needs an answer\n{USAGE}"));
    }
    if !ask_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("'{ask_id}' is not an ask id (the id in `[ask <id> from ...]`)"));
    }
    Ok(LinkCommand::Reply {
        ask_id: (*ask_id).to_owned(),
        text: text.join(" "),
    })
}

fn parse_read(args: &[String]) -> Result<LinkCommand, String> {
    let mut lines = DEFAULT_READ_LINES;
    let mut target: Option<&String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--lines" {
            let value = iter
                .next()
                .ok_or_else(|| format!("'--lines' needs a number\n{USAGE}"))?;
            lines = value
                .parse::<usize>()
                .ok()
                .filter(|lines| *lines > 0)
                .ok_or_else(|| format!("'--lines' needs a positive number, got '{value}'"))?;
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'\n{USAGE}"));
        } else if target.replace(arg).is_some() {
            return Err(format!("'read' takes a single target\n{USAGE}"));
        }
    }
    let target = target.ok_or_else(|| format!("'read' needs a target\n{USAGE}"))?;
    Ok(LinkCommand::Read {
        target: target.clone(),
        lines,
    })
}

fn socket_path_from(value: Option<OsString>) -> Result<PathBuf, String> {
    value
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HORIZON_SOCKET is not set; run inside a Horizon terminal".to_owned())
}

/// Longest sender name sent with a message.
const MAX_SENDER_CHARS: usize = 64;

/// This panel's name (`HORIZON_PANEL_NAME`) as the sender of a message: escapes and control
/// characters dropped, capped to [`MAX_SENDER_CHARS`]; `None` when unset or empty.
fn sender_name(value: Option<&OsStr>) -> Option<String> {
    let name: String = terminal_safe(&value?.to_string_lossy())
        .trim()
        .chars()
        .take(MAX_SENDER_CHARS)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// This panel's id (`HORIZON_PANEL_ID`): digits, letters and dashes only, capped; `None`
/// when unset or empty.
fn panel_id(value: Option<&OsStr>) -> Option<String> {
    let id: String = value?
        .to_string_lossy()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(MAX_SENDER_CHARS)
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Serializes the command into one newline-terminated request line. `from` names the sending
/// panel (badge label) and travels with `send` only; `panel_id` identifies it for the
/// requests that need to know who is talking (`ask`, `reply`, a workspace broadcast).
fn request_line(command: &LinkCommand, from: Option<&str>, panel_id: Option<&str>) -> Result<String, String> {
    let request = match command {
        LinkCommand::List => LinkRequest::List,
        LinkCommand::Send { target, text, enter } => LinkRequest::Send {
            target: target.clone(),
            text: text.clone(),
            enter: *enter,
            from: from.map(str::to_owned),
            from_id: panel_id.map(str::to_owned),
        },
        LinkCommand::Ask {
            target,
            text,
            timeout_secs,
        } => LinkRequest::Ask {
            from: require_panel_id(panel_id)?,
            target: target.clone(),
            text: text.clone(),
            timeout_secs: *timeout_secs,
        },
        LinkCommand::Reply { ask_id, text } => LinkRequest::Reply {
            from: require_panel_id(panel_id)?,
            ask_id: ask_id.clone(),
            text: text.clone(),
        },
        LinkCommand::Read { target, lines } => LinkRequest::Read {
            target: target.clone(),
            lines: *lines,
        },
    };
    let envelope = RequestEnvelope {
        v: LINK_PROTOCOL_VERSION,
        request,
    };
    let mut line = serde_json::to_string(&envelope).map_err(|error| format!("cannot encode request: {error}"))?;
    if line.len() > MAX_REQUEST_BYTES {
        return Err(format!("message too large (limit is {MAX_REQUEST_BYTES} bytes)"));
    }
    line.push('\n');
    Ok(line)
}

fn require_panel_id(panel_id: Option<&str>) -> Result<String, String> {
    panel_id
        .map(str::to_owned)
        .ok_or_else(|| "HORIZON_PANEL_ID is not set; run inside a Horizon terminal".to_owned())
}

fn terminal_safe(text: &str) -> String {
    strip_escapes(text).chars().filter(|ch| !ch.is_control()).collect()
}

/// Cleans text from another panel or the server before it reaches this terminal: escape
/// sequences and control characters (except `\n` and `\t`) are dropped.
fn output_safe(text: &str) -> String {
    strip_escapes(text)
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .collect()
}

fn format_panels(panels: &[PanelInfo]) -> String {
    let rows: Vec<[String; 5]> = panels
        .iter()
        .map(|panel| {
            [
                terminal_safe(&panel.id),
                terminal_safe(&panel.name),
                terminal_safe(&panel.workspace),
                terminal_safe(&panel.kind),
                (if panel.is_agent { "yes" } else { "no" }).to_owned(),
            ]
        })
        .collect();
    let header = ["ID", "NAME", "WORKSPACE", "KIND", "AGENT"].map(str::to_owned);
    let mut widths = [0usize; 5];
    for row in std::iter::once(&header).chain(rows.iter()) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    std::iter::once(&header)
        .chain(rows.iter())
        .map(|row| {
            let last = row.len() - 1;
            row.iter()
                .zip(widths)
                .enumerate()
                .map(|(index, (cell, width))| {
                    if index == last {
                        cell.clone()
                    } else {
                        format!("{cell:<width$}")
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Turns a server response into the text for stdout, or the message for stderr.
fn render(response: LinkResponse) -> Result<String, String> {
    match response {
        LinkResponse::Ok(LinkOk::Panels(panels)) => Ok(format_panels(&panels)),
        LinkResponse::Ok(LinkOk::Sent { delivered, skipped }) if delivered.len() == 1 && skipped.is_empty() => {
            Ok(String::new())
        }
        LinkResponse::Ok(LinkOk::Sent { delivered, skipped }) => Ok(if skipped.is_empty() {
            format!("sent to {}", delivered.len())
        } else {
            format!("sent to {}, skipped {}", delivered.len(), skipped.len())
        }),
        LinkResponse::Ok(LinkOk::Text(text) | LinkOk::Reply { text, .. }) => Ok(output_safe(&text)),
        LinkResponse::Err { message, .. } => Err(output_safe(&message)),
    }
}

/// Process exit code for a failed response: 2 when an `ask` timed out, 1 otherwise.
fn failure_exit_code(response: &LinkResponse) -> u8 {
    match response {
        LinkResponse::Err {
            code: LinkErrorCode::AskTimeout,
            ..
        } => EXIT_ASK_TIMEOUT,
        _ => 1,
    }
}

/// A failed command: the message for stderr and the exit code.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    message: String,
    exit: u8,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { message, exit: 1 }
    }
}

#[cfg(unix)]
fn exchange(socket: &std::path::Path, line: &str, wait: std::time::Duration) -> Result<LinkResponse, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

    // std has no connect timeout for unix sockets: connect on a helper thread and wait for it.
    let (sender, receiver) = std::sync::mpsc::channel();
    let socket = socket.to_path_buf();
    std::thread::spawn(move || {
        let _ = sender.send(UnixStream::connect(&socket));
    });
    let mut stream = receiver
        .recv_timeout(CONNECT_TIMEOUT)
        .map_err(|_| "timed out connecting to Horizon".to_owned())?
        .map_err(|error| format!("cannot connect to Horizon ({error}); is the instance still running?"))?;
    stream
        .set_read_timeout(Some(wait))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|error| format!("cannot configure connection: {error}"))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| format!("cannot send request: {error}"))?;

    let mut response = Vec::new();
    BufReader::new(stream.take(MAX_RESPONSE_BYTES))
        .read_until(b'\n', &mut response)
        .map_err(|error| format!("no response from Horizon: {error}"))?;
    if response.is_empty() {
        return Err("Horizon closed the connection without a response".to_owned());
    }
    serde_json::from_slice(&response).map_err(|error| format!("invalid response from Horizon: {error}"))
}

#[cfg(not(unix))]
fn exchange(_socket: &std::path::Path, _line: &str, _wait: std::time::Duration) -> Result<LinkResponse, String> {
    Err("link is not supported on this platform".to_owned())
}

/// How long to wait for the answer: a short bound for ordinary commands; an `ask` waits for
/// its own timeout plus room for the server's slack.
fn response_wait(command: &LinkCommand) -> std::time::Duration {
    const ORDINARY: std::time::Duration = std::time::Duration::from_secs(10);
    match command {
        LinkCommand::Ask { timeout_secs, .. } => std::time::Duration::from_secs(*timeout_secs) + ORDINARY,
        _ => ORDINARY,
    }
}

fn execute(args: &[String]) -> Result<String, Failure> {
    if !cfg!(unix) {
        return Err("link is not supported on this platform".to_owned().into());
    }
    let command = parse_link_args(args)?;
    let socket = socket_path_from(std::env::var_os("HORIZON_SOCKET"))?;
    let from = sender_name(std::env::var_os("HORIZON_PANEL_NAME").as_deref());
    let id = panel_id(std::env::var_os("HORIZON_PANEL_ID").as_deref());
    let line = request_line(&command, from.as_deref(), id.as_deref())?;
    let response = exchange(&socket, &line, response_wait(&command))?;
    let exit = failure_exit_code(&response);
    render(response).map_err(|message| Failure { message, exit })
}

/// Entry point for `horizon link <args>` (`args` excludes the program name and `link`).
pub(crate) fn run(args: &[String]) -> ExitCode {
    match execute(args) {
        Ok(output) => {
            if output.is_empty() {
                return ExitCode::SUCCESS;
            }
            let newline = if output.ends_with('\n') { "" } else { "\n" };
            let mut stdout = std::io::stdout().lock();
            match write!(stdout, "{output}{newline}").and_then(|()| stdout.flush()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(failure) => {
            eprintln!("horizon link: {}", failure.message);
            ExitCode::from(failure.exit)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use horizon_core::link::{LinkErrorCode, LinkOk, LinkResponse, PanelInfo, SendTarget};

    use super::{
        EXIT_ASK_TIMEOUT, LinkCommand, failure_exit_code, format_panels, panel_id, parse_link_args, request_line,
        sender_name, socket_path_from,
    };

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn parse_list() {
        assert_eq!(parse_link_args(&args(&["list"])), Ok(LinkCommand::List));
        assert!(parse_link_args(&args(&["list", "extra"])).is_err());
    }

    #[test]
    fn parse_send_with_no_enter() {
        assert_eq!(
            parse_link_args(&args(&["send", "backend", "hello", "there", "--no-enter"])),
            Ok(LinkCommand::Send {
                target: SendTarget::panel("backend"),
                text: "hello there".to_owned(),
                enter: false,
            })
        );
        assert_eq!(
            parse_link_args(&args(&["send", "7", "run the tests"])),
            Ok(LinkCommand::Send {
                target: SendTarget::panel("7"),
                text: "run the tests".to_owned(),
                enter: true,
            })
        );
    }

    #[test]
    fn parse_send_requires_target_and_text() {
        assert!(parse_link_args(&args(&["send"])).is_err());
        assert!(parse_link_args(&args(&["send", "backend"])).is_err());
        assert!(parse_link_args(&args(&["send", "--no-enter", "backend"])).is_err());
    }

    #[test]
    fn parse_send_double_dash_keeps_flag_like_text() {
        assert_eq!(
            parse_link_args(&args(&["send", "a", "--", "--no-enter", "is", "text"])),
            Ok(LinkCommand::Send {
                target: SendTarget::panel("a"),
                text: "--no-enter is text".to_owned(),
                enter: true,
            })
        );
    }

    #[test]
    fn parse_send_double_dash_right_after_target() {
        assert_eq!(
            parse_link_args(&args(&["send", "a", "--", "--fix the bug"])),
            Ok(LinkCommand::Send {
                target: SendTarget::panel("a"),
                text: "--fix the bug".to_owned(),
                enter: true,
            })
        );
        assert!(parse_link_args(&args(&["send", "a", "--fix"])).is_err());
    }

    #[test]
    fn render_strips_escapes_from_text_and_errors() {
        assert_eq!(
            super::render(LinkResponse::Ok(LinkOk::Text(
                "ok\u{1b}]0;pwned\u{7}\n\tline\u{1b}[2J\rx\u{8}y".to_owned()
            ))),
            Ok("ok\n\tlinexy".to_owned())
        );
        let error = super::render(LinkResponse::Err {
            code: horizon_core::link::LinkErrorCode::NotFound,
            message: "bad \u{1b}[31mred\u{1b}[0m\u{9b}2J".to_owned(),
        })
        .expect_err("err");
        assert_eq!(error, "bad red");
    }

    #[test]
    fn parse_read_default_and_explicit_lines() {
        assert_eq!(
            parse_link_args(&args(&["read", "backend"])),
            Ok(LinkCommand::Read {
                target: "backend".to_owned(),
                lines: horizon_core::link::DEFAULT_READ_LINES,
            })
        );
        assert_eq!(
            parse_link_args(&args(&["read", "backend", "--lines", "120"])),
            Ok(LinkCommand::Read {
                target: "backend".to_owned(),
                lines: 120,
            })
        );
        assert!(parse_link_args(&args(&["read", "backend", "--lines"])).is_err());
        assert!(parse_link_args(&args(&["read", "backend", "--lines", "abc"])).is_err());
        assert!(parse_link_args(&args(&["read", "backend", "--lines", "0"])).is_err());
        assert!(parse_link_args(&args(&["read"])).is_err());
    }

    #[test]
    fn parse_rejects_unknown_subcommand() {
        let error = parse_link_args(&args(&["explode"])).expect_err("unknown subcommand");
        assert!(error.contains("explode"), "{error}");
        assert!(parse_link_args(&[]).is_err());
        assert!(parse_link_args(&args(&["read", "a", "--bogus"])).is_err());
    }

    #[test]
    fn missing_socket_env_is_clear_error() {
        let error = socket_path_from(None).expect_err("unset");
        assert_eq!(error, "HORIZON_SOCKET is not set; run inside a Horizon terminal");
        assert!(socket_path_from(Some(OsString::new())).is_err());
        assert_eq!(
            socket_path_from(Some(OsString::from("/tmp/x.sock"))),
            Ok(std::path::PathBuf::from("/tmp/x.sock"))
        );
    }

    #[test]
    fn request_line_is_one_json_line_with_version() {
        let line = request_line(
            &LinkCommand::Send {
                target: SendTarget::panel("a"),
                text: "x\ny".to_owned(),
                enter: true,
            },
            None,
            None,
        )
        .expect("line");
        assert_eq!(
            line,
            "{\"v\":1,\"op\":\"send\",\"target\":\"a\",\"text\":\"x\\ny\",\"enter\":true}\n"
        );
        assert_eq!(
            request_line(&LinkCommand::List, Some("ignored"), Some("3")).expect("line"),
            "{\"v\":1,\"op\":\"list\"}\n"
        );
    }

    #[test]
    fn request_line_carries_the_sender_name_on_send_only() {
        let send = LinkCommand::Send {
            target: SendTarget::panel("a"),
            text: "hi".to_owned(),
            enter: false,
        };
        assert_eq!(
            request_line(&send, Some("backend"), None).expect("line"),
            "{\"v\":1,\"op\":\"send\",\"target\":\"a\",\"text\":\"hi\",\"enter\":false,\"from\":\"backend\"}\n"
        );
        assert_eq!(
            request_line(
                &LinkCommand::Read {
                    target: "a".to_owned(),
                    lines: 5
                },
                Some("backend"),
                Some("3")
            )
            .expect("line"),
            "{\"v\":1,\"op\":\"read\",\"target\":\"a\",\"lines\":5}\n"
        );
    }

    #[test]
    fn sender_name_is_cleaned_capped_and_optional() {
        use std::ffi::OsStr;

        assert_eq!(sender_name(None), None);
        assert_eq!(sender_name(Some(OsStr::new(""))), None);
        assert_eq!(sender_name(Some(OsStr::new("\u{1b}[31m \u{7}"))), None);
        assert_eq!(
            sender_name(Some(OsStr::new(" my\u{1b}[31m panel\n "))),
            Some("my panel".to_owned())
        );
        let long = "x".repeat(200);
        let long = sender_name(Some(OsStr::new(&long))).expect("name");
        assert_eq!(long.chars().count(), super::MAX_SENDER_CHARS);
    }

    #[test]
    fn request_line_rejects_oversized_text() {
        let error = request_line(
            &LinkCommand::Send {
                target: SendTarget::panel("a"),
                text: "x".repeat(horizon_core::link::MAX_REQUEST_BYTES),
                enter: true,
            },
            None,
            None,
        )
        .expect_err("too large");
        assert!(error.contains("too large"), "{error}");
    }

    #[test]
    fn panels_table_is_aligned_and_escape_free() {
        let panels = vec![
            PanelInfo {
                id: "1".to_owned(),
                name: "backend".to_owned(),
                workspace: "api".to_owned(),
                kind: "claude".to_owned(),
                is_agent: true,
            },
            PanelInfo {
                id: "12".to_owned(),
                name: "sh\u{1b}[31mell".to_owned(),
                workspace: "w".to_owned(),
                kind: "shell".to_owned(),
                is_agent: false,
            },
        ];
        let table = format_panels(&panels);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(!table.contains('\u{1b}'));
        assert!(lines[0].starts_with("ID"));
        let column = |line: &str| {
            line.find("NAME")
                .or_else(|| line.find("backend"))
                .or_else(|| line.find("shell"))
        };
        assert_eq!(column(lines[0]), column(lines[1]));
        assert_eq!(column(lines[1]), column(lines[2]));
        assert!(lines[1].ends_with("yes"));
        assert!(lines[2].ends_with("no"));
    }

    #[test]
    fn response_text_and_errors_render() {
        assert_eq!(
            super::render(LinkResponse::Ok(LinkOk::Text("a\nb".to_owned()))),
            Ok("a\nb".to_owned())
        );
        assert_eq!(super::render(LinkResponse::Ok(LinkOk::sent_to("1"))), Ok(String::new()));
        let error = super::render(LinkResponse::Err {
            code: horizon_core::link::LinkErrorCode::NotFound,
            message: "no panel matches 'x'".to_owned(),
        })
        .expect_err("err");
        assert_eq!(error, "no panel matches 'x'");
    }

    #[test]
    fn parse_ask_with_timeout_flag() {
        assert_eq!(
            parse_link_args(&args(&["ask", "backend", "what", "port?"])),
            Ok(LinkCommand::Ask {
                target: "backend".to_owned(),
                text: "what port?".to_owned(),
                timeout_secs: 120,
            })
        );
        assert_eq!(
            parse_link_args(&args(&["ask", "backend", "ok?", "--timeout", "30"])),
            Ok(LinkCommand::Ask {
                target: "backend".to_owned(),
                text: "ok?".to_owned(),
                timeout_secs: 30,
            })
        );
        assert!(parse_link_args(&args(&["ask", "backend"])).is_err());
        assert!(parse_link_args(&args(&["ask"])).is_err());
        assert!(parse_link_args(&args(&["ask", "backend", "q", "--timeout"])).is_err());
        assert!(parse_link_args(&args(&["ask", "backend", "q", "--timeout", "abc"])).is_err());
    }

    #[test]
    fn parse_ask_rejects_timeout_over_600() {
        assert!(parse_link_args(&args(&["ask", "a", "q", "--timeout", "601"])).is_err());
        assert!(parse_link_args(&args(&["ask", "a", "q", "--timeout", "4"])).is_err());
        assert!(parse_link_args(&args(&["ask", "a", "q", "--timeout", "600"])).is_ok());
        assert!(parse_link_args(&args(&["ask", "a", "q", "--timeout", "5"])).is_ok());
    }

    #[test]
    fn parse_reply() {
        assert_eq!(
            parse_link_args(&args(&["reply", "a1b2c3", "port", "is", "8080"])),
            Ok(LinkCommand::Reply {
                ask_id: "a1b2c3".to_owned(),
                text: "port is 8080".to_owned(),
            })
        );
        assert!(parse_link_args(&args(&["reply", "a1b2c3"])).is_err());
        assert!(parse_link_args(&args(&["reply"])).is_err());
        assert!(parse_link_args(&args(&["reply", "not-an-id", "x"])).is_err());
        assert_eq!(
            parse_link_args(&args(&["reply", "a1b2c3", "--", "--no"])),
            Ok(LinkCommand::Reply {
                ask_id: "a1b2c3".to_owned(),
                text: "--no".to_owned(),
            })
        );
    }

    #[test]
    fn parse_send_all_in_workspace() {
        assert_eq!(
            parse_link_args(&args(&["send", "--all-in-workspace", "heads", "up"])),
            Ok(LinkCommand::Send {
                target: SendTarget::AllInWorkspace,
                text: "heads up".to_owned(),
                enter: true,
            })
        );
        assert_eq!(
            parse_link_args(&args(&["send", "heads up", "--no-enter", "--all-in-workspace"])),
            Ok(LinkCommand::Send {
                target: SendTarget::AllInWorkspace,
                text: "heads up".to_owned(),
                enter: false,
            })
        );
        assert!(parse_link_args(&args(&["send", "--all-in-workspace"])).is_err());
    }

    #[test]
    fn ask_reply_and_broadcast_request_lines_carry_the_panel_id() {
        let ask = LinkCommand::Ask {
            target: "b".to_owned(),
            text: "q".to_owned(),
            timeout_secs: 30,
        };
        assert_eq!(
            request_line(&ask, Some("name"), Some("7")).expect("line"),
            "{\"v\":1,\"op\":\"ask\",\"from\":\"7\",\"target\":\"b\",\"text\":\"q\",\"timeout_secs\":30}\n"
        );
        let error = request_line(&ask, Some("name"), None).expect_err("needs a panel id");
        assert!(error.contains("HORIZON_PANEL_ID"), "{error}");
        let reply = LinkCommand::Reply {
            ask_id: "a1b2c3".to_owned(),
            text: "a".to_owned(),
        };
        assert_eq!(
            request_line(&reply, None, Some("9")).expect("line"),
            "{\"v\":1,\"op\":\"reply\",\"from\":\"9\",\"ask_id\":\"a1b2c3\",\"text\":\"a\"}\n"
        );
        let broadcast = LinkCommand::Send {
            target: SendTarget::AllInWorkspace,
            text: "hi".to_owned(),
            enter: true,
        };
        assert_eq!(
            request_line(&broadcast, Some("n"), Some("4")).expect("line"),
            "{\"v\":1,\"op\":\"send\",\"target\":{\"scope\":\"workspace\"},\"text\":\"hi\",\"enter\":true,\"from\":\"n\",\"from_id\":\"4\"}\n"
        );
    }

    #[test]
    fn panel_id_keeps_only_safe_characters() {
        use std::ffi::OsStr;

        assert_eq!(panel_id(None), None);
        assert_eq!(panel_id(Some(OsStr::new(""))), None);
        assert_eq!(panel_id(Some(OsStr::new("a1-b_2"))), Some("a1-b_2".to_owned()));
        assert_eq!(
            panel_id(Some(OsStr::new("a\u{1b}[31m b\"c"))),
            Some("a31mbc".to_owned())
        );
    }

    #[test]
    fn ask_exit_codes_distinguish_timeout() {
        let err = |code| LinkResponse::Err {
            code,
            message: String::new(),
        };
        assert_eq!(failure_exit_code(&err(LinkErrorCode::AskTimeout)), EXIT_ASK_TIMEOUT);
        assert_eq!(failure_exit_code(&err(LinkErrorCode::NotFound)), 1);
        assert_eq!(failure_exit_code(&err(LinkErrorCode::AskTargetClosed)), 1);
        assert_ne!(EXIT_ASK_TIMEOUT, 1);
    }

    #[test]
    fn ask_reply_text_and_broadcast_summary_render() {
        assert_eq!(
            super::render(LinkResponse::Ok(LinkOk::Reply {
                from: "b".to_owned(),
                text: "8080\u{1b}[2J".to_owned(),
            })),
            Ok("8080".to_owned())
        );
        let broadcast = |delivered: usize, skipped: usize| {
            super::render(LinkResponse::Ok(LinkOk::Sent {
                delivered: vec!["x".to_owned(); delivered],
                skipped: vec!["y".to_owned(); skipped],
            }))
        };
        assert_eq!(broadcast(3, 0), Ok("sent to 3".to_owned()));
        assert_eq!(broadcast(3, 1), Ok("sent to 3, skipped 1".to_owned()));
        assert_eq!(broadcast(1, 0), Ok(String::new()));
    }

    #[test]
    fn plugin_install_horizon_link_skill_mentions_ask_and_reply() {
        let skill = include_str!("../../../assets/plugins/claude-code/skills/horizon-link/SKILL.md");
        for needle in ["link ask", "link reply", "--all-in-workspace", "[ask "] {
            assert!(skill.contains(needle), "the horizon-link skill must mention {needle:?}");
        }
    }
}
