//! Ephemeral cache directory management.
//!
//! When `corrie tile` runs without `--cache-dir`, the feature store lives
//! in a temp directory tagged with the corrie process's PID. `TempDir`'s
//! `Drop` impl removes the directory on clean scope exit; `cleanup` removes
//! it on Ctrl+C / SIGTERM / SIGHUP. SIGKILL and OOM kills still leave it
//! behind.
//!
//! On startup we scan the system temp dir for sibling `corrie-cache-<pid>-*`
//! entries whose PID is no longer alive, and reap them. PID-based liveness
//! is safe even when a long-running concurrent corrie shares the temp dir.

use anyhow::Context;
use std::path::PathBuf;

const CACHE_PREFIX: &str = "corrie-cache-";

/// Parses a directory name like `corrie-cache-12345-abc6dEF` and returns
/// the PID segment as `u32`. Returns `None` for any name that doesn't
/// match the `corrie-cache-<pid>-<suffix>` shape with a numeric PID.
fn parse_pid_from_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(CACHE_PREFIX)?;
    let (pid_str, _suffix) = rest.split_once('-')?;
    pid_str.parse().ok()
}

/// From a list of `(path, dirname)` candidates, returns paths whose tagged
/// PID is reported dead by `is_alive`. Candidates whose name doesn't parse
/// as `corrie-cache-<pid>-*` are skipped — we can't determine ownership.
fn select_reap_targets<F: Fn(u32) -> bool>(
    candidates: Vec<(PathBuf, String)>,
    is_alive: F,
) -> Vec<PathBuf> {
    candidates
        .into_iter()
        .filter_map(|(path, name)| {
            let pid = parse_pid_from_name(&name)?;
            (!is_alive(pid)).then_some(path)
        })
        .collect()
}

/// Returns true if a process with the given PID is currently alive.
/// Implemented via `kill(pid, 0)`: success means alive; `EPERM` means
/// alive-but-not-signalable (still alive); `ESRCH` means dead.
/// Non-Unix is a no-op that returns true so reaping never fires on
/// platforms we don't support.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: libc::kill is safe to call with any pid and signal; we
    // pass signal 0, which performs an existence/permission check without
    // delivering a signal. The return value and errno are inspected
    // below.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true; // signalable → alive
    }
    // SAFETY: errno is a thread-local int set by libc; reading it via
    // *__error()/*__errno_location() through std::io::Error is the
    // standard portable idiom.
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Scan the system temp dir for `corrie-cache-<pid>-*` directories whose
/// PID is no longer alive and remove them. Returns the number removed.
/// Per-entry failures are logged but don't fail the call.
pub fn reap_orphan_caches() -> usize {
    let tmp = std::env::temp_dir();
    let entries = match std::fs::read_dir(&tmp) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let candidates: Vec<(PathBuf, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?.to_string();
            name.starts_with(CACHE_PREFIX).then_some((path, name))
        })
        .collect();
    let targets = select_reap_targets(candidates, pid_alive);
    let mut reaped = 0;
    for path in &targets {
        match std::fs::remove_dir_all(path) {
            Ok(()) => reaped += 1,
            Err(e) => eprintln!("warn: failed to reap {}: {e}", path.display()),
        }
    }
    if reaped > 0 {
        let s = if reaped == 1 { "" } else { "s" };
        eprintln!("Reaped {reaped} orphan cache dir{s} from prior killed runs");
    }
    reaped
}

/// Create an ephemeral cache directory tagged with this process's PID.
/// The returned `TempDir` removes the directory on clean scope exit;
/// survivors of forced kills are reaped by `reap_orphan_caches` on the
/// next corrie startup.
pub fn create_ephemeral_cache() -> anyhow::Result<tempfile::TempDir> {
    let prefix = format!("{CACHE_PREFIX}{}-", std::process::id());
    let td = tempfile::Builder::new()
        .prefix(&prefix)
        .tempdir()
        .context("create temp cache dir")?;
    eprintln!("Temp cache: {}", td.path().display());
    Ok(td)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pid_extracts_pid_from_tagged_name() {
        assert_eq!(parse_pid_from_name("corrie-cache-12345-abc6dEF"), Some(12345));
    }

    #[test]
    fn parse_pid_rejects_non_corrie_names() {
        assert_eq!(parse_pid_from_name("tmp.XYZ"), None);
        assert_eq!(parse_pid_from_name("corrie-other-12345-foo"), None);
    }

    #[test]
    fn parse_pid_rejects_legacy_unpid_corrie_names() {
        // Pre-PID-tagging temp dirs (created by older corrie binaries) have
        // no PID segment. We can't tell if they're orphaned and must skip.
        assert_eq!(parse_pid_from_name("corrie-cache-abc6dEF"), None);
    }

    #[test]
    fn parse_pid_rejects_non_numeric_pid() {
        assert_eq!(parse_pid_from_name("corrie-cache-notapid-foo"), None);
    }

    #[test]
    fn select_reap_targets_picks_dead_pids_only() {
        use std::path::PathBuf;
        let candidates = vec![
            (PathBuf::from("/tmp/corrie-cache-100-aaa"), "corrie-cache-100-aaa".to_string()),
            (PathBuf::from("/tmp/corrie-cache-200-bbb"), "corrie-cache-200-bbb".to_string()),
            (PathBuf::from("/tmp/corrie-cache-300-ccc"), "corrie-cache-300-ccc".to_string()),
        ];
        let alive: std::collections::HashSet<u32> = [200].into_iter().collect();
        let targets = select_reap_targets(candidates, |pid| alive.contains(&pid));
        assert_eq!(
            targets,
            vec![
                PathBuf::from("/tmp/corrie-cache-100-aaa"),
                PathBuf::from("/tmp/corrie-cache-300-ccc"),
            ]
        );
    }

    #[test]
    fn select_reap_targets_skips_unparseable_names() {
        use std::path::PathBuf;
        // Names without a PID segment (e.g. from a pre-PID-tagging corrie
        // binary) are skipped — we can't tell who owns them.
        let candidates = vec![
            (PathBuf::from("/tmp/corrie-cache-legacy"), "corrie-cache-legacy".to_string()),
            (PathBuf::from("/tmp/corrie-cache-42-x"), "corrie-cache-42-x".to_string()),
        ];
        let targets = select_reap_targets(candidates, |_pid| false);
        assert_eq!(targets, vec![PathBuf::from("/tmp/corrie-cache-42-x")]);
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_reports_self_as_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_handles_permission_denied_as_alive() {
        // PID 1 (init / launchd) exists but a non-root user can't signal
        // it. Liveness check must distinguish "no permission" (EPERM —
        // alive) from "no such process" (ESRCH — dead). If running as
        // root, EPERM doesn't apply, so skip.
        // SAFETY: libc::geteuid is a simple syscall with no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(pid_alive(1), "PID 1 is alive but pid_alive reported it dead");
    }

    #[test]
    fn select_reap_targets_skips_self() {
        // The current process's own PID must never be reaped, even though
        // the predicate would report it alive. Belt-and-braces: the caller
        // passes an `is_alive` that includes its own PID; this confirms the
        // selector respects that.
        use std::path::PathBuf;
        let own_pid = std::process::id();
        let candidates = vec![
            (
                PathBuf::from(format!("/tmp/corrie-cache-{}-self", own_pid)),
                format!("corrie-cache-{}-self", own_pid),
            ),
        ];
        let targets = select_reap_targets(candidates, |pid| pid == own_pid);
        assert!(targets.is_empty());
    }
}
