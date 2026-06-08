//! Non-destructive file-copy engine for the explorer.
//!
//! This module implements the filesystem half of "drop / paste files into a
//! directory": it copies dropped or pasted sources INTO a destination
//! directory, never moving or deleting the originals, and never overwriting an
//! existing destination entry. Name collisions are resolved by appending
//! `" (2)"`, `" (3)"`, ... to produce a fresh, unique target path.
//!
//! There is deliberately no UI, clipboard, or drag-drop logic here — only the
//! pure filesystem engine and collision logic, so it can be unit-tested in
//! isolation and reused by later UI tasks.
//!
//! ## Symlink handling
//!
//! Symlinks are **followed** (copied by value), matching [`std::fs::copy`]
//! semantics: a symlinked file is copied as its target's contents. To avoid
//! infinite loops, a symlinked *directory* is reported as a [`CopyError`] and
//! skipped rather than being recursed into. The self-copy guard (copying a
//! directory into its own subtree) is likewise rejected up front.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// A single failed copy, attributable to one source path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyError {
    /// The source path (or inner descendant) that failed to copy.
    pub source: PathBuf,
    /// Human-readable description of what went wrong.
    pub message: String,
}

/// Aggregate outcome of one [`copy_into_dir`] request.
///
/// `copied` lists every destination path that was successfully created (one
/// entry per top-level source; inner files of a recursively-copied directory
/// are not listed individually). `errors` lists every failure encountered.
#[derive(Debug, Default)]
pub struct CopyReport {
    /// Destination paths that were successfully created.
    pub copied: Vec<PathBuf>,
    /// Failures encountered, best-effort (the batch is never aborted early).
    pub errors: Vec<CopyError>,
}

/// Upper bound on the collision suffix searched by [`unique_dest_name`].
///
/// If `name`, `name (2)`, ... `name (CAP)` are all taken, we stop and return
/// the capped candidate rather than looping forever.
const UNIQUE_NAME_CAP: u32 = 10_000;

/// Copy each `src` INTO `dest_dir` (i.e. to `dest_dir/<name>`), non-destructively.
///
/// Semantics:
/// - Copy only — the source is never moved or deleted.
/// - On a name collision the target is resolved via [`unique_dest_name`]; an
///   existing destination entry is never overwritten.
/// - Files are copied with [`std::fs::copy`]; directories are copied
///   recursively, preserving their relative structure.
/// - Best-effort: a failure on one source (or one inner file) records a
///   [`CopyError`] and processing continues with the remaining items.
/// - Pathological inputs are guarded: a non-existent source, a source equal to
///   or inside `dest_dir`, and copying a directory into its own subtree are all
///   rejected with a [`CopyError`] instead of corrupting data or looping.
///
/// Never panics.
#[must_use]
pub fn copy_into_dir(srcs: &[PathBuf], dest_dir: &Path) -> CopyReport {
    let mut report = CopyReport::default();

    // Resolve the destination directory once so the loop/self-copy guards
    // compare canonical paths where possible.
    let dest_canonical = dest_dir.canonicalize().ok();

    for src in srcs {
        copy_one(src, dest_dir, dest_canonical.as_deref(), &mut report);
    }

    report
}

/// Copy a single top-level source into `dest_dir`, appending to `report`.
fn copy_one(src: &Path, dest_dir: &Path, dest_canonical: Option<&Path>, report: &mut CopyReport) {
    let metadata = match std::fs::symlink_metadata(src) {
        Ok(meta) => meta,
        Err(err) => {
            report.errors.push(CopyError {
                source: src.to_path_buf(),
                message: format!("source does not exist or is unreadable: {err}"),
            });
            return;
        }
    };

    // Guard: refuse to copy a source into itself or its own subtree, which for
    // directories would recurse forever and otherwise risks corrupting the
    // source. Compare canonical paths when both resolve.
    if let (Some(dest_real), Ok(src_real)) = (dest_canonical, src.canonicalize())
        && (dest_real == src_real || dest_real.starts_with(&src_real))
    {
        report.errors.push(CopyError {
            source: src.to_path_buf(),
            message: "refusing to copy a path into itself or its own subtree".to_string(),
        });
        return;
    }

    let Some(name) = src.file_name() else {
        report.errors.push(CopyError {
            source: src.to_path_buf(),
            message: "source has no file name component".to_string(),
        });
        return;
    };

    let dest = unique_dest_name(dest_dir, name);

    let file_type = metadata.file_type();
    if file_type.is_dir() {
        match copy_dir_recursive(src, &dest, report) {
            Ok(()) => report.copied.push(dest),
            Err(err) => report.errors.push(CopyError {
                source: src.to_path_buf(),
                message: err,
            }),
        }
    } else {
        // Files and symlinks-to-files: `fs::copy` follows symlinks, copying the
        // target's contents.
        match std::fs::copy(src, &dest) {
            Ok(_) => report.copied.push(dest),
            Err(err) => report.errors.push(CopyError {
                source: src.to_path_buf(),
                message: format!("failed to copy file: {err}"),
            }),
        }
    }
}

