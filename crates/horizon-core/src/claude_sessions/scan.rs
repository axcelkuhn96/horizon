//! Listing transcripts and reading just their heads, on a worker thread.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::SystemTime;

use serde_json::Value;

use super::cache::{Head, HeadCache};
use super::sanitize_text;
use super::{AccountIndex, SessionAccount, SessionOrigin, SessionSummary, attribute_accounts, is_session_uuid};
use crate::Waker;

const TRANSCRIPT_EXTENSION: &str = ".jsonl";
const MAX_TITLE_CHARS: usize = 120;
const MAX_PROMPT_CHARS: usize = 200;
const UNTITLED: &str = "Untitled session";
/// Cached heads kept at most; a scan over more files starts the cache afresh.
const MAX_CACHED_HEADS: usize = 4_096;

/// Bounds of a scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanLimits {
    /// Bytes read from the start of each transcript.
    pub head_bytes: usize,
    /// Sessions kept per transcript directory (newest first).
    pub max_per_dir: usize,
    /// Sessions kept in total.
    pub max_total: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            head_bytes: 64 * 1024,
            max_per_dir: 200,
            max_total: 1_000,
        }
    }
}

/// One transcript directory to list.
#[derive(Clone, Debug)]
pub struct ScanDir {
    pub dir: PathBuf,
    pub origin: SessionOrigin,
    /// Profile that certainly ran every session of this directory (a profile whose projects
    /// directory is its own, not the shared one).
    pub implied_profile: Option<String>,
}

/// A session id seen live under a profile's config dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredAccount {
    pub session_id: String,
    pub profile: String,
    pub config_dir: PathBuf,
}

/// What to scan.
#[derive(Clone, Debug)]
pub struct ScanRequest {
    /// Echoed in the result so the caller can drop answers to superseded requests.
    pub generation: u64,
    pub dirs: Vec<ScanDir>,
    /// Known accounts, applied to the sessions found.
    pub index: AccountIndex,
    /// `(profile name, expanded config dir)`: their live-session files tell who runs what now.
    pub profiles: Vec<(String, PathBuf)>,
}

/// The answer to a [`ScanRequest`].
#[derive(Clone, Debug)]
pub struct ScanResult {
    pub generation: u64,
    /// Newest first.
    pub sessions: Vec<SessionSummary>,
    /// Transcripts whose head was read from disk (the others came from the cache).
    pub heads_read: usize,
    pub discovered: Vec<DiscoveredAccount>,
    /// The scan could not run (the worker panicked).
    pub failed: bool,
}

struct Candidate<'a> {
    scan_dir: &'a ScanDir,
    path: PathBuf,
    id: String,
    modified: SystemTime,
    len: u64,
}

/// Lists directories and reads transcript heads, remembering what it read.
pub struct DirectoryScanner {
    limits: ScanLimits,
    cache: HeadCache,
}

impl DirectoryScanner {
    #[must_use]
    pub fn new(limits: ScanLimits) -> Self {
        Self {
            limits,
            cache: HeadCache::default(),
        }
    }

    /// Sessions of `dirs`, newest first, and how many heads were read from disk. A directory
    /// that cannot be listed contributes nothing.
    pub fn scan(&mut self, dirs: &[ScanDir]) -> (Vec<SessionSummary>, usize) {
        let mut candidates: Vec<Candidate<'_>> = Vec::new();
        for scan_dir in dirs {
            let mut listed = list_transcripts(scan_dir);
            listed.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
            listed.truncate(self.limits.max_per_dir);
            candidates.extend(listed);
        }
        candidates.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
        candidates.truncate(self.limits.max_total);
        if candidates.len() > MAX_CACHED_HEADS {
            self.cache = HeadCache::default();
        }

        let mut heads_read = 0;
        let mut sessions = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let head = if let Some(head) = self.cache.get(&candidate.path, candidate.modified, candidate.len) {
                head
            } else {
                heads_read += 1;
                let head = read_head(&candidate.path, self.limits.head_bytes);
                self.cache
                    .put(candidate.path.clone(), candidate.modified, candidate.len, head.clone());
                head
            };
            let origin = candidate.scan_dir.origin.clone();
            sessions.push(SessionSummary {
                cwd: origin.worktree.clone(),
                origin,
                id: candidate.id,
                title: if head.title.is_empty() {
                    UNTITLED.to_owned()
                } else {
                    head.title
                },
                first_prompt: head.first_prompt,
                updated: candidate.modified,
                size: candidate.len,
                account: candidate
                    .scan_dir
                    .implied_profile
                    .clone()
                    .map_or(SessionAccount::Unknown, SessionAccount::Profile),
            });
        }
        self.cache.finish_scan();
        (sessions, heads_read)
    }
}

