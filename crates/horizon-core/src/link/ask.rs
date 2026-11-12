//! Pending `ask` requests: the table the UI owns while a connection thread waits.
//!
//! Connection threads only block on a channel; every table access happens on the UI
//! thread, which answers or fails each ask through the stored sender.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{LinkError, LinkOk, LinkResponse};
use crate::panel::PanelId;

/// Pending asks one panel may have open at once.
pub const MAX_ASKS_PER_ASKER: usize = 8;
/// Pending asks across all panels.
pub const MAX_ASKS_TOTAL: usize = 32;
/// Resolved ask ids remembered so a late reply says "expired" rather than "unknown".
const EXPIRED_MEMORY: usize = 64;
const ID_LEN: usize = 6;

/// What a link request did to the panels, for the badges and the visual link line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkEffectKind {
    Send,
    /// A `send` to every agent of the sender's workspace.
    Broadcast,
    Ask,
    Reply,
}

/// Panels touched by a handled request. `from` is `None` when the sender is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEffect {
    pub kind: LinkEffectKind,
    pub from: Option<PanelId>,
    pub to: Vec<PanelId>,
    /// Panels a broadcast left out (not an agent, closed, over the cap).
    pub skipped: usize,
}

/// An ask that just left the table (answered, failed or swept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskEnd {
    pub asker: PanelId,
    pub target: PanelId,
}

struct PendingAsk {
    id: String,
    asker: PanelId,
    target: PanelId,
    deadline: Instant,
    timeout_secs: u64,
    reply: mpsc::Sender<LinkResponse>,
}

impl PendingAsk {
    fn end(&self) -> AskEnd {
        AskEnd {
            asker: self.asker,
            target: self.target,
        }
    }
}

#[derive(Default)]
pub struct AskTable {
    asks: Vec<PendingAsk>,
    expired: VecDeque<String>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..ID_LEN].to_owned()
}

impl AskTable {
    #[must_use]
    pub fn len(&self) -> usize {
        self.asks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.asks.is_empty()
    }

    /// Registers an ask and returns its id (6 hex characters).
    ///
    /// # Errors
    ///
    /// `BadRequest` when the asker or the table is at its cap.
    pub fn insert(
        &mut self,
        asker: PanelId,
        target: PanelId,
        timeout_secs: u64,
        reply: mpsc::Sender<LinkResponse>,
        now: Instant,
    ) -> Result<String, LinkError> {
        if self.asks.len() >= MAX_ASKS_TOTAL {
            return Err(LinkError::BadRequest(
                "too many pending asks; wait for one to finish".to_owned(),
            ));
        }
        if self.asks.iter().filter(|ask| ask.asker == asker).count() >= MAX_ASKS_PER_ASKER {
            return Err(LinkError::BadRequest(
                "this panel already has too many pending asks".to_owned(),
            ));
        }
        let mut id = new_id();
        while self.knows(&id) {
            id = new_id();
        }
        self.asks.push(PendingAsk {
            id: id.clone(),
            asker,
            target,
            deadline: now + Duration::from_secs(timeout_secs),
            timeout_secs,
            reply,
        });
        Ok(id)
    }

    /// Drops a just-inserted ask whose question could not be delivered; nobody is told.
    pub fn discard(&mut self, id: &str) {
        self.asks.retain(|ask| ask.id != id);
    }

    fn knows(&self, id: &str) -> bool {
        self.asks.iter().any(|ask| ask.id == id) || self.expired.iter().any(|old| old == id)
    }

    fn remember_expired(&mut self, id: String) {
        if self.expired.len() >= EXPIRED_MEMORY {
            self.expired.pop_front();
        }
        self.expired.push_back(id);
    }

    /// Delivers `text` as the answer to ask `id`, written by `replier` (named `from_name`).
    ///
    /// # Errors
    ///
    /// `UnknownAsk`, `AskExpired` (resolved earlier, or its client gave up) or
    /// `NotAddressee` when `replier` is not the asked panel.
    pub fn answer(&mut self, id: &str, replier: PanelId, from_name: String, text: String) -> Result<AskEnd, LinkError> {
        let id = id.trim().to_ascii_lowercase();
        let Some(index) = self.asks.iter().position(|ask| ask.id == id) else {
            return Err(if self.expired.iter().any(|old| *old == id) {
                LinkError::AskExpired(id)
            } else {
                LinkError::UnknownAsk(id)
            });
        };
        if self.asks[index].target != replier {
            return Err(LinkError::NotAddressee(id));
        }
        let ask = self.asks.remove(index);
        let end = ask.end();
        let delivered = ask
            .reply
            .send(LinkResponse::Ok(LinkOk::Reply { from: from_name, text }))
            .is_ok();
        self.remember_expired(ask.id);
        if delivered {
            Ok(end)
        } else {
            Err(LinkError::AskExpired(id))
        }
    }

    fn fail(ask: &PendingAsk, error: &LinkError) {
        let _ = ask.reply.send(LinkResponse::Err {
            code: error.code(),
            message: error.to_string(),
        });
    }

