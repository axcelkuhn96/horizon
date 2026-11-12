//! `~/.horizon/claude-session-accounts.json`: which account ran which session.
//!
//! Claude's transcripts are shared by all accounts, so only Horizon, which knows a panel's
//! profile and session id, can say who ran a session. The file holds no credentials and no
//! transcript content: `{"version":1,"sessions":{"<uuid>":{"profile","config_dir","ts"}}}`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use super::{SessionError, is_session_uuid};
use crate::Waker;

/// Most sessions remembered; the oldest are dropped first.
pub const MAX_INDEX_ENTRIES: usize = 5_000;
const FILE_VERSION: u32 = 1;
const MAX_PROFILE_CHARS: usize = 64;
const MAX_DIR_CHARS: usize = 1_024;
/// A larger file is not ours (or is damaged); it is set aside instead of read.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One remembered session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountEntry {
    pub profile: String,
    pub config_dir: String,
    /// Unix seconds when Horizon saw the pair.
    pub ts: i64,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    #[serde(default)]
    sessions: BTreeMap<String, AccountEntry>,
}

fn entry_is_sane(id: &str, entry: &AccountEntry) -> bool {
    is_session_uuid(id)
        && !entry.profile.is_empty()
        && entry.profile.chars().count() <= MAX_PROFILE_CHARS
        && !entry.profile.chars().any(char::is_control)
        && entry.config_dir.chars().count() <= MAX_DIR_CHARS
        && !entry.config_dir.chars().any(char::is_control)
}

/// Session id to account, in memory, with its file path.
#[derive(Clone, Debug)]
pub struct AccountIndex {
    path: PathBuf,
    entries: BTreeMap<String, AccountEntry>,
}

impl AccountIndex {
    /// An index with no entries that will save to `path`.
    #[must_use]
    pub fn empty(path: PathBuf) -> Self {
        Self {
            path,
            entries: BTreeMap::new(),
        }
    }

    /// Loads `path`. A missing file starts empty; a corrupt (or oversized, or wrong-version)
    /// one is renamed to `<name>.bad` and starts empty. Entries that fail validation are dropped.
    #[must_use]
    pub fn load(path: PathBuf) -> Self {
        let mut index = Self::empty(path);
        match read_disk(&index.path) {
            Disk::Missing => {}
            Disk::Entries(entries) => {
                index.entries = entries;
                index.trim();
            }
            Disk::Unusable => set_aside(&index.path),
        }
        index
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn lookup(&self, session_id: &str) -> Option<&AccountEntry> {
        self.entries.get(session_id)
    }

    /// Remembers that `session_id` ran under `profile`. Returns whether the index changed
    /// (a repeat of what is already known changes nothing, so callers can skip the write).
    /// Ids that are not UUIDs and malformed fields are ignored.
    pub fn record(&mut self, session_id: &str, profile: &str, config_dir: &str, ts: i64) -> bool {
        let entry = AccountEntry {
            profile: profile.to_owned(),
            config_dir: config_dir.to_owned(),
            ts,
        };
        if !entry_is_sane(session_id, &entry) {
            return false;
        }
        if let Some(known) = self.entries.get(session_id)
            && known.profile == entry.profile
            && known.config_dir == entry.config_dir
        {
            return false;
        }
        self.entries.insert(session_id.to_owned(), entry);
        self.trim();
        true
    }

    /// Adds what `other` knows and this index does not, or knows newer.
    pub fn merge(&mut self, other: &Self) {
        for (id, entry) in &other.entries {
            if self.entries.get(id).is_none_or(|known| known.ts < entry.ts) {
                self.entries.insert(id.clone(), entry.clone());
            }
        }
        self.trim();
    }

    fn trim(&mut self) {
        let excess = self.entries.len().saturating_sub(MAX_INDEX_ENTRIES);
        if excess == 0 {
            return;
        }
        let mut by_age: Vec<(i64, String)> = self.entries.iter().map(|(id, e)| (e.ts, id.clone())).collect();
        by_age.sort();
        for (_, id) in by_age.into_iter().take(excess) {
            self.entries.remove(&id);
        }
    }

    /// Folds in what the file holds now (another Horizon instance may have recorded sessions
    /// since this index was loaded), then saves. A file that cannot be read is set aside.
    ///
    /// # Errors
    ///
    /// As [`Self::save_atomic`].
    pub fn save_merged(&mut self) -> Result<(), SessionError> {
        match read_disk(&self.path) {
            Disk::Missing => {}
            Disk::Entries(entries) => {
                let on_disk = Self {
                    path: self.path.clone(),
                    entries,
                };
                self.merge(&on_disk);
            }
            Disk::Unusable => set_aside(&self.path),
        }
        self.save_atomic()
    }

    /// Writes the index next to its final path (mode 0600 on Unix) and renames it into place,
    /// so a reader never sees a partial file.
    ///
    /// # Errors
    ///
    /// Any I/O or serialization failure; the previous file is then left as it was.
    pub fn save_atomic(&self) -> Result<(), SessionError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec(&FileFormat {
            version: FILE_VERSION,
            sessions: self.entries.clone(),
        })?;
        let mut tmp_name = self.path.as_os_str().to_owned();
        tmp_name.push(format!(".tmp{}", std::process::id()));
        let tmp = PathBuf::from(tmp_name);
        let written = write_private(&tmp, &body).and_then(|()| std::fs::rename(&tmp, &self.path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(written?)
    }
}

/// What the index file holds right now.
enum Disk {
    Missing,
    Entries(BTreeMap<String, AccountEntry>),
    /// Corrupt, oversized or of another version.
    Unusable,
}

fn read_disk(path: &Path) -> Disk {
    let Ok(meta) = std::fs::metadata(path) else {
        return Disk::Missing;
    };
    let parsed = if meta.len() > MAX_FILE_BYTES {
        None
    } else {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FileFormat>(&bytes).ok())
            .filter(|file| file.version == FILE_VERSION)
    };
    match parsed {
        Some(file) => Disk::Entries(
            file.sessions
                .into_iter()
                .filter(|(id, entry)| entry_is_sane(id, entry))
                .collect(),
        ),
        None => Disk::Unusable,
    }
}

fn set_aside(path: &Path) {
    let mut bad = path.as_os_str().to_owned();
    bad.push(".bad");
    if let Err(error) = std::fs::rename(path, PathBuf::from(bad)) {
        tracing::warn!("could not set aside a corrupt Claude session index: {error}");
    }
}

fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body)?;
    file.sync_all()
}

