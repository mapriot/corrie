//! `corrie merge`: equal to tiling all sources at once, writable into one of
//! its own inputs, and strict about duplicate inputs and layer names.
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn corrie(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_corrie")).args(args).output().expect("spawn corrie")
}

fn s(p: &Path) -> &str { p.to_str().unwrap() }

/// Tile one fixture SHP starting at `lon0` into `<name>.pmtiles`, using
/// output layer `layer`.
fn tile(dir: &Path, name: &str, lon0: f64, layer: &str) -> PathBuf {
    let sub = dir.join(name);
    std::fs::create_dir_all(&sub).unwrap();
    common::write_shapefile_at(&sub, "tiny.shp", lon0);
    let cfg = common::write_tiny_config(&sub, "tiny.shp");
    let json = std::fs::read_to_string(&cfg).unwrap().replace("\"contour\": {", &format!("\"{layer}\": {{"));
    std::fs::write(&cfg, json).unwrap();
    let out = dir.join(format!("{name}.pmtiles"));
    let o = corrie(&["tile", s(&cfg), s(&out)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    out
}

#[test]
fn merge_into_one_of_its_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let a = tile(d, "a", 14.0, "contour");
    let b = tile(d, "b", 14.4, "contour");
    let fresh = d.join("fresh.pmtiles");
    assert!(corrie(&["merge", s(&fresh), s(&a), s(&b)]).status.success());

    // "Add a country to the world file": the output is also an input.
    let o = corrie(&["merge", s(&a), s(&a), s(&b)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!d.join("a.pmtiles.partial").exists());
    let cmp = corrie(&["compare", s(&fresh), s(&a)]);
    assert!(cmp.status.success(), "{}", String::from_utf8_lossy(&cmp.stdout));
}

#[test]
fn rejects_duplicate_inputs_and_mixed_layer_names() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let a = tile(d, "a", 14.0, "contour");
    let b = tile(d, "b", 14.4, "contours");

    let o = corrie(&["merge", s(&d.join("x.pmtiles")), s(&a), s(&a)]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("listed twice"));

    let o = corrie(&["merge", s(&d.join("y.pmtiles")), s(&a), s(&b)]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("one layer name"));
    assert!(!d.join("y.pmtiles").exists() && !d.join("y.pmtiles.partial").exists());
}
