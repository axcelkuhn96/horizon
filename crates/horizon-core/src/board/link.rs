//! Board side of the panel link: answers `list`/`send`/`read` requests from the socket
//! server. Called by the UI thread only.

use std::path::{Path, PathBuf};

use alacritty_terminal::term::TermMode;

use crate::link::{
    LinkError, LinkOk, LinkRequest, LinkResponse, MAX_READ_LINES, MAX_REQUEST_BYTES, PanelInfo, resolve_target,
    sanitize_send_text, strip_escapes,
};
use crate::panel::{Panel, PanelId, PanelKind};

use super::Board;

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

const fn kind_label(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::Shell => "shell",
        PanelKind::Ssh => "ssh",
        PanelKind::Codex => "codex",
        PanelKind::Claude => "claude",
        PanelKind::OpenCode => "open_code",
        PanelKind::Gemini => "gemini",
        PanelKind::KiloCode => "kilo_code",
        PanelKind::Pi => "pi",
        PanelKind::Command => "command",
        PanelKind::Editor => "editor",
        PanelKind::GitChanges => "git_changes",
        PanelKind::FileExplorer => "file_explorer",
        PanelKind::Usage => "usage",
    }
}

/// Text as the PTY should receive it: wrapped in bracketed-paste markers when the
/// terminal asked for them (so embedded newlines do not execute), else newlines become
/// carriage returns like a typed Enter.
fn paste_payload(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut bytes = Vec::with_capacity(text.len() + BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len());
        bytes.extend_from_slice(BRACKETED_PASTE_START);
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(BRACKETED_PASTE_END);
        bytes
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

impl Board {
    /// Stores the panel-link socket exposed to newly spawned and restarted panels as
    /// `HORIZON_SOCKET`.
    pub fn set_link_socket(&mut self, path: Option<PathBuf>) {
        self.link_socket = path;
    }

    #[must_use]
    pub fn link_socket(&self) -> Option<&Path> {
        self.link_socket.as_deref()
    }

    /// Executes one panel-link request. Never panics and never writes to a panel other
    /// than the one the target resolved to.
    pub fn handle_link_request(&mut self, request: LinkRequest) -> LinkResponse {
        let result = match request {
            LinkRequest::List => Ok(LinkOk::Panels(self.link_panels())),
            LinkRequest::Send { target, text, enter } => self.link_send(&target, &text, enter),
            LinkRequest::Read { target, lines } => self.link_read(&target, lines),
        };
        match result {
            Ok(ok) => LinkResponse::Ok(ok),
            Err(error) => LinkResponse::Err {
                code: error.code(),
                message: error.to_string(),
            },
        }
    }

    /// Terminal-backed panels only; names are escape-stripped because they may carry
    /// terminal-controlled titles.
    fn link_panels(&self) -> Vec<PanelInfo> {
        self.panels
            .iter()
            .filter(|panel| panel.terminal().is_some())
            .map(|panel| PanelInfo {
                id: panel.local_id.clone(),
                name: strip_escapes(&panel.title),
                workspace: self
                    .workspace(panel.workspace_id)
                    .map(|workspace| strip_escapes(&workspace.name))
                    .unwrap_or_default(),
                kind: kind_label(panel.kind).to_owned(),
                is_agent: panel.kind.is_agent(),
            })
            .collect()
    }

    fn link_target(&self, target: &str) -> Result<PanelId, LinkError> {
        let panels = self.link_panels();
        let info = resolve_target(&panels, target)?;
        self.panel_id_by_local_id(&info.id).ok_or(LinkError::Closed)
    }

    fn link_send(&mut self, target: &str, text: &str, enter: bool) -> Result<LinkOk, LinkError> {
        // The socket server already caps the whole line; this keeps the board safe for any caller.
        if text.len() > MAX_REQUEST_BYTES {
            return Err(LinkError::TooLarge);
        }
        let id = self.link_target(target)?;
        let panel = self.panel_mut(id).ok_or(LinkError::Closed)?;
        let bracketed = live_terminal_mode(panel)?.contains(TermMode::BRACKETED_PASTE);
        let text = sanitize_send_text(text);
        if !text.is_empty() {
            panel.write_input(&paste_payload(&text, bracketed));
        }
        if enter {
            panel.write_input(b"\r");
        }
        Ok(LinkOk::Sent)
    }

    fn link_read(&self, target: &str, lines: usize) -> Result<LinkOk, LinkError> {
        let id = self.link_target(target)?;
        let terminal = self.panel(id).and_then(Panel::terminal).ok_or(LinkError::Closed)?;
        let lines = lines.clamp(1, MAX_READ_LINES);
        let (all, _) = terminal.full_text_lines(lines + usize::from(terminal.rows()));
        let start = all.len().saturating_sub(lines);
        let text = all[start..]
            .iter()
            .map(|line| strip_escapes(line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(LinkOk::Text(text))
    }
}

/// Mode of the panel's terminal, or `Closed` when it has none or its process exited.
fn live_terminal_mode(panel: &Panel) -> Result<TermMode, LinkError> {
    match panel.terminal() {
        Some(terminal) if !terminal.child_exited() => Ok(terminal.mode()),
        _ => Err(LinkError::Closed),
    }
}

#[cfg(test)]
mod tests;