/// Recursively copy directory `src` to the (not-yet-existing) path `dest`.
///
/// Creates `dest`, then copies each child. Inner-file failures are recorded in
/// `report` and do not abort the rest of the directory (best-effort). A symlink
/// encountered as a directory entry is followed for files via [`std::fs::copy`];
/// a symlinked *subdirectory* is skipped with an error to avoid loops.
///
/// Returns `Err` only for failures that prevent the directory copy from
/// starting at all (e.g. the top-level `create_dir` or `read_dir` failing).
fn copy_dir_recursive(src: &Path, dest: &Path, report: &mut CopyReport) -> Result<(), String> {
    std::fs::create_dir(dest).map_err(|err| format!("failed to create directory: {err}"))?;

    let entries = std::fs::read_dir(src).map_err(|err| format!("failed to read directory: {err}"))?;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                report.errors.push(CopyError {
                    source: src.to_path_buf(),
                    message: format!("failed to read directory entry: {err}"),
                });
                continue;
            }
        };

        let child_src = entry.path();
        let child_name = entry.file_name();
        let child_dest = dest.join(&child_name);

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(err) => {
                report.errors.push(CopyError {
                    source: child_src,
                    message: format!("failed to read entry type: {err}"),
                });
                continue;
            }
        };

        if file_type.is_symlink() {
            // Determine whether the link points at a directory; follow files,
            // skip directories to avoid infinite loops.
            match std::fs::metadata(&child_src) {
                Ok(target_meta) if target_meta.is_dir() => {
                    report.errors.push(CopyError {
                        source: child_src,
                        message: "skipping symlinked directory to avoid loops".to_string(),
                    });
                }
                Ok(_) => match std::fs::copy(&child_src, &child_dest) {
                    Ok(_) => {}
                    Err(err) => report.errors.push(CopyError {
                        source: child_src,
                        message: format!("failed to copy symlinked file: {err}"),
                    }),
                },
                Err(err) => report.errors.push(CopyError {
                    source: child_src,
                    message: format!("failed to resolve symlink target: {err}"),
                }),
            }
        } else if file_type.is_dir() {
            // Inner directories reuse their exact name (no collision possible —
            // the parent `dest` was just freshly created).
            if let Err(err) = copy_dir_recursive(&child_src, &child_dest, report) {
                report.errors.push(CopyError {
                    source: child_src,
                    message: err,
                });
            }
        } else if let Err(err) = std::fs::copy(&child_src, &child_dest) {
            report.errors.push(CopyError {
                source: child_src,
                message: format!("failed to copy file: {err}"),
            });
        }
    }

    Ok(())
}

/// Return a path inside `dest_dir` that does not yet exist, derived from the
/// desired `file_name`.
///
/// Collision rule: if `dest_dir/<file_name>` is free, it is returned unchanged.
/// Otherwise a `" (N)"` suffix is inserted, starting at `N = 2`:
/// - For names with an extension the suffix goes *before* the extension:
///   `a.txt` -> `a (2).txt`, `a (3).txt`, ...
/// - For extensionless names (including directories) the suffix is appended to
///   the whole name: `data` -> `data (2)`.
/// - Dotfiles like `.env` are treated as extensionless (the leading dot is not
///   an extension), giving `.env (2)`.
///
/// Searches up to [`UNIQUE_NAME_CAP`]; if everything is taken it returns the
/// capped candidate rather than looping forever.
#[must_use]
pub fn unique_dest_name(dest_dir: &Path, file_name: &OsStr) -> PathBuf {
    let candidate = dest_dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let (stem, ext) = split_stem_ext(file_name);

    for n in 2..=UNIQUE_NAME_CAP {
        let mut name = OsString::new();
        name.push(&stem);
        name.push(format!(" ({n})"));
        if let Some(ext) = &ext {
            name.push(".");
            name.push(ext);
        }

        let candidate = dest_dir.join(&name);
        if !candidate.exists() {
            return candidate;
        }
    }

    // Cap reached: fall back to the capped candidate (extremely unlikely).
    let mut name = OsString::new();
    name.push(&stem);
    name.push(format!(" ({UNIQUE_NAME_CAP})"));
    if let Some(ext) = &ext {
        name.push(".");
        name.push(ext);
    }
    dest_dir.join(name)
}

