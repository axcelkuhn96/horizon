//! Pending `ask` requests: the table the UI owns while a connection thread waits.
//!
//! Connection threads only block on a channel; every table access happens on the UI
//! thread, which answers or fails each ask through the stored sender.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
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
/// How long the answer to "does this shell still run an agent?" (a `/proc` walk) is reused.
pub const AGENT_CHECK_EVERY: Duration = Duration::from_secs(1);

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
    /// Set by the connection thread once its client is gone (it stopped waiting).
    abandoned: Option<Arc<AtomicBool>>,
}

impl PendingAsk {
    fn is_abandoned(&self) -> bool {
        self.abandoned.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    fn end(&self) -> AskEnd {
        AskEnd {
            asker: self.asker,
            target: self.target,
        }
    }
}

/// What the board can tell about an ask target without any costly check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// Panel closed or its process exited.
    Gone,
    /// An agent panel: alive as long as its process runs.
    Agent,
    /// A shell: alive only while an agent runs in it, which costs a `/proc` walk.
    Shell,
}

/// Last answer of the costly "shell runs an agent" check per target, so a frame loop does
/// not repeat it every frame.
#[derive(Default)]
struct AgentChecks {
    last: HashMap<PanelId, (Instant, bool)>,
}

impl AgentChecks {
    /// The cached answer for `panel` while younger than [`AGENT_CHECK_EVERY`]; otherwise
    /// runs `probe` and remembers its answer.
    fn check(&mut self, panel: PanelId, now: Instant, probe: impl FnOnce() -> bool) -> bool {
        if let Some(&(at, runs)) = self.last.get(&panel)
            && now.saturating_duration_since(at) < AGENT_CHECK_EVERY
        {
            return runs;
        }
        let runs = probe();
        self.last.insert(panel, (now, runs));
        runs
    }

    /// Whether an ask to `target` can no longer be answered; only a shell costs a probe.
    fn target_closed(
        &mut self,
        target: PanelId,
        now: Instant,
        state: TargetState,
        probe: impl FnOnce() -> bool,
    ) -> bool {
        match state {
            TargetState::Gone => true,
            TargetState::Agent => false,
            TargetState::Shell => !self.check(target, now, probe),
        }
    }

    fn next_check(&self) -> Option<Instant> {
        self.last.values().map(|(at, _)| *at + AGENT_CHECK_EVERY).min()
    }

    fn keep_only(&mut self, keep: impl Fn(PanelId) -> bool) {
        self.last.retain(|panel, _| keep(*panel));
    }
}

