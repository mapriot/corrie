mod common;
use corrie::config::{parse_and_resolve, Config};

#[test]
fn parse_tiny_config() {
    let (dir, _shp, cfg) = common::make_tiny_fixture();
    let config: Config = corrie::config::parse(&cfg).expect("parse");
    assert_eq!(config.settings.minzoom, 11);
    assert_eq!(config.settings.maxzoom, 14);
    assert_eq!(config.layers.len(), 1);
    let l = config.layers.get("contour").unwrap();
    assert_eq!(l.simplify_below, 14);
    drop(dir);
}

#[test]
fn parse_and_resolve_single_layer() {
    let (dir, _shp, cfg) = common::make_tiny_fixture();
    let sources = parse_and_resolve(&cfg).expect("resolve");
    assert_eq!(sources.len(), 1);
    let s = &sources[0];
    assert_eq!(s.source_layer_name, "contour");
    assert_eq!(s.output_layer_name, "contour"); // no write_to ⇒ same name
    drop(dir);
}

#[test]
fn write_to_merges_into_target_layer() {
    let dir = tempfile::tempdir().unwrap();
    let shp_path = common::write_tiny_shapefile(dir.path());
    let cfg_path = dir.path().join("merged.json");
    let json = format!(r#"{{
      "settings": {{
        "minzoom": 11, "maxzoom": 14,
        "bounding_box": [13,49,15,51],
        "compress": "gzip"
      }},
      "layers": {{
        "contour": {{
          "minzoom":11, "maxzoom":14,
          "source": "{shp}", "source_columns": ["elevation"],
          "simplify_below":14, "simplify_level":0.00005, "simplify_ratio":2
        }},
        "cz_extra": {{
          "minzoom":11, "maxzoom":14,
          "source": "{shp}", "source_columns": ["elevation"],
          "write_to": "contour",
          "simplify_below":14, "simplify_level":0.00005, "simplify_ratio":2
        }}
      }}
    }}"#, shp = shp_path.file_name().unwrap().to_str().unwrap());
    std::fs::write(&cfg_path, json).unwrap();
    let sources = parse_and_resolve(&cfg_path).unwrap();
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().all(|s| s.output_layer_name == "contour"));
}

#[test]
fn parse_real_mapriot_config_if_present() {
    // Point CORRIE_REAL_CONFIG at mapriot's config-contours.json to run this.
    let Some(path) = std::env::var_os("CORRIE_REAL_CONFIG") else {
        eprintln!("skipping: CORRIE_REAL_CONFIG not set");
        return;
    };
    let sources = parse_and_resolve(std::path::Path::new(&path)).expect("real config");
    assert!(sources.len() >= 100, "expected many layers; got {}", sources.len());
    let all_writes_to_contour = sources.iter().all(|s| s.output_layer_name == "contour");
    assert!(all_writes_to_contour, "expected every layer to write_to=contour");
}
