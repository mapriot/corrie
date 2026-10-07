//! Remove temp data and partial output when a run is stopped.
//!
//! `corrie tile` can write hundreds of GB of temp cache, and neither the
//! cache's `TempDir` drop nor any other destructor runs when the process is
//! stopped with Ctrl+C, SIGTERM or SIGHUP (a closed terminal or tmux pane).
//! Paths registered here are deleted on those signals before the process
//! exits with status 130. SIGKILL and OOM kills still leave them behind;
//! `temp_cache::reap_orphan_caches` removes orphaned caches on the next run.
//!
//! A signal that was ignored when corrie started stays ignored: `nohup`
//! (SIGHUP) and background jobs of a non-interactive shell (SIGINT) keep
//! running as before.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;

struct Registry {
    handler_installed: bool,
    paths: Vec<PathBuf>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry { handler_installed: false, paths: Vec::new() });

/// Delete `path` (a file or a whole directory) if the process is stopped
/// before [`forget_on_signal`] is called for it. Register an output only
/// once this run has created it, so a stop never deletes an older file.
pub fn remove_on_signal(path: &Path) -> anyhow::Result<()> {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if !reg.handler_installed {
        let ignored = ignored_signals();
        ctrlc::set_handler(on_signal).context("install Ctrl+C / SIGTERM handler")?;
        // ctrlc replaces every disposition, including SIG_IGN set by nohup.
        restore_ignored(&ignored);
        reg.handler_installed = true;
    }
    reg.paths.push(path.to_path_buf());
    Ok(())
}

/// Stop deleting `path` on a signal (e.g. the output is complete).
pub fn forget_on_signal(path: &Path) {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.paths.retain(|p| p != path);
}

// Runs on ctrlc's own thread, not inside the signal handler, so file system
// calls are fine. Worker threads may still be reading or writing the files:
// on Unix an unlinked file stays usable until the process exits. The lock is
// held until exit, so the main thread cannot register, finish an output and
// exit 0 while its files are being removed.
fn on_signal() {
    let reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    eprintln!();
    for p in reg.paths.iter().rev() {
        match remove_path(p) {
            Ok(true) => eprintln!("Stopped: removed {}", p.display()),
            Ok(false) => {}
            Err(e) => eprintln!("Stopped: could not remove {}: {e}", p.display()),
        }
    }
    std::process::exit(130);
}

/// An output written as `<name>.partial` next to its final path and renamed
/// into place by [`PartialOutput::commit`]. Until then a file already at the
/// final path stays untouched and readable (it may even be one of the run's
/// inputs). A stop removes the partial file; so does dropping it without a
/// commit, e.g. when the run fails.
pub struct PartialOutput {
    target: PathBuf,
    partial: PathBuf,
    committed: bool,
}

impl PartialOutput {
    pub fn new(target: &Path) -> anyhow::Result<Self> {
        let name = target.file_name().context("output path has no file name")?;
        let mut partial_name = name.to_os_string();
        partial_name.push(".partial");
        let partial = target.with_file_name(partial_name);
        remove_on_signal(&partial)?;
        Ok(Self { target: target.to_path_buf(), partial, committed: false })
    }

    /// Where to write the output.
    pub fn path(&self) -> &Path { &self.partial }

    /// Move the finished file to its final path.
    pub fn commit(mut self) -> anyhow::Result<()> {
        std::fs::rename(&self.partial, &self.target)
            .with_context(|| format!("rename {:?} to {:?}", self.partial, self.target))?;
        self.committed = true;
        forget_on_signal(&self.partial);
        Ok(())
    }
}

impl Drop for PartialOutput {
    fn drop(&mut self) {
        if !self.committed {
            forget_on_signal(&self.partial);
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

#[cfg(unix)]
const HANDLED: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// The handled signals whose disposition is currently SIG_IGN.
#[cfg(unix)]
fn ignored_signals() -> Vec<libc::c_int> {
    HANDLED.iter().copied().filter(|&sig| {
        // SAFETY: a null new-action only queries the current disposition
        // into `old`, which is a zeroed, properly sized sigaction.
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) };
        rc == 0 && old.sa_sigaction == libc::SIG_IGN
    }).collect()
}

#[cfg(unix)]
fn restore_ignored(sigs: &[libc::c_int]) {
    for &sig in sigs {
        // SAFETY: setting a valid signal back to SIG_IGN.
        unsafe { libc::signal(sig, libc::SIG_IGN) };
    }
}

#[cfg(not(unix))]
fn ignored_signals() -> Vec<i32> { Vec::new() }
#[cfg(not(unix))]
fn restore_ignored(_: &[i32]) {}

/// Remove a file or directory tree. Ok(false) if it did not exist.
fn remove_path(p: &Path) -> std::io::Result<bool> {
    let r = match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    r.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_path_handles_files_dirs_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("partial.pmtiles");
        std::fs::write(&f, b"x").unwrap();
        let d = dir.path().join("corrie-cache-1-x");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/coords.bin"), b"y").unwrap();

        assert!(remove_path(&f).unwrap());
        assert!(remove_path(&d).unwrap());
        assert!(!f.exists() && !d.exists());
        assert!(!remove_path(&f).unwrap());
    }

    /// The handler must not take over a signal the process was told to
    /// ignore (nohup), and must still handle the others.
    #[cfg(unix)]
    #[test]
    fn installing_keeps_ignored_signals_ignored() {
        let disposition = |sig| {
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) };
            old.sa_sigaction
        };
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        let p = PathBuf::from("/nonexistent/installing-keeps-ignored");
        remove_on_signal(&p).unwrap();
        forget_on_signal(&p);
        assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN, "SIGHUP was ignored and must stay so");
        assert_ne!(disposition(libc::SIGTERM), libc::SIG_IGN);
        assert_ne!(disposition(libc::SIGTERM), libc::SIG_DFL, "SIGTERM must be handled");
    }

    #[test]
    fn partial_output_replaces_the_target_only_on_commit() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.pmtiles");
        std::fs::write(&target, b"old").unwrap();

        let out = PartialOutput::new(&target).unwrap();
        assert_eq!(out.path(), dir.path().join("out.pmtiles.partial"));
        std::fs::write(out.path(), b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        out.commit().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!dir.path().join("out.pmtiles.partial").exists());

        let failed = PartialOutput::new(&target).unwrap();
        std::fs::write(failed.path(), b"half").unwrap();
        drop(failed);
        assert!(!dir.path().join("out.pmtiles.partial").exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn forget_drops_only_that_path() {
        let a = PathBuf::from("/nonexistent/a");
        let b = PathBuf::from("/nonexistent/b");
        REGISTRY.lock().unwrap().paths.extend([a.clone(), b.clone()]);
        forget_on_signal(&a);
        assert!({ let reg = REGISTRY.lock().unwrap(); !reg.paths.contains(&a) && reg.paths.contains(&b) });
        forget_on_signal(&b);
    }
}
