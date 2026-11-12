//! `horizon link ...`: command-line client for the panel link socket.
//!
//! Runs before any GUI start-up. Talks to the running instance through the unix socket named
//! by `HORIZON_SOCKET`: one JSON request line out, one JSON response line back.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use horizon_core::link::{
    DEFAULT_READ_LINES, LINK_PROTOCOL_VERSION, LinkOk, LinkRequest, LinkResponse, MAX_REQUEST_BYTES, PanelInfo,
    RequestEnvelope, strip_escapes,
};

const USAGE: &str = "usage:\n  horizon link list\n  horizon link send <target> <text>... [--no-enter]\n  horizon link read <target> [--lines N]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkCommand {
    List,
    Send { target: String, text: String, enter: bool },
    Read { target: String, lines: usize },
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
        "read" => parse_read(rest),
        other => Err(format!("unknown subcommand '{other}'\n{USAGE}")),
    }
}

fn parse_send(args: &[String]) -> Result<LinkCommand, String> {
    let mut enter = true;
    let mut words: Vec<&str> = Vec::new();
    let mut literal = false;
    for arg in args {
        if literal {
            words.push(arg);
        } else if arg == "--" {
            literal = true;
        } else if arg == "--no-enter" {
            enter = false;
        } else if arg.starts_with("--") {
            return Err(format!("unknown option '{arg}'\n{USAGE}"));
        } else {
            words.push(arg);
        }
    }
    let Some((target, text)) = words.split_first() else {
        return Err(format!("'send' needs a target and a text\n{USAGE}"));
    };
    if text.is_empty() {
        return Err(format!("'send' needs a text\n{USAGE}"));
    }
    Ok(LinkCommand::Send {
        target: (*target).to_owned(),
        text: text.join(" "),
        enter,
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

/// Serializes the command into one newline-terminated request line.
fn request_line(command: &LinkCommand) -> Result<String, String> {
    let request = match command {
        LinkCommand::List => LinkRequest::List,
        LinkCommand::Send { target, text, enter } => LinkRequest::Send {
            target: target.clone(),
            text: text.clone(),
            enter: *enter,
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
        LinkResponse::Ok(LinkOk::Sent) => Ok(String::new()),
        LinkResponse::Ok(LinkOk::Text(text)) => Ok(output_safe(&text)),
        LinkResponse::Err { message, .. } => Err(output_safe(&message)),
    }
}

#[cfg(unix)]
fn exchange(socket: &std::path::Path, line: &str) -> Result<LinkResponse, String> {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const IO_TIMEOUT: Duration = Duration::from_secs(10);
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
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
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
fn exchange(_socket: &std::path::Path, _line: &str) -> Result<LinkResponse, String> {
    Err("link is not supported on this platform".to_owned())
}

fn execute(args: &[String]) -> Result<String, String> {
    if !cfg!(unix) {
        return Err("link is not supported on this platform".to_owned());
    }
    let command = parse_link_args(args)?;
    let socket = socket_path_from(std::env::var_os("HORIZON_SOCKET"))?;
    let line = request_line(&command)?;
    render(exchange(&socket, &line)?)
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
        Err(message) => {
            eprintln!("horizon link: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use horizon_core::link::{LinkOk, LinkResponse, PanelInfo};

    use super::{LinkCommand, format_panels, parse_link_args, request_line, socket_path_from};

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
                target: "backend".to_owned(),
                text: "hello there".to_owned(),
                enter: false,
            })
        );
        assert_eq!(
            parse_link_args(&args(&["send", "7", "run the tests"])),
            Ok(LinkCommand::Send {
                target: "7".to_owned(),
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
                target: "a".to_owned(),
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
                target: "a".to_owned(),
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
        let line = request_line(&LinkCommand::Send {
            target: "a".to_owned(),
            text: "x\ny".to_owned(),
            enter: true,
        })
        .expect("line");
        assert_eq!(
            line,
            "{\"v\":1,\"op\":\"send\",\"target\":\"a\",\"text\":\"x\\ny\",\"enter\":true}\n"
        );
        assert_eq!(
            request_line(&LinkCommand::List).expect("line"),
            "{\"v\":1,\"op\":\"list\"}\n"
        );
    }

    #[test]
    fn request_line_rejects_oversized_text() {
        let error = request_line(&LinkCommand::Send {
            target: "a".to_owned(),
            text: "x".repeat(horizon_core::link::MAX_REQUEST_BYTES),
            enter: true,
        })
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
        assert_eq!(super::render(LinkResponse::Ok(LinkOk::Sent)), Ok(String::new()));
        let error = super::render(LinkResponse::Err {
            code: horizon_core::link::LinkErrorCode::NotFound,
            message: "no panel matches 'x'".to_owned(),
        })
        .expect_err("err");
        assert_eq!(error, "no panel matches 'x'");
    }
}
