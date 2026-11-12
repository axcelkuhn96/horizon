//! What the scanner already read from each transcript, keyed by `(path, mtime, len)`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Head contents of one transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Head {
    pub(super) title: String,
    pub(super) first_prompt: String,
}

struct Entry {
    modified: SystemTime,
    len: u64,
    head: Head,
}

/// Entries kept across scans; the scanner drops what the latest scan did not touch.
#[derive(Default)]
pub(super) struct HeadCache {
    entries: HashMap<PathBuf, Entry>,
    touched: Vec<PathBuf>,
}

impl HeadCache {
    /// The cached head when the file is still the one that was read.
    pub(super) fn get(&mut self, path: &Path, modified: SystemTime, len: u64) -> Option<Head> {
        let entry = self.entries.get(path)?;
        if entry.modified != modified || entry.len != len {
            return None;
        }
        self.touched.push(path.to_path_buf());
        Some(entry.head.clone())
    }

    pub(super) fn put(&mut self, path: PathBuf, modified: SystemTime, len: u64, head: Head) {
        self.touched.push(path.clone());
        self.entries.insert(path, Entry { modified, len, head });
    }

    /// Ends a scan: forgets the files it did not see (deleted, or in a workspace no longer shown).
    pub(super) fn finish_scan(&mut self) {
        let touched: std::collections::HashSet<PathBuf> = self.touched.drain(..).collect();
        self.entries.retain(|path, _| touched.contains(path));
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn hit_needs_same_mtime_and_len() {
        let mut cache = HeadCache::default();
        let path = PathBuf::from("/a.jsonl");
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
        cache.put(path.clone(), at, 10, Head::default());

        assert!(cache.get(&path, at, 10).is_some());
        assert!(cache.get(&path, at, 11).is_none());
        assert!(cache.get(&path, at + Duration::from_secs(1), 10).is_none());
        assert!(cache.get(Path::new("/b.jsonl"), at, 10).is_none());
    }

    #[test]
    fn finish_scan_forgets_untouched_files() {
        let mut cache = HeadCache::default();
        let at = SystemTime::UNIX_EPOCH;
        cache.put(PathBuf::from("/a"), at, 1, Head::default());
        cache.put(PathBuf::from("/b"), at, 1, Head::default());
        cache.finish_scan();
        assert_eq!(cache.len(), 2);

        assert!(cache.get(Path::new("/a"), at, 1).is_some());
        cache.finish_scan();

        assert_eq!(cache.len(), 1);
    }
}
