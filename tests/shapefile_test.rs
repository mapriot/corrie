mod common;

use corrie::shapefile_read::read_polylines;

#[test]
fn read_tiny_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let shp = common::write_tiny_shapefile(dir.path());
    let features = read_polylines(&shp, "elevation").expect("read");
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

#[test]
fn read_features_have_bbox() {
    let dir = tempfile::tempdir().unwrap();
    let shp = common::write_tiny_shapefile(dir.path());
    let features = read_polylines(&shp, "elevation").expect("read");
    for f in &features {
        // All vertices should be inside the feature's bbox.
        for c in &f.coords {
            assert!(c[0] >= f.bbox_min_lon - 1e-12);
            assert!(c[0] <= f.bbox_max_lon + 1e-12);
            assert!(c[1] >= f.bbox_min_latp - 1e-12);
            assert!(c[1] <= f.bbox_max_latp + 1e-12);
        }
    }
}
