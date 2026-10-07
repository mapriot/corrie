//! `corrie tile --shards`: one archive per source group, reused while its
//! inputs are unchanged, merged into the same tiles as a single run.
mod common;

use std::path::Path;
use std::process::{Command, Output};

fn corrie(args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_corrie")).args(args).output().expect("spawn corrie");
    assert!(out.status.success() || args[0] == "compare",
        "corrie {:?} failed:\n{}", args, String::from_utf8_lossy(&out.stderr));
    out
}

fn write_config(dir: &Path) -> std::path::PathBuf {
    let cfg = common::write_tiny_config(dir, "a/tiny.shp");
    // Add a second layer from b/ that writes into the same output layer.
    let json = std::fs::read_to_string(&cfg).unwrap().replace(
        "\n  }\n}\n",
        ",\n    \"b\": { \"minzoom\": 11, \"maxzoom\": 14, \"source\": \"b/tiny.shp\", \"source_columns\": [\"elevation\"],\n      \"write_to\": \"contour\", \"simplify_below\": 14, \"simplify_level\": 0.00005, \"simplify_ratio\": 2 }\n  }\n}\n",
    );
    std::fs::write(&cfg, json).unwrap();
    cfg
}

#[test]
fn sharded_build_matches_single_run_and_rebuilds_only_changed_groups() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("a")).unwrap();
    std::fs::create_dir_all(d.join("b")).unwrap();
    // b's lines start where a's end, so some tiles hold lines of both.
    common::write_shapefile_at(&d.join("a"), "tiny.shp", 14.0);
    let b_shp = common::write_shapefile_at(&d.join("b"), "tiny.shp", 14.4);
    let cfg = write_config(d);
    let (cfg, p) = (cfg.to_str().unwrap(), |n: &str| d.join(n).to_str().unwrap().to_string());

    corrie(&["tile", cfg, &p("single.pmtiles")]);
    let first = corrie(&["tile", cfg, &p("sharded.pmtiles"), "--shards", &p("groups")]);
    assert!(d.join("groups/a.pmtiles").exists() && d.join("groups/b.pmtiles").exists());
    let log = String::from_utf8_lossy(&first.stderr);
    assert!(log.contains("2 built, 0 reused"), "{log}");
    let joined: u64 = log.lines().find_map(|l| l.strip_prefix("Merged: "))
        .and_then(|l| l.split(", ").nth(1)?.split(' ').next()?.parse().ok())
        .expect("merge summary line");
    assert!(joined > 0, "expected tiles joined from both groups:\n{log}");

    let cmp = corrie(&["compare", &p("single.pmtiles"), &p("sharded.pmtiles")]);
    assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));

    let again = corrie(&["tile", cfg, &p("sharded2.pmtiles"), "--shards", &p("groups")]);
    assert!(String::from_utf8_lossy(&again.stderr).contains("0 built, 2 reused"));

    // A changed SHP (here: its modification time) rebuilds that group only.
    let f = std::fs::OpenOptions::new().write(true).open(&b_shp).unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
    drop(f);
    let third = corrie(&["tile", cfg, &p("sharded3.pmtiles"), "--shards", &p("groups")]);
    let log = String::from_utf8_lossy(&third.stderr);
    assert!(log.contains("b  building") && log.contains("1 built, 1 reused"), "{log}");
    let cmp = corrie(&["compare", &p("single.pmtiles"), &p("sharded3.pmtiles")]);
    assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));
}

#[test]
fn shards_and_cache_dir_are_exclusive() {
    let (dir, _shp, cfg) = common::make_tiny_fixture();
    let out = Command::new(env!("CARGO_BIN_EXE_corrie"))
        .args(["tile", cfg.to_str().unwrap(), dir.path().join("o.pmtiles").to_str().unwrap(),
               "--shards", dir.path().join("g").to_str().unwrap(),
               "--cache-dir", dir.path().join("c").to_str().unwrap()])
        .output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
}

/// A group whose data lies just outside the config's bounding_box, but in a
/// lowest-zoom tile the box intersects, gets the same tiles in a sharded run
/// as in a single run.
#[test]
fn group_just_outside_the_bounding_box_matches_a_single_run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("a")).unwrap();
    std::fs::create_dir_all(d.join("b")).unwrap();
    // z11 column 1103 spans 13.945..14.121°E; the box starts at 14.0°E.
    common::write_shapefile_at(&d.join("a"), "tiny.shp", 14.0);
    common::write_lines_between(&d.join("b"), "tiny.shp", 13.95, 13.99);
    let cfg = write_config(d);
    let json = std::fs::read_to_string(&cfg).unwrap()
        .replace("[13.0, 49.0, 15.0, 51.0]", "[14.0, 49.0, 15.0, 51.0]");
    assert!(json.contains("[14.0, 49.0"));
    std::fs::write(&cfg, json).unwrap();
    let (cfg, p) = (cfg.to_str().unwrap(), |n: &str| d.join(n).to_str().unwrap().to_string());

    corrie(&["tile", cfg, &p("single.pmtiles")]);
    corrie(&["tile", cfg, &p("sharded.pmtiles"), "--shards", &p("groups")]);
    let cmp = corrie(&["compare", &p("single.pmtiles"), &p("sharded.pmtiles")]);
    assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));
    // and group b did contribute tiles
    let b = corrie::container::pmtiles::PmtilesReader::open(&d.join("groups/b.pmtiles")).unwrap();
    assert!(!b.entries().unwrap().is_empty());
}