fn list_transcripts(scan_dir: &ScanDir) -> Vec<Candidate<'_>> {
    let Ok(entries) = std::fs::read_dir(&scan_dir.dir) else {
        return Vec::new();
    };
    let mut listed = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(TRANSCRIPT_EXTENSION)) else {
            continue;
        };
        if !is_session_uuid(id) {
            continue;
        }
        // `DirEntry::metadata` does not follow symlinks: a link planted among the transcripts
        // (to a credentials file, say) is not a regular file and is never opened.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        listed.push(Candidate {
            scan_dir,
            path: entry.path(),
            id: id.to_ascii_lowercase(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            len: meta.len(),
        });
    }
    listed
}

/// Title and first prompt from the first `head_bytes` of a transcript. A line cut by the limit
/// is ignored; lines that are not JSON (binary, truncated) are skipped.
fn read_head(path: &Path, head_bytes: usize) -> Head {
    let Ok(file) = std::fs::File::open(path) else {
        return Head::default();
    };
    let limit = u64::try_from(head_bytes).unwrap_or(u64::MAX);
    let mut buf = Vec::new();
    if file.take(limit).read_to_end(&mut buf).is_err() {
        return Head::default();
    }
    let cut = buf.len() >= head_bytes;
    parse_head(&buf, cut)
}

fn parse_head(bytes: &[u8], cut: bool) -> Head {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if cut {
        lines.pop();
    }
    let mut summary = None;
    let mut ai_title = None;
    let mut prompt = None;
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("summary") if summary.is_none() => {
                summary = text_field(&value, "summary");
            }
            Some("ai-title") if ai_title.is_none() => {
                ai_title = text_field(&value, "aiTitle");
            }
            Some("user") if prompt.is_none() => {
                prompt = user_prompt(&value);
            }
            _ => {}
        }
        if summary.is_some() && prompt.is_some() {
            break;
        }
    }
    let first_prompt = prompt.unwrap_or_default();
    let title = summary
        .or(ai_title)
        .unwrap_or_else(|| sanitize_text(&first_prompt, MAX_TITLE_CHARS));
    Head { title, first_prompt }
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    let text = sanitize_text(value.get(key)?.as_str()?, MAX_TITLE_CHARS);
    (!text.is_empty()).then_some(text)
}

/// The text of a real user prompt: not a meta line, a compaction summary, a sidechain, a tool
/// result, a slash-command echo or an image placeholder.
fn user_prompt(value: &Value) -> Option<String> {
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("isMeta") || flag("isSidechain") || flag("isCompactSummary") {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    let raw = match content {
        Value::String(text) => text.as_str(),
        Value::Array(blocks) => blocks
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)?,
        _ => return None,
    };
    let trimmed = raw.trim_start();
    if ["<command-", "<local-command-", "<system-reminder", "[Image: source"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return None;
    }
    let text = sanitize_text(trimmed, MAX_PROMPT_CHARS);
    (!text.is_empty()).then_some(text)
}

/// Sessions that run right now under each profile, from `<config dir>/sessions/<pid>.json`.
#[must_use]
pub fn discover_live_accounts(profiles: &[(String, PathBuf)]) -> Vec<DiscoveredAccount> {
    let mut found = Vec::new();
    for (profile, config_dir) in profiles {
        let mut ids: Vec<String> = crate::live_claude_session_ids(config_dir).into_iter().collect();
        ids.sort();
        found.extend(
            ids.into_iter()
                .filter(|id| is_session_uuid(id))
                .map(|session_id| DiscoveredAccount {
                    session_id: session_id.to_ascii_lowercase(),
                    profile: profile.clone(),
                    config_dir: config_dir.clone(),
                }),
        );
    }
    found
}

fn run_request(scanner: &mut DirectoryScanner, request: &ScanRequest) -> ScanResult {
    let (mut sessions, heads_read) = scanner.scan(&request.dirs);
    attribute_accounts(&mut sessions, &request.index);
    ScanResult {
        generation: request.generation,
        sessions,
        heads_read,
        discovered: discover_live_accounts(&request.profiles),
        failed: false,
    }
}

