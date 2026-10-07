//! Stopping `corrie tile` with a signal: the run's temp cache and partial
//! output are removed, an existing output stays as it was, and a signal the
//! process was started with ignored (nohup) stays ignored.
#![cfg(unix)]
mod common;

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Start `corrie tile` on a fixture that takes a moment to tile, with its
/// temp dir inside `dir`, writing over an existing `out.pmtiles`.
fn start(dir: &Path, ignore_sighup: bool) -> Child {
    common::write_large_shapefile(dir, "big.shp", 400, 4000);
    let cfg = common::write_tiny_config(dir, "big.shp");
    std::fs::create_dir_all(dir.join("tmp")).unwrap();
    std::fs::write(dir.join("out.pmtiles"), b"previous build").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_corrie"));
    cmd.args(["tile", cfg.to_str().unwrap(), dir.join("out.pmtiles").to_str().unwrap(), "-j", "1"])
        .env("TMPDIR", dir.join("tmp"));
    if ignore_sighup {
        // As nohup does: SIGHUP ignored before exec.
        unsafe { cmd.pre_exec(|| { libc::signal(libc::SIGHUP, libc::SIG_IGN); Ok(()) }); }
    }
    cmd.spawn().expect("spawn corrie")
}

/// Wait until the run is writing tiles (its .partial output exists).
fn wait_for_tiling(child: &mut Child, dir: &Path) {
    let t0 = Instant::now();
    while !dir.join("out.pmtiles.partial").exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("corrie finished ({status}) before it could be stopped; enlarge the fixture");
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "no .partial output after 120 s");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn temp_entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir.join("tmp")).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn sigterm_while_tiling_removes_cache_and_partial_output_only() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut child = start(d, false);
    wait_for_tiling(&mut child, d);
    assert!(temp_entries(d).iter().any(|n| n.starts_with("corrie-cache-")));

    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130), "{status}");
    assert!(!d.join("out.pmtiles.partial").exists(), "partial output left behind");
    assert_eq!(std::fs::read(d.join("out.pmtiles")).unwrap(), b"previous build", "existing output changed");
    assert!(temp_entries(d).is_empty(), "temp cache left: {:?}", temp_entries(d));
}

#[test]
fn sighup_ignored_at_start_stays_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut child = start(d, true);
    wait_for_tiling(&mut child, d);

    unsafe { libc::kill(child.id() as i32, libc::SIGHUP) };
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    assert_eq!(&std::fs::read(d.join("out.pmtiles")).unwrap()[..7], b"PMTiles");
    assert!(temp_entries(d).is_empty(), "temp cache left: {:?}", temp_entries(d));
}
