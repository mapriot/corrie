mod common;

use corrie::config::parse_and_resolve;
use corrie::feature_store::{Reader, read_manifest, cache_valid};
use corrie::ingest;

#[test]
fn ingest_tiny_fixture() {
    let (dir, _shp, cfg) = common::make_tiny_fixture();
    let sources = parse_and_resolve(&cfg).unwrap();
    let cache = dir.path().join(".corrie-cache");

    let stats = ingest::run(&sources, &cache).expect("ingest");
    assert_eq!(stats.features, 5);
    assert_eq!(stats.vertices, 5 * 10);

    let r = Reader::open(&cache).unwrap();
    assert_eq!(r.features().len(), 5);

    let m = read_manifest(&cache).unwrap();
    assert_eq!(m.feature_count, 5);
    assert_eq!(m.layer_names, vec!["contour".to_string()]);

    // Cache should validate against the source SHP file.
    let shp_paths: Vec<_> = sources.iter().map(|s| s.shp_path.clone()).collect();
    assert!(cache_valid(&cache, &shp_paths));
}

#[test]
fn failed_reingest_leaves_the_cache_invalid() {
    let (dir, shp, cfg) = common::make_tiny_fixture();
    let sources = parse_and_resolve(&cfg).unwrap();
    let cache = dir.path().join(".corrie-cache");
    ingest::run(&sources, &cache).expect("first ingest");
    let shp_paths: Vec<_> = sources.iter().map(|s| s.shp_path.clone()).collect();
    assert!(cache_valid(&cache, &shp_paths));

    // A second ingest that fails halfway (here: its SHP is gone) must not
    // leave the first run's manifest next to rewritten data files.
    std::fs::rename(&shp, dir.path().join("moved.shp")).unwrap();
    assert!(ingest::run(&sources, &cache).is_err());
    assert!(!cache.join("manifest.bin").exists());
    assert!(!cache_valid(&cache, &shp_paths));
}