/// Background scanner: one thread, requests in, results out. When requests pile up only the
/// newest is scanned.
pub struct SessionScanner {
    requests: Sender<ScanRequest>,
    results: Mutex<Receiver<ScanResult>>,
    thread: Option<JoinHandle<()>>,
}

impl SessionScanner {
    /// Starts the thread; `waker` is called after each result so an idle UI repaints.
    #[must_use]
    pub fn start(limits: ScanLimits, waker: Option<Waker>) -> Self {
        let (requests, request_rx) = mpsc::channel::<ScanRequest>();
        let (result_tx, results) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("claude-session-scan".into())
            .spawn(move || {
                let mut scanner = DirectoryScanner::new(limits);
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }
                    let generation = request.generation;
                    // A panic still answers, so a modal never waits for a result that cannot come.
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_request(&mut scanner, &request)))
                            .unwrap_or_else(|_| ScanResult {
                                generation,
                                sessions: Vec::new(),
                                heads_read: 0,
                                discovered: Vec::new(),
                                failed: true,
                            });
                    if result_tx.send(result).is_err() {
                        break;
                    }
                    if let Some(waker) = &waker {
                        waker();
                    }
                }
            })
            .map_err(|error| tracing::warn!("failed to spawn the Claude session scanner: {error}"))
            .ok();
        Self {
            requests,
            results: Mutex::new(results),
            thread,
        }
    }

    pub fn request(&self, request: ScanRequest) {
        let _ = self.requests.send(request);
    }

    /// The oldest undelivered result, if any.
    #[must_use]
    pub fn try_recv(&self) -> Option<ScanResult> {
        self.results.lock().ok()?.try_recv().ok()
    }
}

impl Drop for SessionScanner {
    /// Never blocks the caller (usually the UI thread): closing the channel ends the worker,
    /// which is joined only when it has already finished.
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> String {
        serde_json::json!({"type":"user","message":{"role":"user","content":text}}).to_string()
    }

    fn head_of(lines: &[String]) -> Head {
        parse_head(lines.join("\n").as_bytes(), false)
    }

    #[test]
    fn title_prefers_summary_then_ai_title_then_first_prompt() {
        let summary = serde_json::json!({"type":"summary","summary":"Fix login redirect"}).to_string();
        let ai = serde_json::json!({"type":"ai-title","aiTitle":"AI named"}).to_string();

        assert_eq!(
            head_of(&[user("hello there"), summary.clone(), ai.clone()]).title,
            "Fix login redirect"
        );
        assert_eq!(head_of(&[user("hello there"), ai]).title, "AI named");
        let plain = head_of(&[user("hello   there\nfriend")]);
        assert_eq!(plain.title, "hello there friend");
        assert_eq!(plain.first_prompt, "hello there friend");
        assert_eq!(head_of(&[]).title, "");
    }

    #[test]
    fn skips_meta_command_and_tool_result_lines() {
        let meta = serde_json::json!({"type":"user","isMeta":true,"message":{"content":"meta text"}}).to_string();
        let tool =
            serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}).to_string();
        let cmd = user("<command-name>/clear</command-name>");
        let local = user("<local-command-stdout>ok</local-command-stdout>");
        let image = serde_json::json!({"type":"user","message":{"content":[{"type":"text","text":"[Image: source: /tmp/x.png]"}]}}).to_string();
        let compact = serde_json::json!({"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}).to_string();
        let real = serde_json::json!({"type":"user","message":{"content":[{"type":"image","source":{}},{"type":"text","text":"the real ask"}]}}).to_string();

        let head = head_of(&[meta, tool, cmd, local, image, compact, real]);

        assert_eq!(head.first_prompt, "the real ask");
    }

    #[test]
    fn tolerates_truncated_binary_and_garbage_lines() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00, b'\n']);
        bytes.extend_from_slice(b"{\"type\":\"user\",\"message\":{\"content\":\"ok prompt\"}}\n");
        bytes.extend_from_slice(b"{\"type\":\"user\",\"message\":{\"conte");

        assert_eq!(parse_head(&bytes, false).first_prompt, "ok prompt");
        // With the read cut by the limit, the half line is dropped, not parsed.
        assert_eq!(parse_head(&bytes, true).first_prompt, "ok prompt");
        assert_eq!(parse_head(b"\x00\x01\x02", false), Head::default());
    }

    #[test]
    fn prompt_strips_control_characters() {
        let head = head_of(&[user("run \u{1b}[2J this\u{7}")]);

        assert_eq!(head.first_prompt, "run [2J this");
    }
}