enum WriterMessage {
    Record {
        session_id: String,
        profile: String,
        config_dir: String,
        ts: i64,
    },
}

/// Owns the index file on a thread of its own: it loads the file at start (delivered through
/// [`Self::try_loaded`]) and saves after every recorded change, so the UI never touches the disk.
pub struct AccountIndexWriter {
    commands: Sender<WriterMessage>,
    loaded: Mutex<Receiver<AccountIndex>>,
    thread: Option<JoinHandle<()>>,
}

impl AccountIndexWriter {
    /// Starts the writer for `path`; `waker` (if any) is called once the file has been loaded.
    #[must_use]
    pub fn start(path: PathBuf, waker: Option<Waker>) -> Self {
        let (commands, command_rx) = mpsc::channel::<WriterMessage>();
        let (loaded_tx, loaded) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("claude-session-index".into())
            .spawn(move || {
                let mut index = AccountIndex::load(path);
                let _ = loaded_tx.send(index.clone());
                if let Some(waker) = &waker {
                    waker();
                }
                while let Ok(message) = command_rx.recv() {
                    let WriterMessage::Record {
                        session_id,
                        profile,
                        config_dir,
                        ts,
                    } = message;
                    if index.record(&session_id, &profile, &config_dir, ts)
                        && let Err(error) = index.save_merged()
                    {
                        tracing::warn!("could not save the Claude session index: {error}");
                    }
                }
            })
            .map_err(|error| tracing::warn!("failed to spawn the Claude session index writer: {error}"))
            .ok();
        Self {
            commands,
            loaded: Mutex::new(loaded),
            thread,
        }
    }

    /// The index as the file had it, once the thread has read it (at most once).
    #[must_use]
    pub fn try_loaded(&self) -> Option<AccountIndex> {
        self.loaded.lock().ok()?.try_recv().ok()
    }

    /// Queues a record; the thread saves it. Never blocks.
    pub fn record(&self, session_id: &str, profile: &str, config_dir: &str, ts: i64) {
        let _ = self.commands.send(WriterMessage::Record {
            session_id: session_id.to_owned(),
            profile: profile.to_owned(),
            config_dir: config_dir.to_owned(),
            ts,
        });
    }
}

impl Drop for AccountIndexWriter {
    /// Closing the channel ends the thread after it saves what is queued; it is joined only when
    /// already finished, so dropping on the UI thread never waits.
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

    const A: &str = "f054f32b-7840-4c95-9f84-e61f37db5f5c";
    const B: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn index_roundtrip_and_mode_0600() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sub/claude-session-accounts.json");
        let mut index = AccountIndex::empty(path.clone());
        assert!(index.record(A, "conta2", "/home/u/.claude-conta2", 1_731_400_000));

        index.save_atomic().expect("save");
        let loaded = AccountIndex::load(path.clone());

