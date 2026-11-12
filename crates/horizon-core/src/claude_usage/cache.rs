//! On-disk cache of the last usage snapshot (no secrets: `ProfileUsage` carries none).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use super::ProfileUsage;

const MAX_CACHE_BYTES: u64 = 1024 * 1024;

/// Reads the cached snapshot; any problem (absent, oversized, malformed) yields an empty list.
#[must_use]
pub fn load_cache(path: &Path) -> Vec<ProfileUsage> {
    let mut raw = String::new();
    let read = File::open(path).and_then(|file| file.take(MAX_CACHE_BYTES).read_to_string(&mut raw));
    if read.is_err() {
        return Vec::new();
    }
    serde_json::from_str(&raw).unwrap_or_default()
}

/// `<name>.<pid>.tmp` next to `path`, so two Horizon instances never share a temp file.
fn tmp_path(path: &Path) -> io::Result<std::path::PathBuf> {
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| io::Error::other("cache path has no file name"))?
        .to_owned();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    Ok(path.with_file_name(tmp_name))
}

/// Atomically writes the snapshot (temp file in the same dir, then rename); mode 0600 on unix.
///
/// # Errors
/// Any I/O error creating the directory or writing/renaming the file.
pub fn save_cache(path: &Path, usage: &[ProfileUsage]) -> io::Result<()> {
    let json = serde_json::to_vec(usage).map_err(io::Error::other)?;
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = dir {
        fs::create_dir_all(dir)?;
    }
    let tmp = tmp_path(path)?;
    // A stale temp file from this pid could carry looser permissions; start from a fresh one.
    let _ = fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(&json)?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_usage::{UsageState, UsageWindow};

    fn sample(name: &str) -> ProfileUsage {
        ProfileUsage {
            name: name.to_owned(),
            display_name: Some("Ada".to_owned()),
            email: Some("ada@example.com".to_owned()),
            five_hour: UsageWindow {
                percent: Some(37.0),
                resets_at: Some("2026-11-12T18:00:00Z".to_owned()),
            },
            seven_day: UsageWindow::default(),
            severity: Some("warning".to_owned()),
            state: UsageState::Ok,
            fetched_at_ms: Some(1_000),
        }
    }

    #[test]
    fn cache_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("claude-usage-cache.json");
        let usage = vec![sample("a"), sample("b")];
        save_cache(&path, &usage).unwrap();
        assert_eq!(load_cache(&path), usage);
        save_cache(&path, &usage[..1]).unwrap();
        assert_eq!(load_cache(&path), usage[..1].to_vec());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temp file must be renamed away");
    }

    #[test]
    fn temp_file_is_per_process_and_foreign_temp_files_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-usage-cache.json");
        let tmp = tmp_path(&path).unwrap();
        assert_eq!(
            tmp.file_name().unwrap().to_string_lossy(),
            format!("claude-usage-cache.json.{}.tmp", std::process::id())
        );
        let foreign = dir.path().join("claude-usage-cache.json.999999.tmp");
        std::fs::write(&foreign, "other instance mid-write").unwrap();
        save_cache(&path, &[sample("a")]).unwrap();
        assert_eq!(std::fs::read_to_string(&foreign).unwrap(), "other instance mid-write");
    }

    #[test]
    fn load_cache_tolerates_missing_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_cache(&dir.path().join("absent.json")).is_empty());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        assert!(load_cache(&bad).is_empty());
    }
}
