//! Scanning synthetic Claude project directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_mtime};
use horizon_core::claude_sessions::{
    DirectoryScanner, ScanDir, ScanLimits, SessionAccount, SessionOrigin, SessionScanner,
};

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn origin(path: &str, branch: Option<&str>, is_main: bool) -> SessionOrigin {
    SessionOrigin {
        worktree: PathBuf::from(path),
        branch: branch.map(str::to_owned),
        is_main,
        is_agent: false,
    }
}

fn scan_dir(dir: &Path, origin: SessionOrigin) -> ScanDir {
    ScanDir {
        dir: dir.to_path_buf(),
        origin,
        implied_profile: None,
    }
}

fn write_session(dir: &Path, n: u32, prompt: &str, age_secs: u64) -> PathBuf {
    let path = dir.join(format!("{}.jsonl", id(n)));
    let line = serde_json::json!({"type":"user","message":{"role":"user","content":prompt}});
    fs::write(&path, format!("{line}\n")).expect("write transcript");
    touch(&path, age_secs);
    path
}

fn touch(path: &Path, age_secs: u64) {
    let when = SystemTime::now() - Duration::from_secs(age_secs);
    set_file_mtime(path, FileTime::from_system_time(when)).expect("set mtime");
}

#[test]
fn scan_lists_sessions_newest_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_session(dir.path(), 1, "oldest", 300);
    write_session(dir.path(), 2, "newest", 10);
    write_session(dir.path(), 3, "middle", 100);
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (sessions, _) = scanner.scan(&[scan_dir(dir.path(), origin("/p", None, true))]);

    let titles: Vec<_> = sessions.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["newest", "middle", "oldest"]);
    assert_eq!(sessions[0].id, id(2));
    assert_eq!(sessions[0].cwd, PathBuf::from("/p"));
    assert_eq!(sessions[0].account, SessionAccount::Unknown);
}

#[test]
fn scan_reads_only_head_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("{}.jsonl", id(1)));
    let filler = serde_json::json!({"type":"progress","data":"x".repeat(900)}).to_string();
    let late = serde_json::json!({"type":"user","message":{"content":"too late"}}).to_string();
    let mut body = String::new();
    while body.len() < 4_000 {
        body.push_str(&filler);
        body.push('\n');
    }
    body.push_str(&late);
    body.push('\n');
    fs::write(&path, body).expect("write");
    let limits = ScanLimits {
        head_bytes: 2_048,
        ..ScanLimits::default()
    };
    let mut scanner = DirectoryScanner::new(limits);

    let (sessions, _) = scanner.scan(&[scan_dir(dir.path(), origin("/p", None, true))]);

    assert_eq!(sessions[0].title, "Untitled session");
    assert_eq!(sessions[0].first_prompt, "");
    assert!(sessions[0].size > 4_000);
}

#[test]
fn scan_cache_hit_when_mtime_and_len_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_session(dir.path(), 1, "a", 50);
    write_session(dir.path(), 2, "b", 40);
    let dirs = [scan_dir(dir.path(), origin("/p", None, true))];
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (_, first) = scanner.scan(&dirs);
    let (second_sessions, second) = scanner.scan(&dirs);

    assert_eq!(first, 2);
    assert_eq!(second, 0, "an unchanged directory reads no transcript head");
    assert_eq!(second_sessions.len(), 2);
}

#[test]
fn scan_cache_invalidates_on_mtime_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let changed = write_session(dir.path(), 1, "first", 50);
    write_session(dir.path(), 2, "other", 40);
    let dirs = [scan_dir(dir.path(), origin("/p", None, true))];
    let mut scanner = DirectoryScanner::new(ScanLimits::default());
    scanner.scan(&dirs);
    let line = serde_json::json!({"type":"user","message":{"content":"rewritten"}});
    fs::write(&changed, format!("{line}\n")).expect("rewrite");
    touch(&changed, 5);

    let (sessions, read) = scanner.scan(&dirs);

    assert_eq!(read, 1, "only the changed transcript is read again");
    assert_eq!(sessions[0].title, "rewritten");
}

#[test]
fn scan_caps_sessions_per_project_and_in_total() {
    let one = tempfile::tempdir().expect("tempdir");
    let two = tempfile::tempdir().expect("tempdir");
    for n in 0..5 {
        write_session(one.path(), n, "a", 100 + u64::from(n));
        write_session(two.path(), 100 + n, "b", 200 + u64::from(n));
    }
    let limits = ScanLimits {
        max_per_dir: 3,
        max_total: 4,
        ..ScanLimits::default()
    };
    let mut scanner = DirectoryScanner::new(limits);

    let (sessions, _) = scanner.scan(&[
        scan_dir(one.path(), origin("/one", None, true)),
        scan_dir(two.path(), origin("/two", None, false)),
    ]);

    assert_eq!(sessions.len(), 4);
    let from_one = sessions.iter().filter(|s| s.cwd == Path::new("/one")).count();
    assert_eq!(
        from_one, 3,
        "newest three of the first dir, then the newest of the second"
    );
}

