//! `ask`/`reply` and the workspace broadcast on the board side.
//!
//! The pending-ask table is owned by the UI and passed in: the board writes the question
//! into the asked panel, the table remembers who is waiting.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Instant;

use alacritty_terminal::term::TermMode;

use crate::link::{
    AskEnd, AskTable, LinkEffect, LinkEffectKind, LinkError, LinkOk, LinkRequest, LinkResponse, MAX_REQUEST_BYTES,
    TargetState, ask_message, sanitize_send_text, strip_escapes,
};
use crate::panel::PanelId;
use crate::workspace::WorkspaceId;

use super::super::Board;
use super::{error_response, live_terminal_mode};

/// Panels one broadcast may reach; the rest are reported as skipped.
pub const MAX_BROADCAST_TARGETS: usize = 16;
/// Longest sender name written into an ask header.
const MAX_ASKER_NAME_CHARS: usize = 64;
/// Expired asks failed per frame.
const SWEEP_CAP: usize = 8;

/// Panel title as a single-line label safe to type into another terminal.
fn clean_name(title: &str, fallback: &str) -> String {
    let name: String = strip_escapes(title)
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_ASKER_NAME_CHARS)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        fallback.to_owned()
    } else {
        name.to_owned()
    }
}

/// The fields of an `ask` request.
#[derive(Clone, Copy)]
pub(super) struct AskArgs<'a> {
    pub(super) from: &'a str,
    pub(super) target: &'a str,
    pub(super) question: &'a str,
    pub(super) timeout_secs: u64,
    /// Set when the asking client vanished; the table ends the ask when it sees it.
    pub(super) abandoned: &'a Arc<AtomicBool>,
}

impl Board {
    /// Live terminal panels of `workspace` that run an agent right now (an agent panel, or a
    /// shell with `claude` in it; see [`Board::panel_runs_agent`]).
    #[must_use]
    pub fn agent_panels_in_workspace(&self, workspace: WorkspaceId) -> Vec<PanelId> {
        self.panels
            .iter()
            .filter(|panel| panel.workspace_id == workspace && panel.terminal().is_some())
            .filter(|panel| !panel.child_exited())
            .filter(|panel| Self::panel_runs_agent(panel))
            .map(|panel| panel.id)
            .collect()
    }

    /// Sends `text` to the other agent panels of the sender's workspace (at most
    /// [`MAX_BROADCAST_TARGETS`]); every other terminal panel there is reported skipped.
    pub(super) fn link_broadcast(
        &mut self,
        sender: Option<PanelId>,
        text: &str,
        enter: bool,
    ) -> Result<(LinkOk, LinkEffect), LinkError> {
        if text.trim().is_empty() {
            return Err(LinkError::BadRequest("a broadcast needs text".to_owned()));
        }
        let sender = sender.ok_or_else(|| {
            LinkError::BadRequest("a workspace broadcast must come from a Horizon panel (HORIZON_PANEL_ID)".to_owned())
        })?;
        let workspace = self.panel_workspace_id(sender).ok_or(LinkError::Closed)?;
        let agents = self.agent_panels_in_workspace(workspace);
        let others: Vec<(PanelId, String, bool)> = self
            .panels
            .iter()
            .filter(|panel| panel.workspace_id == workspace && panel.id != sender && panel.terminal().is_some())
            .map(|panel| (panel.id, panel.local_id.clone(), agents.contains(&panel.id)))
            .collect();
        if !others.iter().any(|(_, _, is_agent)| *is_agent) {
            return Err(LinkError::NotFound("another agent panel in this workspace".to_owned()));
        }
        let mut delivered = Vec::new();
        let mut skipped = Vec::new();
        let mut reached = Vec::new();
        for (id, local_id, is_agent) in others {
            if !is_agent || reached.len() >= MAX_BROADCAST_TARGETS || self.link_deliver(id, text, enter).is_err() {
                skipped.push(local_id);
            } else {
                delivered.push(local_id);
                reached.push(id);
            }
        }
        let effect = LinkEffect {
            kind: LinkEffectKind::Broadcast,
            from: Some(sender),
            to: reached,
            skipped: skipped.len(),
        };
        Ok((LinkOk::Sent { delivered, skipped }, effect))
    }

