use corrie::feature_store::{Builder, FeatureRecord, Reader, compute_source_hash, write_manifest, read_manifest, Manifest};

#[test]
fn write_and_count_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Builder::create(dir.path()).expect("create builder");

    // Two features: 3 vertices and 5 vertices.
    let coords_a = vec![[14.0_f64, 50.0], [14.1, 50.1], [14.2, 50.2]];
    let coords_b = vec![[15.0_f64, 51.0], [15.1, 51.05], [15.2, 51.1], [15.25, 51.15], [15.3, 51.2]];

    b.add_feature(&coords_a, 100, 11, 0b11, 0).unwrap();
    b.add_feature(&coords_b, 50, 11, 0b01, 1).unwrap();

    let count = b.finalize().expect("finalize");
    assert_eq!(count, 2);

    let features_path = dir.path().join("features.bin");
    let meta = std::fs::metadata(&features_path).unwrap();
    assert_eq!(meta.len(), 2 * std::mem::size_of::<FeatureRecord>() as u64);

    let coords_path = dir.path().join("coords.bin");
    let coords_meta = std::fs::metadata(&coords_path).unwrap();
    // 8 B/vertex: two i32s, lon and latp at 1e-7 scale.
    assert_eq!(coords_meta.len(), (3 + 5) * 8);
}

#[test]
fn round_trip_via_mmap_reader() {
    let dir = tempfile::tempdir().unwrap();
    let mut b = Builder::create(dir.path()).unwrap();
    let coords = vec![[14.0_f64, 57.0], [14.5, 57.5]];
    b.add_feature(&coords, 100, 11, 0b11, 0).unwrap();
    b.add_feature(&coords, 50, 11, 0b01, 0).unwrap();
    b.finalize().unwrap();

    let r = Reader::open(dir.path()).expect("open");
    let recs = r.features();
    assert_eq!(recs.len(), 2);
    let e0 = recs[0].elevation;
    assert_eq!(e0, 100);
    let f0 = recs[0].flags;
    assert_eq!(f0, 0b11);
    let e1 = recs[1].elevation;
    assert_eq!(e1, 50);

    // coords_of returns raw i32 fixed-point at 1e-7 scale.
    let c0 = r.coords_of(&recs[0]);
    assert_eq!(c0, &[[140_000_000_i32, 570_000_000], [145_000_000, 575_000_000]]);

    // decode_coords_into reconstructs the f64 values (lossless to 1e-7 deg).
    let mut decoded: Vec<[f64; 2]> = Vec::new();
    r.decode_coords_into(&recs[0], &mut decoded);
    assert_eq!(decoded.len(), 2);
    for (got, exp) in decoded.iter().zip([[14.0_f64, 57.0], [14.5, 57.5]].iter()) {
        assert!((got[0] - exp[0]).abs() < 5e-8, "lon: got {} expected {}", got[0], exp[0]);
        assert!((got[1] - exp[1]).abs() < 5e-8, "latp: got {} expected {}", got[1], exp[1]);
    }
}

#[test]
fn source_hash_changes_with_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.shp");
    std::fs::write(&a, b"shp-bytes-1").unwrap();
    let h1 = compute_source_hash(&[a.clone()]).unwrap();

    // Sleep > 1s to ensure mtime resolution catches the change.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&a, b"shp-bytes-2").unwrap();
    let h2 = compute_source_hash(&[a]).unwrap();
    assert_ne!(h1, h2);
}

#[test]
fn manifest_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let m = Manifest {
        version: 1,
        feature_count: 42,
        vertex_count: 100,
        layer_names: vec!["contour".to_string()],
        source_hash: [0xab; 32],
    };
    write_manifest(dir.path(), &m).unwrap();
    let back = read_manifest(dir.path()).unwrap();
    assert_eq!(m.feature_count, back.feature_count);
    assert_eq!(m.source_hash, back.source_hash);
    assert_eq!(m.layer_names, back.layer_names);
}