    /// Fails every ask whose asker or target is no longer `alive` (closed panel or exited
    /// process): `AskCancelled` for a gone asker, else `AskTargetClosed`.
    pub fn resolve_closed(&mut self, alive: impl Fn(PanelId) -> bool) -> Vec<AskEnd> {
        let mut ended = Vec::new();
        let mut index = 0;
        while index < self.asks.len() {
            let ask = &self.asks[index];
            let error = if !alive(ask.asker) {
                LinkError::AskCancelled
            } else if !alive(ask.target) {
                LinkError::AskTargetClosed
            } else {
                index += 1;
                continue;
            };
            let ask = self.asks.remove(index);
            Self::fail(&ask, &error);
            ended.push(ask.end());
            self.remember_expired(ask.id);
        }
        ended
    }

    /// Fails up to `cap` asks past their deadline with `AskTimeout`.
    pub fn sweep_expired(&mut self, now: Instant, cap: usize) -> Vec<AskEnd> {
        let mut ended = Vec::new();
        let mut index = 0;
        while index < self.asks.len() && ended.len() < cap {
            if self.asks[index].deadline > now {
                index += 1;
                continue;
            }
            let ask = self.asks.remove(index);
            Self::fail(&ask, &LinkError::AskTimeout(ask.timeout_secs));
            ended.push(ask.end());
            self.remember_expired(ask.id);
        }
        ended
    }

    /// Earliest deadline among the pending asks.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.asks.iter().map(|ask| ask.deadline).min()
    }

    /// `(asker, target)` of every pending ask, oldest first.
    pub fn pairs(&self) -> impl Iterator<Item = (PanelId, PanelId)> + '_ {
        self.asks.iter().map(|ask| (ask.asker, ask.target))
    }

    /// Panel `asker` is currently waiting on, if any (the oldest of its asks).
    #[must_use]
    pub fn waiting_on(&self, asker: PanelId) -> Option<PanelId> {
        self.asks.iter().find(|ask| ask.asker == asker).map(|ask| ask.target)
    }
}