    /// Delivers the question to the asked panel and registers the ask; the answer (or an
    /// error) later goes through `reply`. Nothing is written when any check fails.
    ///
    /// # Errors
    ///
    /// `TooLarge`, `NotFound` (unknown asker or target), `Ambiguous`, `Closed`, and
    /// `BadRequest` for an empty question, an ask to oneself, a non-agent target or a full table.
    pub(super) fn start_ask(
        &mut self,
        asks: &mut AskTable,
        args: &AskArgs<'_>,
        reply: mpsc::Sender<LinkResponse>,
        now: Instant,
    ) -> Result<LinkEffect, LinkError> {
        let AskArgs {
            from,
            target,
            question,
            timeout_secs,
            abandoned,
        } = *args;
        if question.len() > MAX_REQUEST_BYTES {
            return Err(LinkError::TooLarge);
        }
        if question.trim().is_empty() {
            return Err(LinkError::BadRequest("an ask needs a question".to_owned()));
        }
        let asker = self
            .panel_id_by_local_id(from)
            .filter(|id| self.panel(*id).is_some_and(|panel| panel.terminal().is_some()))
            .ok_or_else(|| {
                LinkError::BadRequest("an ask must come from a Horizon panel (HORIZON_PANEL_ID)".to_owned())
            })?;
        let target_id = self.link_target(target)?;
        if target_id == asker {
            return Err(LinkError::BadRequest("a panel cannot ask itself".to_owned()));
        }
        let (asker_name, target_is_agent) = {
            let asker_panel = self.panel(asker).ok_or(LinkError::Closed)?;
            let target_panel = self.panel(target_id).ok_or(LinkError::Closed)?;
            (
                clean_name(&asker_panel.display_title(), &format!("panel {from}")),
                Self::panel_runs_agent(target_panel),
            )
        };
        if !target_is_agent {
            return Err(LinkError::BadRequest(format!(
                "'{target}' is not running an agent, so there is nobody to answer an ask"
            )));
        }
        let bracketed = {
            let panel = self.panel(target_id).ok_or(LinkError::Closed)?;
            live_terminal_mode(panel)?.contains(TermMode::BRACKETED_PASTE)
        };
        let id = asks.insert_watched(asker, target_id, timeout_secs, reply, Some(Arc::clone(abandoned)), now)?;
        let message = ask_message(&id, &asker_name, &sanitize_send_text(question), bracketed);
        if let Err(error) = self.link_deliver(target_id, &message, true) {
            asks.discard(&id);
            return Err(error);
        }
        Ok(LinkEffect {
            kind: LinkEffectKind::Ask,
            from: Some(asker),
            to: vec![target_id],
            skipped: 0,
        })
    }

    /// Hands `text` to whoever asked `ask_id`; only the asked panel may.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown replying panel, `UnknownAsk`, `NotAddressee`, `AskExpired`.
    pub(super) fn answer_ask(
        &mut self,
        asks: &mut AskTable,
        from: &str,
        ask_id: &str,
        text: &str,
    ) -> Result<LinkEffect, LinkError> {
        if text.len() > MAX_REQUEST_BYTES {
            return Err(LinkError::TooLarge);
        }
        let replier = self.panel_id_by_local_id(from).ok_or_else(|| {
            LinkError::BadRequest("a reply must come from a Horizon panel (HORIZON_PANEL_ID)".to_owned())
        })?;
        let name = self
            .panel(replier)
            .map(|panel| clean_name(&panel.display_title(), &format!("panel {from}")))
            .unwrap_or_default();
        let text = sanitize_send_text(&strip_escapes(text));
        let AskEnd { asker, .. } = asks.answer(ask_id, replier, name, text)?;
        Ok(LinkEffect {
            kind: LinkEffectKind::Reply,
            from: Some(replier),
            to: vec![asker],
            skipped: 0,
        })
    }

    /// Fails pending asks whose asker or target is gone (closed panel, exited process) or
    /// whose deadline passed; returns what ended so the UI can drop its markers.
    pub fn resolve_link_asks(&self, asks: &mut AskTable, now: Instant) -> Vec<AskEnd> {
        let asker_alive = |id: PanelId| self.panel(id).is_some_and(|panel| !panel.child_exited());
        // Cheap, runs every frame: only presence and the exited flag.
        let target_state = |id: PanelId| match self.panel(id) {
            Some(panel) if !panel.child_exited() => {
                if panel.kind.is_agent() {
                    TargetState::Agent
                } else {
                    TargetState::Shell
                }
            }
            _ => TargetState::Gone,
        };
        // An asked shell whose `claude` exited is back at a prompt: it can no longer answer.
        // This walks `/proc`; the table throttles it to once per second per target.
        let shell_runs_agent = |id: PanelId| self.panel(id).is_some_and(Self::panel_runs_agent);
        let mut ended = asks.resolve_closed(now, asker_alive, target_state, shell_runs_agent);
        ended.extend(asks.sweep_expired(now, SWEEP_CAP));
        ended
    }

    /// Serves one request from the socket: answers it through `reply` unless it is an ask
    /// that now waits in `asks` (then the answer comes later, from a reply, a timeout or a
    /// closed panel). Returns the panels the request touched.
    pub fn serve_link(
        &mut self,
        asks: &mut AskTable,
        request: LinkRequest,
        reply: &mpsc::Sender<LinkResponse>,
        abandoned: &Arc<AtomicBool>,
        now: Instant,
    ) -> Option<LinkEffect> {
        let (response, effect) = match request {
            LinkRequest::Ask {
                from,
                target,
                text,
                timeout_secs,
            } => match self.start_ask(
                asks,
                &AskArgs {
                    from: &from,
                    target: &target,
                    question: &text,
                    timeout_secs,
                    abandoned,
                },
                reply.clone(),
                now,
            ) {
                Ok(effect) => return Some(effect),
                Err(error) => (error_response(&error), None),
            },
            LinkRequest::Reply { from, ask_id, text } => match self.answer_ask(asks, &from, &ask_id, &text) {
                Ok(effect) => {
                    let asker = effect.to.first().and_then(|id| self.panel(*id));
                    let ok = asker.map_or_else(
                        || LinkOk::Sent {
                            delivered: Vec::new(),
                            skipped: Vec::new(),
                        },
                        |panel| LinkOk::sent_to(&panel.local_id),
                    );
                    (LinkResponse::Ok(ok), Some(effect))
                }
                Err(error) => (error_response(&error), None),
            },
            other => self.handle_link_request_with_effect(other),
        };
        let _ = reply.send(response);
        effect
    }
}
