use corrie::feature_store::{Builder, FeatureRecord, Reader};

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
    assert_eq!(coords_meta.len(), (3 + 5) * 16);
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

    let c0 = r.coords_of(&recs[0]);
    assert_eq!(c0, &[[14.0, 57.0], [14.5, 57.5]]);
}