/// Text typed into the asked panel: the question under an `[ask <id> from <name>]` header
/// plus the reply hint. `bracketed` is whether the panel accepts multi-line pastes; when it
/// does not, everything is one line so a newline cannot run as a separate command.
#[must_use]
pub fn ask_message(id: &str, from_name: &str, question: &str, bracketed: bool) -> String {
    let hint = format!("(reply with: horizon link reply {id} \"<answer>\")");
    if bracketed {
        format!("[ask {id} from {from_name}] {question}\n{hint}")
    } else {
        let flat = question.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("[ask {id} from {from_name}] {flat} {hint}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel() -> (mpsc::Sender<LinkResponse>, mpsc::Receiver<LinkResponse>) {
        mpsc::channel()
    }

    fn insert(table: &mut AskTable, asker: u64, target: u64, now: Instant) -> (String, mpsc::Receiver<LinkResponse>) {
        let (sender, receiver) = channel();
        let id = table
            .insert(PanelId(asker), PanelId(target), 30, sender, now)
            .expect("insert");
        (id, receiver)
    }

    #[test]
    fn ids_are_six_hex_characters_and_unique() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (a, _ra) = insert(&mut table, 1, 2, now);
        let (b, _rb) = insert(&mut table, 1, 3, now);
        assert_eq!(a.len(), 6);
        assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn ask_message_header_contains_id_and_reply_hint() {
        let text = ask_message("a1b2c3", "backend", "what port?", true);
        assert!(text.starts_with("[ask a1b2c3 from backend] what port?"), "{text}");
        assert!(
            text.ends_with("(reply with: horizon link reply a1b2c3 \"<answer>\")"),
            "{text}"
        );
        assert!(text.contains('\n'));
    }

    #[test]
    fn ask_message_is_one_line_without_bracketed_paste() {
        let text = ask_message("a1b2c3", "x", "line one\nline two", false);
        assert!(!text.contains('\n'), "{text}");
        assert!(text.contains("line one line two"));
    }

    #[test]
    fn answer_goes_to_the_asker_and_only_the_addressee_may_give_it() {
        let mut table = AskTable::default();
        let (id, receiver) = insert(&mut table, 1, 2, Instant::now());

        let err = table
            .answer(&id, PanelId(3), "c".to_owned(), "no".to_owned())
            .unwrap_err();
        assert_eq!(err, LinkError::NotAddressee(id.clone()));
        assert_eq!(table.len(), 1, "a wrong replier must not consume the ask");
        assert!(receiver.try_recv().is_err());

        let end = table
            .answer(&id.to_uppercase(), PanelId(2), "b".to_owned(), "yes".to_owned())
            .expect("addressee answers");
        assert_eq!(
            end,
            AskEnd {
                asker: PanelId(1),
                target: PanelId(2)
            }
        );
        assert_eq!(
            receiver.try_recv().ok(),
            Some(LinkResponse::Ok(LinkOk::Reply {
                from: "b".to_owned(),
                text: "yes".to_owned()
            }))
        );
        assert!(table.is_empty());
    }

    #[test]
    fn reply_unknown_or_expired_ask_errors() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let err = table
            .answer("ffffff", PanelId(2), String::new(), String::new())
            .unwrap_err();
        assert_eq!(err, LinkError::UnknownAsk("ffffff".to_owned()));

        let (id, _receiver) = insert(&mut table, 1, 2, now);
        assert_eq!(table.sweep_expired(now + Duration::from_secs(31), 8).len(), 1);
        let err = table.answer(&id, PanelId(2), String::new(), String::new()).unwrap_err();
        assert_eq!(err, LinkError::AskExpired(id.clone()));

        // answering twice: the second one finds it already resolved
        let (id2, _receiver2) = insert(&mut table, 1, 2, now);
        table
            .answer(&id2, PanelId(2), String::new(), "a".to_owned())
            .expect("first answer");
        let err = table
            .answer(&id2, PanelId(2), String::new(), "b".to_owned())
            .unwrap_err();
        assert_eq!(err, LinkError::AskExpired(id2));
    }

    #[test]
    fn a_reply_to_a_client_that_gave_up_reports_expired() {
        let mut table = AskTable::default();
        let (id, receiver) = insert(&mut table, 1, 2, Instant::now());
        drop(receiver);
        let err = table
            .answer(&id, PanelId(2), String::new(), "late".to_owned())
            .unwrap_err();
        assert_eq!(err, LinkError::AskExpired(id));
    }

    #[test]
    fn ask_table_caps_per_asker_and_total() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let mut keep = Vec::new();
        for _ in 0..MAX_ASKS_PER_ASKER {
            keep.push(insert(&mut table, 1, 2, now));
        }
        let (sender, _receiver) = channel();
        let err = table.insert(PanelId(1), PanelId(2), 30, sender, now).unwrap_err();
        assert_eq!(err.code(), crate::link::LinkErrorCode::BadRequest);

        let mut asker = 10;
        while table.len() < MAX_ASKS_TOTAL {
            for _ in 0..MAX_ASKS_PER_ASKER.min(MAX_ASKS_TOTAL - table.len()) {
                keep.push(insert(&mut table, asker, 2, now));
            }
            asker += 1;
        }
        let (sender, _receiver) = channel();
        let err = table.insert(PanelId(99), PanelId(2), 30, sender, now).unwrap_err();
        assert_eq!(err.code(), crate::link::LinkErrorCode::BadRequest);
        assert_eq!(table.len(), MAX_ASKS_TOTAL);
    }

    #[test]
    fn closing_target_resolves_pending_ask_with_error() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (_, to_target_closed) = insert(&mut table, 1, 2, now);
        let (_, to_asker_closed) = insert(&mut table, 3, 4, now);
        let (_, untouched) = insert(&mut table, 5, 6, now);

        let ended = table.resolve_closed(|panel| panel != PanelId(2) && panel != PanelId(3));

        assert_eq!(ended.len(), 2);
        let code = |receiver: &mpsc::Receiver<LinkResponse>| match receiver.try_recv() {
            Ok(LinkResponse::Err { code, .. }) => Some(code),
            _ => None,
        };
        assert_eq!(
            code(&to_target_closed),
            Some(crate::link::LinkErrorCode::AskTargetClosed)
        );
        assert_eq!(code(&to_asker_closed), Some(crate::link::LinkErrorCode::AskCancelled));
        assert!(untouched.try_recv().is_err());
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn expired_asks_are_swept() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (_, first) = insert(&mut table, 1, 2, now);
        let (_, second) = insert(&mut table, 3, 4, now);
        let (_, third) = insert(&mut table, 5, 6, now);
        assert_eq!(table.next_deadline(), Some(now + Duration::from_secs(30)));

        assert!(table.sweep_expired(now + Duration::from_secs(29), 8).is_empty());
        let ended = table.sweep_expired(now + Duration::from_secs(30), 2);

        assert_eq!(ended.len(), 2, "the sweep honours its per-call cap");
        assert!(matches!(
            first.try_recv(),
            Ok(LinkResponse::Err {
                code: crate::link::LinkErrorCode::AskTimeout,
                ..
            })
        ));
        assert!(second.try_recv().is_ok());
        assert!(third.try_recv().is_err());
        assert_eq!(table.len(), 1);
        assert_eq!(table.sweep_expired(now + Duration::from_secs(31), 8).len(), 1);
        assert!(table.is_empty());
        assert_eq!(table.next_deadline(), None);
    }

    #[test]
    fn waiting_on_reports_the_oldest_target() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (_a, _ra) = insert(&mut table, 1, 2, now);
        let (_b, _rb) = insert(&mut table, 1, 3, now);
        assert_eq!(table.waiting_on(PanelId(1)), Some(PanelId(2)));
        assert_eq!(table.waiting_on(PanelId(2)), None);
        assert_eq!(table.pairs().count(), 2);
    }
}