#[derive(Default)]
pub struct AskTable {
    asks: Vec<PendingAsk>,
    expired: VecDeque<String>,
    agent_checks: AgentChecks,
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
        self.insert_watched(asker, target, timeout_secs, reply, None, now)
    }

    /// Like [`Self::insert`], also remembering the connection's `abandoned` flag: once it
    /// is set (the client vanished) the next [`Self::resolve_closed`] ends the ask as
    /// `AskCancelled`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::insert`].
    pub fn insert_watched(
        &mut self,
        asker: PanelId,
        target: PanelId,
        timeout_secs: u64,
        reply: mpsc::Sender<LinkResponse>,
        abandoned: Option<Arc<AtomicBool>>,
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
            abandoned,
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

    /// Fails every ask whose client vanished or whose asker is no longer `asker_alive`
    /// (`AskCancelled`), or whose target is gone or no longer runs an agent
    /// (`AskTargetClosed`). `target_state` must be cheap (it runs every frame); for a
    /// shell target `shell_runs_agent` is the costly check, reused for
    /// [`AGENT_CHECK_EVERY`] (see [`Self::next_agent_check`]).
    pub fn resolve_closed(
        &mut self,
        now: Instant,
        asker_alive: impl Fn(PanelId) -> bool,
        target_state: impl Fn(PanelId) -> TargetState,
        shell_runs_agent: impl Fn(PanelId) -> bool,
    ) -> Vec<AskEnd> {
        let mut ended = Vec::new();
        let mut index = 0;
        while index < self.asks.len() {
            let ask = &self.asks[index];
            let target = ask.target;
            let error = if ask.is_abandoned() || !asker_alive(ask.asker) {
                LinkError::AskCancelled
            } else if self
                .agent_checks
                .target_closed(target, now, target_state(target), || shell_runs_agent(target))
            {
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
        let asks = &self.asks;
        self.agent_checks
            .keep_only(|panel| asks.iter().any(|ask| ask.target == panel));
        ended
    }

    /// When the costly check of a shell target is due again; the UI wakes then because
    /// nothing else repaints an idle window.
    #[must_use]
    pub fn next_agent_check(&self) -> Option<Instant> {
        self.agent_checks.next_check()
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
    let hint = format!("(reply with: horizon link reply {id} '<answer>')");
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
            text.ends_with("(reply with: horizon link reply a1b2c3 '<answer>')"),
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

        let alive = |panel: PanelId| panel != PanelId(2) && panel != PanelId(3);
        let ended = table.resolve_closed(
            now,
            alive,
            |panel| {
                if alive(panel) {
                    TargetState::Agent
                } else {
                    TargetState::Gone
                }
            },
            |_| true,
        );

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
    fn an_ask_whose_client_vanished_is_cancelled_and_its_id_remembered() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (sender, receiver) = channel();
        let gone = Arc::new(AtomicBool::new(false));
        let id = table
            .insert_watched(PanelId(1), PanelId(2), 30, sender, Some(Arc::clone(&gone)), now)
            .expect("insert");
        let (_other, other_receiver) = insert(&mut table, 3, 4, now);

        assert!(
            table
                .resolve_closed(Instant::now(), |_| true, |_| TargetState::Agent, |_| true)
                .is_empty(),
            "client still there"
        );
        gone.store(true, Ordering::Release);
        let ended = table.resolve_closed(Instant::now(), |_| true, |_| TargetState::Agent, |_| true);

        assert_eq!(
            ended,
            vec![AskEnd {
                asker: PanelId(1),
                target: PanelId(2)
            }]
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(LinkResponse::Err {
                code: crate::link::LinkErrorCode::AskCancelled,
                ..
            })
        ));
        assert!(other_receiver.try_recv().is_err());
        assert_eq!(table.len(), 1);
        let err = table
            .answer(&id, PanelId(2), String::new(), "late".to_owned())
            .unwrap_err();
        assert_eq!(err, LinkError::AskExpired(id));
    }

    #[test]
    fn a_target_that_no_longer_runs_an_agent_ends_the_ask_even_if_its_panel_lives() {
        let mut table = AskTable::default();
        let (_id, receiver) = insert(&mut table, 1, 2, Instant::now());

        let ended = table.resolve_closed(
            Instant::now(),
            |_| true,
            |_| TargetState::Shell,
            |panel| panel != PanelId(2),
        );

        assert_eq!(ended.len(), 1);
        assert!(matches!(
            receiver.try_recv(),
            Ok(LinkResponse::Err {
                code: crate::link::LinkErrorCode::AskTargetClosed,
                ..
            })
        ));
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

    #[test]
    fn the_shell_agent_check_is_reused_for_a_second_and_then_repeated() {
        use std::cell::Cell;
        let mut table = AskTable::default();
        let start = Instant::now();
        let (_id, receiver) = insert(&mut table, 1, 2, start);
        let probes = Cell::new(0_u32);
        let runs = Cell::new(true);
        let resolve = |table: &mut AskTable, now: Instant| {
            table.resolve_closed(
                now,
                |_| true,
                |_| TargetState::Shell,
                |_| {
                    probes.set(probes.get() + 1);
                    runs.get()
                },
            )
        };

        assert!(resolve(&mut table, start).is_empty());
        assert_eq!(probes.get(), 1);
        assert_eq!(table.next_agent_check(), Some(start + AGENT_CHECK_EVERY));
        // Frames in between trust the cached answer, even if the agent is gone by now.
        runs.set(false);
        for millis in [16, 500, 999] {
            assert!(resolve(&mut table, start + Duration::from_millis(millis)).is_empty());
        }
        assert_eq!(probes.get(), 1);
        // Once the second has passed, the next frame probes again and ends the ask.
        let ended = resolve(&mut table, start + AGENT_CHECK_EVERY);
        assert_eq!(probes.get(), 2);
        assert_eq!(ended.len(), 1);
        assert!(matches!(
            receiver.try_recv(),
            Ok(LinkResponse::Err {
                code: crate::link::LinkErrorCode::AskTargetClosed,
                ..
            })
        ));
    }

    #[test]
    fn agent_targets_never_probe_and_a_gone_target_ends_without_probing() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (_a, _ra) = insert(&mut table, 1, 2, now);
        let (_b, gone) = insert(&mut table, 3, 4, now);
        let state = |panel: PanelId| {
            if panel == PanelId(4) {
                TargetState::Gone
            } else {
                TargetState::Agent
            }
        };

        let ended = table.resolve_closed(now, |_| true, state, |_| panic!("no shell target here"));

        assert_eq!(ended.len(), 1);
        assert!(gone.try_recv().is_ok());
        assert_eq!(
            table.next_agent_check(),
            None,
            "nothing to wake for without a shell target"
        );
    }

    #[test]
    fn cached_checks_are_dropped_once_no_ask_targets_the_shell() {
        let mut table = AskTable::default();
        let now = Instant::now();
        let (id, _receiver) = insert(&mut table, 1, 2, now);
        assert!(
            table
                .resolve_closed(now, |_| true, |_| TargetState::Shell, |_| true)
                .is_empty()
        );
        assert!(table.next_agent_check().is_some());

        table
            .answer(&id, PanelId(2), String::new(), "ok".to_owned())
            .expect("answer");
        assert!(
            table
                .resolve_closed(now, |_| true, |_| TargetState::Shell, |_| true)
                .is_empty()
        );

        assert_eq!(table.next_agent_check(), None);
    }
}