        assert_eq!(loaded.lookup(A), index.lookup(A));
        assert_eq!(loaded.lookup(A).map(|e| e.profile.as_str()), Some("conta2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("\"version\":1"));
    }

    #[test]
    fn index_corrupt_file_starts_empty_and_renames_bad() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        std::fs::write(&path, b"{ not json").expect("write");

        let index = AccountIndex::load(path.clone());

        assert!(index.is_empty());
        assert!(!path.exists());
        assert!(dir.path().join("idx.json.bad").exists());
    }

    #[test]
    fn index_missing_file_starts_empty_without_a_bad_file() {
        let dir = tempfile::tempdir().expect("tempdir");

        let index = AccountIndex::load(dir.path().join("idx.json"));

        assert!(index.is_empty());
        assert!(!dir.path().join("idx.json.bad").exists());
    }

    #[test]
    fn index_drops_invalid_entries_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        let body = format!(
            r#"{{"version":1,"sessions":{{"{A}":{{"profile":"ok","config_dir":"/x","ts":1}},"not-a-uuid":{{"profile":"p","config_dir":"/x","ts":1}},"{B}":{{"profile":"bad\u001bname","config_dir":"/x","ts":1}}}}}}"#
        );
        std::fs::write(&path, body).expect("write");

        let index = AccountIndex::load(path);

        assert_eq!(index.len(), 1);
        assert!(index.lookup(A).is_some());
    }

    #[test]
    fn index_caps_entries_dropping_oldest() {
        let mut index = AccountIndex::empty(PathBuf::from("/nonexistent/idx.json"));
        for n in 0..MAX_INDEX_ENTRIES + 3 {
            let id = format!("00000000-0000-0000-0000-{n:012}");
            let ts = i64::try_from(n).expect("small");
            index.record(&id, "p", "/x", ts);
        }

        assert_eq!(index.len(), MAX_INDEX_ENTRIES);
        assert!(index.lookup("00000000-0000-0000-0000-000000000000").is_none());
        assert!(index.lookup("00000000-0000-0000-0000-000000000003").is_some());
    }

    #[test]
    fn index_atomic_save_leaves_no_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        let mut index = AccountIndex::empty(path);
        index.record(A, "conta1", "/x", 1);

        index.save_atomic().expect("save");

        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["idx.json"]);
    }

    #[test]
    fn record_reports_changes_and_ignores_garbage() {
        let mut index = AccountIndex::empty(PathBuf::from("/nonexistent/idx.json"));

        assert!(index.record(A, "conta1", "/x", 1));
        assert!(!index.record(A, "conta1", "/x", 2), "same pair is not a change");
        assert!(index.record(A, "conta2", "/y", 3), "a different account is");
        assert!(!index.record("../../etc", "conta1", "/x", 1));
        assert!(!index.record(B, "", "/x", 1));
        assert!(!index.record(B, "bad\nname", "/x", 1));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn merge_keeps_the_newer_entry() {
        let mut ours = AccountIndex::empty(PathBuf::from("/a"));
        ours.record(A, "old", "/x", 1);
        let mut theirs = AccountIndex::empty(PathBuf::from("/b"));
        theirs.record(A, "new", "/x", 5);
        theirs.record(B, "other", "/x", 1);

        ours.merge(&theirs);

        assert_eq!(ours.lookup(A).map(|e| e.profile.as_str()), Some("new"));
        assert!(ours.lookup(B).is_some());
    }

    #[test]
    fn save_merged_keeps_sessions_another_instance_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        let mut ours = AccountIndex::empty(path.clone());
        ours.record(A, "conta1", "/x", 1);
        // Another instance saved its own record after we loaded.
        let mut theirs = AccountIndex::empty(path.clone());
        theirs.record(B, "conta2", "/y", 2);
        theirs.save_atomic().expect("their save");

        ours.save_merged().expect("merged save");

        let saved = AccountIndex::load(path);
        assert_eq!(saved.lookup(A).map(|e| e.profile.as_str()), Some("conta1"));
        assert_eq!(saved.lookup(B).map(|e| e.profile.as_str()), Some("conta2"));
        assert!(ours.lookup(B).is_some(), "what was merged in is kept in memory");
    }

    #[test]
    fn save_merged_prefers_our_newer_entry_and_sets_a_corrupt_file_aside() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        let mut theirs = AccountIndex::empty(path.clone());
        theirs.record(A, "old", "/x", 1);
        theirs.save_atomic().expect("their save");
        let mut ours = AccountIndex::empty(path.clone());
        ours.record(A, "new", "/x", 9);

        ours.save_merged().expect("merged save");

        assert_eq!(
            AccountIndex::load(path.clone()).lookup(A).map(|e| e.profile.as_str()),
            Some("new")
        );

        std::fs::write(&path, b"{ not json").expect("corrupt");
        ours.save_merged().expect("save over corrupt");
        assert!(dir.path().join("idx.json.bad").exists());
        assert!(AccountIndex::load(path).lookup(A).is_some());
    }

    #[test]
    fn writer_loads_then_saves_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("idx.json");
        let writer = AccountIndexWriter::start(path.clone(), None);
        let loaded = (0..200).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            writer.try_loaded()
        });
        assert!(loaded.is_some_and(|index| index.is_empty()));

        writer.record(A, "conta1", "/x", 1);
        drop(writer);
        let saved = (0..200).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            let index = AccountIndex::load(path.clone());
            index.lookup(A).map(|e| e.profile.clone())
        });

        assert_eq!(saved.as_deref(), Some("conta1"));
    }
}
