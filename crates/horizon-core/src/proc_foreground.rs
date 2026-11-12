//! Foreground-job probes over `/proc/<pid>/stat` (Linux only; other platforms answer `false`).
//!
//! A terminal has one foreground process group (`tpgid`). A shell that waits at its prompt owns
//! it; a job it started takes it over; a job it suspended or sent to the background does not
//! hold it.

/// `(pgrp, tpgid)` out of a `/proc/<pid>/stat` line: the process's own group and the
/// foreground group of its controlling terminal (`-1`/`0` without one).
#[cfg(any(target_os = "linux", test))]
fn group_ids(stat: &str) -> Option<(i64, i64)> {
    // `comm` sits in parentheses and may itself contain spaces and parentheses.
    let (_, after_comm) = stat.rsplit_once(')')?;
    let mut fields = after_comm.split_whitespace();
    // state ppid pgrp session tty_nr tpgid
    let (pgrp, tpgid) = (fields.nth(2)?, fields.nth(2)?);
    Some((pgrp.parse().ok()?, tpgid.parse().ok()?))
}

/// `true` when `stat` (of a shell) says a job other than the shell owns the terminal: the
/// terminal's foreground group (`tpgid`) differs from the shell's own group (`pgrp`).
/// Shells without a controlling terminal (`tpgid` -1 or 0) never count as busy.
#[cfg(any(target_os = "linux", test))]
#[must_use]
pub(crate) fn foreground_job_running(stat: &str) -> bool {
    group_ids(stat).is_some_and(|(pgrp, tpgid)| tpgid > 0 && tpgid != pgrp)
}

/// `true` when `stat` (of any process) says its own group is its terminal's foreground group,
/// i.e. it is not suspended or backgrounded behind another job.
#[cfg(any(target_os = "linux", test))]
#[must_use]
pub(crate) fn in_foreground_group(stat: &str) -> bool {
    group_ids(stat).is_some_and(|(pgrp, tpgid)| tpgid > 0 && tpgid == pgrp)
}

/// Busy probe of the shell behind a panel's PTY child: a job other than the shell owns the
/// terminal. Linux only; elsewhere it never reports busy.
#[cfg(target_os = "linux")]
#[must_use]
pub fn shell_is_busy(child_pid: i32) -> bool {
    let Ok(pid) = u32::try_from(child_pid) else {
        return false;
    };
    // With transcript capture the PTY child is a `script` wrapper; its child is the shell.
    let shell = crate::terminal::shell_pid_behind_wrapper(pid);
    std::fs::read_to_string(format!("/proc/{shell}/stat")).is_ok_and(|stat| foreground_job_running(&stat))
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn shell_is_busy(_child_pid: i32) -> bool {
    false
}

/// `true` when process `pid` holds the foreground of its terminal (a suspended or `&`-ed job
/// does not). Linux only; elsewhere `false`.
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn process_in_foreground(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| in_foreground_group(&stat))
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub(crate) fn process_in_foreground(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_shell_owns_its_terminal() {
        // pgrp 100 is also the terminal's foreground group.
        assert!(!foreground_job_running("100 (bash) S 99 100 100 34816 100 4194304 1 2"));
    }

    #[test]
    fn a_foreground_job_in_another_group_makes_the_shell_busy() {
        assert!(foreground_job_running("100 (bash) S 99 100 100 34816 4321 4194304 1 2"));
    }

    #[test]
    fn no_controlling_terminal_is_never_busy() {
        assert!(!foreground_job_running("100 (bash) S 99 100 100 0 -1 4194304 1 2"));
    }

    #[test]
    fn comm_with_spaces_and_parentheses_does_not_shift_the_fields() {
        assert!(foreground_job_running("7 (my (odd) sh) S 1 7 7 34816 99 0"));
        assert!(!foreground_job_running("7 (my (odd) sh) S 1 7 7 34816 7 0"));
    }

    #[test]
    fn malformed_stat_is_not_busy() {
        assert!(!foreground_job_running(""));
        assert!(!foreground_job_running("100 (bash) S 99"));
        assert!(!foreground_job_running("100 (bash) S 99 x y z w"));
    }

    #[test]
    fn a_process_owning_the_terminal_foreground_is_in_foreground() {
        assert!(in_foreground_group(
            "4321 (claude) S 100 4321 100 34816 4321 4194304 1 2"
        ));
    }

    #[test]
    fn a_suspended_or_backgrounded_process_is_not_in_foreground() {
        // The shell (group 100) owns the terminal; claude sits in group 4321.
        assert!(!in_foreground_group(
            "4321 (claude) T 100 4321 100 34816 100 4194304 1 2"
        ));
        assert!(!in_foreground_group(
            "4321 (claude) S 100 4321 100 34816 100 4194304 1 2"
        ));
    }

    #[test]
    fn a_process_without_a_terminal_or_with_a_bad_stat_is_not_in_foreground() {
        assert!(!in_foreground_group("4321 (claude) S 100 4321 100 0 -1 4194304 1 2"));
        assert!(!in_foreground_group(""));
        assert!(!in_foreground_group("4321 (claude) S 100"));
    }
}
