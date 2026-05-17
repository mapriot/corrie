use corrie::feature_store::{Builder, FeatureRecord};

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
