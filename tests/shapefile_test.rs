mod common;

use corrie::shapefile_read::{read_polylines_streaming, ReadFeature};

fn read_all(path: &std::path::Path, field: &str) -> Vec<ReadFeature> {
    let mut out = Vec::new();
    read_polylines_streaming(path, field, |f| { out.push(f); Ok(()) }).expect("read");
    out
}

#[test]
fn read_tiny_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let shp = common::write_tiny_shapefile(dir.path());
    let features = read_all(&shp, "elevation");
    assert_eq!(features.len(), 5);
    let elevations: Vec<i16> = features.iter().map(|f| f.elevation).collect();
    assert_eq!(elevations, vec![100, 150, 200, 250, 300]);
    for f in &features {
        assert_eq!(f.coords.len(), 10);
        // Coordinates should be in (lon, latp) space — latp ≈ lat for small lats.
        assert!(f.coords[0][0] >= 14.0);
        assert!(f.coords[0][1] > 49.0); // latp(50) ≈ 57.5 but still positive
    }
}
