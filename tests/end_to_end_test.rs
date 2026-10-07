mod common;

use std::process::Command;

#[test]
fn tile_tiny_fixture_produces_pmtiles() {
    let (dir, _shp, cfg) = common::make_tiny_fixture();
    let out = dir.path().join("out.pmtiles");
    let exe = env!("CARGO_BIN_EXE_corrie");
    let status = Command::new(exe)
        .args(["tile", cfg.to_str().unwrap(), out.to_str().unwrap()])
        .status()
        .expect("spawn corrie");
    assert!(status.success(), "corrie tile failed");
    assert!(out.exists(), "output PMTiles missing");
    let sz = std::fs::metadata(&out).unwrap().len();
    assert!(sz > 200, "output suspiciously tiny: {} bytes", sz);

    // The `clustered` flag is byte 96 of the 127-byte PMTiles v3 header.
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(bytes[96], 1, "output must be clustered");
}
