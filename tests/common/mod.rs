//! Shared fixture builders for integration tests.
#![allow(dead_code)] // each test compiles its own copy

use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// A tiny contour shapefile: 5 lines at elevations 100, 150, 200, 250, 300 m,
/// spanning longitude 14.0 .. 14.5 around Prague (lat ~50.0), each line composed
/// of 10 vertices in a smooth arc.
pub fn write_tiny_shapefile(dir: &Path) -> PathBuf {
    use dbase::{FieldName, FieldValue, Record, TableWriterBuilder};
    use shapefile::record::polyline::Polyline as ShpPolyline;
    use shapefile::record::Point;

    let shp_path = dir.join("tiny.shp");

    let dbf_builder = TableWriterBuilder::new()
        .add_numeric_field(FieldName::try_from("elevation").unwrap(), 10, 0);

    let mut writer = shapefile::Writer::from_path(&shp_path, dbf_builder)
        .expect("shapefile writer");

    for (i, &elev) in [100i32, 150, 200, 250, 300].iter().enumerate() {
        let mut points: Vec<Point> = Vec::with_capacity(10);
        for j in 0..10 {
            let lon = 14.0 + 0.05 * (j as f64) + 0.001 * (i as f64);
            let lat = 50.0 + 0.001 * ((j as f64).sin());
            points.push(Point::new(lon, lat));
        }
        let poly = ShpPolyline::new(points);

        let mut rec = Record::default();
        rec.insert("elevation".to_string(), FieldValue::Numeric(Some(elev as f64)));

        writer.write_shape_and_record(&poly, &rec).expect("write feature");
    }
    // Writer flushes and closes on drop; no explicit finalize needed.
    drop(writer);

    shp_path
}

/// Write a minimal tilemaker config.json that references one SHP and one output layer.
pub fn write_tiny_config(dir: &Path, shp_relpath: &str) -> PathBuf {
    let cfg_path = dir.join("config.json");
    let json = format!(r#"{{
  "settings": {{
    "minzoom": 11,
    "maxzoom": 14,
    "basezoom": 14,
    "include_ids": false,
    "bounding_box": [13.0, 49.0, 15.0, 51.0],
    "name": "tiny test",
    "version": "0.0.1",
    "description": "fixture",
    "compress": "gzip"
  }},
  "layers": {{
    "contour": {{
      "minzoom": 11,
      "maxzoom": 14,
      "source": "{shp_relpath}",
      "source_columns": ["elevation"],
      "simplify_below": 14,
      "simplify_level": 0.00005,
      "simplify_ratio": 2
    }}
  }}
}}
"#);
    std::fs::write(&cfg_path, json).expect("write config");
    cfg_path
}

/// Create a tempdir and return (dir, shp_path, config_path) for an end-to-end test.
pub fn make_tiny_fixture() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let shp = write_tiny_shapefile(dir.path());
    let cfg = write_tiny_config(dir.path(), shp.file_name().unwrap().to_str().unwrap());
    (dir, shp, cfg)
}
