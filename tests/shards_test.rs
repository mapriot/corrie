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

/// The migration from SHPs to packs, one group at a time: pack one group,
/// build from the mixed config, delete that group's SHPs, then switch the
/// rest. The tiles never change, and only switched groups are tiled again.
#[test]
fn moving_groups_to_packs_keeps_the_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("a")).unwrap();
    std::fs::create_dir_all(d.join("b")).unwrap();
    common::write_shapefile_at(&d.join("a"), "tiny.shp", 14.0);
    common::write_shapefile_at(&d.join("b"), "tiny.shp", 14.4);
    write_config(d);
    let p = |n: &str| d.join(n).to_str().unwrap().to_string();
    let log = |o: &Output| String::from_utf8_lossy(&o.stderr).into_owned();
    let same = |x: &str, y: &str| {
        let cmp = corrie(&["compare", &p(x), &p(y)]);
        assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));
    };
    corrie(&["tile", &p("config.json"), &p("shp.pmtiles"), "--shards", &p("groups")]);

    // pack group b only: the copy differs from the config in b's source alone
    let packed = corrie(&["pack", "--config", &p("config.json"), "--only", "b"]);
    assert!(log(&packed).contains("1 source switched to packs"), "{}", log(&packed));
    assert!(d.join("b/tiny.cpk").exists() && !d.join("a/tiny.cpk").exists());
    let (orig, copy) = (std::fs::read_to_string(p("config.json")).unwrap(), std::fs::read_to_string(p("config-packs.json")).unwrap());
    let changed: Vec<(&str, &str)> = orig.lines().zip(copy.lines()).filter(|(x, y)| x != y).collect();
    assert_eq!(orig.lines().count(), copy.lines().count());
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].0.replace("b/tiny.shp", "b/tiny.cpk"), changed[0].1);

    let mixed = corrie(&["tile", &p("config-packs.json"), &p("mixed.pmtiles"), "--shards", &p("groups")]);
    assert!(log(&mixed).contains("1 built, 1 reused"), "{}", log(&mixed));
    same("shp.pmtiles", "mixed.pmtiles");

    // b's SHP set is no longer needed, and its archive is reused without it
    for ext in ["shp", "shx", "dbf", "prj"] {
        let _ = std::fs::remove_file(d.join("b/tiny").with_extension(ext));
    }
    let gone = corrie(&["tile", &p("config-packs.json"), &p("gone.pmtiles"), "--shards", &p("groups")]);
    assert!(log(&gone).contains("0 built, 2 reused"), "{}", log(&gone));
    same("shp.pmtiles", "gone.pmtiles");

    // an unknown group is an error, naming the groups there are
    let bad = Command::new(env!("CARGO_BIN_EXE_corrie"))
        .args(["pack", "--config", &p("config-packs.json"), "--only", "nope"]).output().unwrap();
    assert!(!bad.status.success() && log(&bad).contains("no group \"nope\"") && log(&bad).contains("groups: a, b"), "{}", log(&bad));

    // the rest, written over the packs config itself
    let rest = corrie(&["pack", "--config", &p("config-packs.json"), "--out-config", &p("config-packs.json")]);
    assert!(log(&rest).contains("1 source switched to packs"), "{}", log(&rest));
    let all = std::fs::read_to_string(p("config-packs.json")).unwrap();
    assert!(all.contains("a/tiny.cpk") && all.contains("b/tiny.cpk") && !all.contains(".shp"), "{all}");
    let packs = corrie(&["tile", &p("config-packs.json"), &p("packs.pmtiles"), "--shards", &p("groups")]);
    assert!(log(&packs).contains("1 built, 1 reused"), "{}", log(&packs));
    same("shp.pmtiles", "packs.pmtiles");
}

/// --per-group makes one pack per group from its sources (shapefiles or
/// packs) and a config with one layer per group; the tiles stay the same.
#[test]
fn one_pack_per_group_keeps_the_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for g in ["a", "b"] {
        std::fs::create_dir_all(d.join(g)).unwrap();
    }
    common::write_shapefile_at(&d.join("a"), "one.shp", 14.0);
    common::write_shapefile_at(&d.join("a"), "two.shp", 14.2);
    common::write_shapefile_at(&d.join("b"), "tiny.shp", 14.4);
    let layer = |src: &str, extra: &str| format!(
        r#"{{"minzoom": 11, "maxzoom": 14, "source": "{src}", "source_columns": ["elevation"], "write_to": "contour",
           "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2{extra}}}"#);
    let config = |extra: &str| format!(
        r#"{{"settings": {{"minzoom": 11, "maxzoom": 14, "bounding_box": [-180, -85, 180, 85]}},
           "layers": {{"a1": {}, "a2": {}, "b0": {}}}}}"#, layer("a/one.shp", ""), layer("a/two.shp", extra), layer("b/tiny.shp", ""));
    std::fs::write(d.join("config.json"), config("")).unwrap();
    let p = |n: &str| d.join(n).to_str().unwrap().to_string();
    let log = |o: &Output| String::from_utf8_lossy(&o.stderr).into_owned();
    corrie(&["tile", &p("config.json"), &p("shp.pmtiles")]);

    // from packs, as on a machine that only has the packs left
    corrie(&["pack", "--config", &p("config.json")]);
    let grouped = corrie(&["pack", "--config", &p("config-packs.json"), "--per-group", "--out-config", &p("groups.json")]);
    assert!(log(&grouped).contains("2 groups read one pack each"), "{}", log(&grouped));
    assert!(d.join("a/a.cpk").exists() && d.join("b/b.cpk").exists());
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p("groups.json")).unwrap()).unwrap();
    let layers = cfg["layers"].as_object().unwrap();
    assert_eq!(layers.keys().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(layers["a"]["source"], "a/a.cpk");
    assert_eq!(layers["a"]["write_to"], "contour");
    assert_eq!(cfg["settings"]["maxzoom"], 14);

    corrie(&["tile", &p("groups.json"), &p("groups.pmtiles")]);
    let cmp = corrie(&["compare", &p("shp.pmtiles"), &p("groups.pmtiles")]);
    assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));

    // again: everything is current
    let again = corrie(&["pack", "--config", &p("config-packs.json"), "--per-group", "--out-config", &p("groups.json")]);
    assert!(!log(&again).contains("Packing"), "{}", log(&again));

    // a group whose layers differ in more than their source stays as it is
    std::fs::write(d.join("config.json"), config(r#", "simplify_length": 10"#)).unwrap();
    let bad = Command::new(env!("CARGO_BIN_EXE_corrie"))
        .args(["pack", "--config", &p("config.json"), "--per-group"]).output().unwrap();
    assert!(!bad.status.success() && log(&bad).contains("differ in more than their source"), "{}", log(&bad));
}
