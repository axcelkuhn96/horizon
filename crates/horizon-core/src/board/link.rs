//! Board side of the panel link: answers `list`/`send`/`read` requests from the socket
//! server. Called by the UI thread only. `ask`/`reply` and the workspace broadcast live in
//! [`ask`].

mod ask;

use std::path::{Path, PathBuf};

use alacritty_terminal::term::TermMode;

use crate::agent_detect::detect_claude_process;
use crate::link::{
    LinkEffect, LinkEffectKind, LinkError, LinkOk, LinkRequest, LinkResponse, MAX_READ_LINES, MAX_REQUEST_BYTES,
    PanelInfo, SendTarget, resolve_target, sanitize_send_text, strip_escapes,
};
use crate::panel::{Panel, PanelId, PanelKind};
use crate::proc_foreground::process_in_foreground;

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
    /// Whether `panel` runs an agent right now: an agent panel (Claude, Codex, ...), or a
    /// shell whose live process tree contains `claude` (started by hand or by a quick-start)
    /// that holds the terminal's foreground. A suspended (`Ctrl-Z`) or backgrounded (`&`)
    /// `claude` does not count: the shell is at its prompt again, and typing `[ask ...]` plus
    /// Enter into it would run the text as a command.
    ///
    /// Inspects the process tree each time (Linux `/proc`; other platforms see shells as
    /// non-agents) and is never cached or inferred from the terminal title, so call it at
    /// the moment of delivery: a stale "yes" would type text and Enter into a bare shell.
    #[must_use]
    pub fn panel_runs_agent(panel: &Panel) -> bool {
        if panel.kind.is_agent() {
            return true;
        }
        panel.kind == PanelKind::Shell
            && !panel.child_exited()
            && panel
                .child_pid()
                .and_then(|pid| detect_claude_process(pid, &[]))
                .is_some_and(|process| process_in_foreground(process.pid))
    }

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
    /// than the one the target resolved to. `ask`/`reply` need the pending-ask table:
    /// they go through [`Board::serve_link`].
    pub fn handle_link_request(&mut self, request: LinkRequest) -> LinkResponse {
        self.handle_link_request_with_effect(request).0
    }

    /// Like [`Self::handle_link_request`], also naming the panels a delivered `send` touched.
    pub fn handle_link_request_with_effect(&mut self, request: LinkRequest) -> (LinkResponse, Option<LinkEffect>) {
        let mut effect = None;
        let result = match request {
            LinkRequest::List => Ok(LinkOk::Panels(self.link_panels(true))),
            LinkRequest::Send {
                target,
                text,
                enter,
                from_id,
                ..
            } => self
                .link_send(&target, &text, enter, from_id.as_deref())
                .map(|(ok, touched)| {
                    effect = Some(touched);
                    ok
                }),
            LinkRequest::Read { target, lines } => self.link_read(&target, lines),
            LinkRequest::Ask { .. } | LinkRequest::Reply { .. } => Err(LinkError::BadRequest(
                "ask and reply are only served together with the pending-ask table".to_owned(),
            )),
        };
        (response_of(result), effect)
    }

    /// Terminal-backed panels only; names are escape-stripped because they may carry
    /// terminal-controlled titles.
    ///
    /// `is_agent` costs a `/proc` walk per shell, so it is only computed for `detect_agents`
    /// (the `list` answer); target resolution does not read it.
    fn link_panels(&self, detect_agents: bool) -> Vec<PanelInfo> {
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
                is_agent: detect_agents && Self::panel_runs_agent(panel),
                session: panel.agent_session_name().map(strip_escapes),
            })
            .collect()
    }

    fn link_target(&self, target: &str) -> Result<PanelId, LinkError> {
        let panels = self.link_panels(false);
        let info = resolve_target(&panels, target)?;
        self.panel_id_by_local_id(&info.id).ok_or(LinkError::Closed)
    }

    fn link_send(
        &mut self,
        target: &SendTarget,
        text: &str,
        enter: bool,
        from_id: Option<&str>,
    ) -> Result<(LinkOk, LinkEffect), LinkError> {
        // The socket server already caps the whole line; this keeps the board safe for any caller.
        if text.len() > MAX_REQUEST_BYTES {
            return Err(LinkError::TooLarge);
        }
        let sender = from_id.and_then(|local_id| self.panel_id_by_local_id(local_id));
        match target {
            SendTarget::Panel(target) => {
                let id = self.link_target(target)?;
                self.link_deliver(id, text, enter)?;
                let local_id = self.panel(id).map(|panel| panel.local_id.clone()).unwrap_or_default();
                let effect = LinkEffect {
                    kind: LinkEffectKind::Send,
                    from: sender,
                    to: vec![id],
                    skipped: 0,
                };
                Ok((LinkOk::sent_to(&local_id), effect))
            }
            SendTarget::AllInWorkspace => self.link_broadcast(sender, text, enter),
        }
    }

    /// Types `text` into `panel` the way `link send` does: sanitised, wrapped in bracketed
    /// paste when the terminal asked for it, followed by Enter only when `enter` is set.
    ///
    /// # Errors
    ///
    /// [`LinkError::TooLarge`] for text over the request cap, [`LinkError::Closed`] when the
    /// panel is gone, has no terminal or its process exited, and [`LinkError::BadRequest`]
    /// for multi-line text that cannot be left unsent (no bracketed paste, no Enter).
    pub fn deliver_text(&mut self, panel: PanelId, text: &str, enter: bool) -> Result<(), LinkError> {
        if text.len() > MAX_REQUEST_BYTES {
            return Err(LinkError::TooLarge);
        }
        self.link_deliver(panel, text, enter)
    }

    /// Types `text` into one panel (bracketed paste when the terminal asked for it), then
    /// Enter if requested.
    fn link_deliver(&mut self, id: PanelId, text: &str, enter: bool) -> Result<(), LinkError> {
        let panel = self.panel_mut(id).ok_or(LinkError::Closed)?;
        let bracketed = live_terminal_mode(panel)?.contains(TermMode::BRACKETED_PASTE);
        let text = sanitize_send_text(text);
        // Without bracketed paste a newline is a typed Enter: the lines would execute, which
        // `--no-enter` / "Send without Enter" promises not to do.
        if !bracketed && !enter && text.contains('\n') {
            return Err(LinkError::BadRequest(
                "this panel has no bracketed paste, so a multi-line message cannot be left unsent; \
                 send it with Enter or as a single line"
                    .to_owned(),
            ));
        }
        if !text.is_empty() {
            panel.write_input(&paste_payload(&text, bracketed));
        }
        if enter {
            panel.write_input(b"\r");
        }
        Ok(())
    }

    /// Whether `panel` has a live terminal that asked for bracketed paste, so multi-line text
    /// reaches it as one paste instead of as typed lines.
    #[must_use]
    pub fn bracketed_paste_enabled(&self, panel: PanelId) -> bool {
        self.panel(panel)
            .is_some_and(|panel| live_terminal_mode(panel).is_ok_and(|mode| mode.contains(TermMode::BRACKETED_PASTE)))
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

fn response_of(result: Result<LinkOk, LinkError>) -> LinkResponse {
    match result {
        Ok(ok) => LinkResponse::Ok(ok),
        Err(error) => error_response(&error),
    }
}

fn error_response(error: &LinkError) -> LinkResponse {
    LinkResponse::Err {
        code: error.code(),
        message: error.to_string(),
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