#[test]
fn scan_ignores_non_uuid_names_other_extensions_and_symlinks() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_session(dir.path(), 1, "real", 10);
    fs::write(dir.path().join("notes.jsonl"), "{}\n").expect("write");
    fs::write(dir.path().join("session-1.jsonl"), "{}\n").expect("write");
    fs::write(dir.path().join(format!("{}.txt", id(2))), "{}\n").expect("write");
    #[cfg(unix)]
    {
        let secret = dir.path().join("secret");
        fs::write(&secret, "{\"type\":\"user\",\"message\":{\"content\":\"leak\"}}\n").expect("write");
        std::os::unix::fs::symlink(&secret, dir.path().join(format!("{}.jsonl", id(3)))).expect("symlink");
    }
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (sessions, _) = scanner.scan(&[scan_dir(dir.path(), origin("/p", None, true))]);

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id(1));
}

#[test]
fn aggregate_sorted_newest_first_across_dirs() {
    let main = tempfile::tempdir().expect("tempdir");
    let wt = tempfile::tempdir().expect("tempdir");
    write_session(main.path(), 1, "main old", 500);
    write_session(wt.path(), 2, "wt new", 5);
    write_session(main.path(), 3, "main mid", 50);
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (sessions, _) = scanner.scan(&[
        scan_dir(main.path(), origin("/main", Some("main"), true)),
        scan_dir(wt.path(), origin("/wt", Some("feat/x"), false)),
    ]);

    let titles: Vec<_> = sessions.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["wt new", "main mid", "main old"]);
    assert_eq!(sessions[0].origin.branch.as_deref(), Some("feat/x"));
    assert!(!sessions[0].origin.is_main);
}

#[test]
fn implied_profile_marks_sessions_of_a_private_projects_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_session(dir.path(), 1, "x", 5);
    let mut scan = scan_dir(dir.path(), origin("/p", None, true));
    scan.implied_profile = Some("conta3".to_owned());
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (sessions, _) = scanner.scan(&[scan]);

    assert_eq!(sessions[0].account, SessionAccount::Profile("conta3".to_owned()));
}

#[test]
fn missing_directory_yields_nothing() {
    let mut scanner = DirectoryScanner::new(ScanLimits::default());

    let (sessions, read) = scanner.scan(&[scan_dir(Path::new("/definitely/not/here"), origin("/p", None, true))]);

    assert!(sessions.is_empty());
    assert_eq!(read, 0);
}

#[test]
fn background_scanner_answers_the_latest_request() {
    use horizon_core::claude_sessions::{AccountIndex, ScanRequest};

    let dir = tempfile::tempdir().expect("tempdir");
    write_session(dir.path(), 1, "hello", 5);
    let scanner = SessionScanner::start(ScanLimits::default(), None);
    let mut index = AccountIndex::empty(dir.path().join("idx.json"));
    index.record(&id(1), "conta2", "/home/u/.claude-conta2", 1);

    scanner.request(ScanRequest {
        generation: 7,
        dirs: vec![scan_dir(dir.path(), origin("/p", None, true))],
        index,
        profiles: Vec::new(),
    });
    let result = (0..300).find_map(|_| {
        std::thread::sleep(Duration::from_millis(10));
        scanner.try_recv()
    });

    let result = result.expect("a result");
    assert_eq!(result.generation, 7);
    assert!(!result.failed);
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(result.sessions[0].account, SessionAccount::Profile("conta2".to_owned()));
}

#[test]
fn scan_reports_how_many_transcripts_exist_beyond_the_caps() {
    let dir = tempfile::tempdir().expect("tempdir");
    for n in 0..6 {
        write_session(dir.path(), n, "a", 100 + u64::from(n));
    }
    let limits = ScanLimits {
        max_per_dir: 2,
        ..ScanLimits::default()
    };
    let mut scanner = DirectoryScanner::new(limits);

    let (sessions, _) = scanner.scan(&[scan_dir(dir.path(), origin("/p", None, true))]);

    assert_eq!(sessions.len(), 2);
    assert_eq!(scanner.total_found(), 6);
}