/// Split a file name into `(stem, Some(extension))` or `(name, None)`.
///
/// A leading dot does not start an extension, so `.env` -> (`.env`, None) and
/// `archive.tar.gz` -> (`archive.tar`, `gz`). Extensions are only recognised
/// when there is a non-empty stem before the final dot.
fn split_stem_ext(file_name: &OsStr) -> (OsString, Option<OsString>) {
    let path = Path::new(file_name);
    match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext)) if !stem.is_empty() => (stem.to_os_string(), Some(ext.to_os_string())),
        _ => (file_name.to_os_string(), None),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("read file")
    }

    #[test]
    fn copies_single_file_into_empty_dest() {
        let src_dir = TempDir::new().expect("src tempdir");
        let dest_dir = TempDir::new().expect("dest tempdir");
        let src = src_dir.path().join("a.txt");
        fs::write(&src, "hello").expect("write src");

        let report = copy_into_dir(&[src.clone()], dest_dir.path());

        let expected = dest_dir.path().join("a.txt");
        assert_eq!(report.copied, vec![expected.clone()]);
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
        assert_eq!(read(&expected), "hello");
        // Source still exists.
        assert!(src.exists());
        assert_eq!(read(&src), "hello");
    }

    #[test]
    fn collision_appends_incrementing_suffix() {
        let src_dir = TempDir::new().expect("src tempdir");
        let dest_dir = TempDir::new().expect("dest tempdir");
        let src = src_dir.path().join("a.txt");
        fs::write(&src, "original").expect("write src");

        // First copy -> a.txt
        let r1 = copy_into_dir(&[src.clone()], dest_dir.path());
        assert_eq!(r1.copied, vec![dest_dir.path().join("a.txt")]);

        // Second copy -> a (2).txt
        let r2 = copy_into_dir(&[src.clone()], dest_dir.path());
        assert_eq!(r2.copied, vec![dest_dir.path().join("a (2).txt")]);

        // Third copy -> a (3).txt
        let r3 = copy_into_dir(&[src.clone()], dest_dir.path());
        assert_eq!(r3.copied, vec![dest_dir.path().join("a (3).txt")]);

        // Original untouched.
        assert_eq!(read(&dest_dir.path().join("a.txt")), "original");
        assert_eq!(read(&dest_dir.path().join("a (2).txt")), "original");
        assert_eq!(read(&dest_dir.path().join("a (3).txt")), "original");
    }

    #[test]
    fn unique_dest_name_inserts_before_extension() {
        let dir = TempDir::new().expect("tempdir");
        let name = OsStr::new("a.txt");

        // Free -> unchanged.
        assert_eq!(unique_dest_name(dir.path(), name), dir.path().join("a.txt"));

        // Taken -> a (2).txt
        fs::write(dir.path().join("a.txt"), "").expect("write");
        assert_eq!(unique_dest_name(dir.path(), name), dir.path().join("a (2).txt"));

        // a.txt and a (2).txt taken -> a (3).txt
        fs::write(dir.path().join("a (2).txt"), "").expect("write");
        assert_eq!(unique_dest_name(dir.path(), name), dir.path().join("a (3).txt"));
    }

    #[test]
    fn unique_dest_name_extensionless_appends_suffix() {
        let dir = TempDir::new().expect("tempdir");
        let name = OsStr::new("data");
        fs::create_dir(dir.path().join("data")).expect("mkdir");
        assert_eq!(unique_dest_name(dir.path(), name), dir.path().join("data (2)"));
    }

    #[test]
    fn unique_dest_name_dotfile_treated_as_extensionless() {
        let dir = TempDir::new().expect("tempdir");
        let name = OsStr::new(".env");
        fs::write(dir.path().join(".env"), "").expect("write");
        assert_eq!(unique_dest_name(dir.path(), name), dir.path().join(".env (2)"));
    }

    #[test]
    fn copies_directory_recursively() {
        let src_dir = TempDir::new().expect("src tempdir");
        let dest_dir = TempDir::new().expect("dest tempdir");

        // Build src/tree/{top.txt, sub/inner.txt}
        let tree = src_dir.path().join("tree");
        fs::create_dir(&tree).expect("mkdir tree");
        fs::write(tree.join("top.txt"), "top").expect("write top");
        let sub = tree.join("sub");
        fs::create_dir(&sub).expect("mkdir sub");
        fs::write(sub.join("inner.txt"), "inner").expect("write inner");

        let report = copy_into_dir(&[tree.clone()], dest_dir.path());

        let dest_tree = dest_dir.path().join("tree");
        assert_eq!(report.copied, vec![dest_tree.clone()]);
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);

        // Structure + contents replicated.
        assert_eq!(read(&dest_tree.join("top.txt")), "top");
        assert_eq!(read(&dest_tree.join("sub").join("inner.txt")), "inner");

        // Source intact.
        assert_eq!(read(&tree.join("top.txt")), "top");
        assert_eq!(read(&sub.join("inner.txt")), "inner");
    }

    #[test]
    fn copying_dir_into_its_own_subtree_is_rejected() {
        let root = TempDir::new().expect("tempdir");
        // src is the parent; dest is a subdirectory of src.
        let src = root.path().join("project");
        fs::create_dir(&src).expect("mkdir src");
        fs::write(src.join("file.txt"), "data").expect("write");
        let dest = src.join("nested");
        fs::create_dir(&dest).expect("mkdir dest");

        let report = copy_into_dir(&[src.clone()], &dest);

        assert!(report.copied.is_empty(), "nothing copied: {:?}", report.copied);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].source, src);
        // No infinite loop, source uncorrupted.
        assert_eq!(read(&src.join("file.txt")), "data");
        // dest still empty (the guard fired before any copy).
        assert!(fs::read_dir(&dest).expect("read dest").next().is_none());
    }

    #[test]
    fn src_inside_dest_is_rejected() {
        let dest_dir = TempDir::new().expect("dest tempdir");
        // A source that lives inside the destination directory.
        let src = dest_dir.path().join("already.txt");
        fs::write(&src, "x").expect("write");

        let report = copy_into_dir(&[src.clone()], dest_dir.path());

        // Copying already.txt into its own parent dir collides and would just
        // make a duplicate; this is allowed (not a self-subtree), so assert it
        // behaves non-destructively rather than erroring.
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(report.copied, vec![dest_dir.path().join("already (2).txt")]);
        assert_eq!(read(&src), "x");
    }

    #[test]
    fn nonexistent_src_records_error_but_others_copy() {
        let src_dir = TempDir::new().expect("src tempdir");
        let dest_dir = TempDir::new().expect("dest tempdir");

        let missing = src_dir.path().join("ghost.txt");
        let real = src_dir.path().join("real.txt");
        fs::write(&real, "present").expect("write real");

        let report = copy_into_dir(&[missing.clone(), real.clone()], dest_dir.path());

        // The real file was still copied.
        assert_eq!(report.copied, vec![dest_dir.path().join("real.txt")]);
        assert_eq!(read(&dest_dir.path().join("real.txt")), "present");

        // The missing file is recorded as an error.
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].source, missing);
    }

    #[test]
    fn report_aggregates_copied_and_errors() {
        let src_dir = TempDir::new().expect("src tempdir");
        let dest_dir = TempDir::new().expect("dest tempdir");

        let a = src_dir.path().join("a.txt");
        let b = src_dir.path().join("b.txt");
        let missing = src_dir.path().join("nope.txt");
        fs::write(&a, "A").expect("write a");
        fs::write(&b, "B").expect("write b");

        let report = copy_into_dir(&[a.clone(), missing.clone(), b.clone()], dest_dir.path());

        assert_eq!(
            report.copied,
            vec![dest_dir.path().join("a.txt"), dest_dir.path().join("b.txt")]
        );
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].source, missing);
    }
}
