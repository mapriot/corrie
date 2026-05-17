//! Tilemaker config.json parser (subset for SHP-only contour pipelines).
//!
//! Unknown OSM-only fields (Lua hooks, way_function, node_keys, …) are
//! silently ignored via serde's default behaviour.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub minzoom: u8,
    pub maxzoom: u8,
    #[serde(default)]
    pub basezoom: Option<u8>,
    #[serde(default = "default_include_ids")]
    pub include_ids: bool,
    pub bounding_box: [f64; 4],
    #[serde(default = "default_compress")]
    pub compress: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

fn default_include_ids() -> bool { true }
fn default_compress() -> String { "gzip".to_string() }

#[derive(Debug, Clone, Deserialize)]
pub struct LayerDef {
    pub minzoom: u8,
    pub maxzoom: u8,
    pub source: String,
    #[serde(default)]
    pub source_columns: Vec<String>,
    /// If present, this layer's features merge into the named output layer.
    #[serde(default)]
    pub write_to: Option<String>,
    #[serde(default)]
    pub simplify_below: u8,
    #[serde(default = "default_simplify_level")]
    pub simplify_level: f64,
    #[serde(default)]
    pub simplify_length: f64,
    #[serde(default = "default_simplify_ratio")]
    pub simplify_ratio: f64,
    #[serde(default)]
    pub simplify_algorithm: String,
}

fn default_simplify_level() -> f64 { 0.01 }
fn default_simplify_ratio() -> f64 { 2.0 }

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub settings: Settings,
    pub layers: BTreeMap<String, LayerDef>,
}

/// Output of `resolve` — one entry per *source* layer, with the effective
/// output layer name already looked up via `write_to`.
#[derive(Debug, Clone)]
pub struct SourceLayer {
    /// The JSON key (e.g., "cz1", "ae00").
    pub source_layer_name: String,
    /// The output layer this feeds (after resolving `write_to`).
    pub output_layer_name: String,
    /// Absolute path to the SHP file.
    pub shp_path: PathBuf,
    pub minzoom: u8,
    pub maxzoom: u8,
    pub source_columns: Vec<String>,
    pub simplify_below: u8,
    pub simplify_level: f64,
    pub simplify_length: f64,
    pub simplify_ratio: f64,
    pub simplify_algorithm: String,
}

/// Parse a tilemaker config.json file. Unknown fields are silently ignored.
pub fn parse(path: &Path) -> Result<Config> {
    let bytes = std::fs::read(path).with_context(|| format!("read {:?}", path))?;
    let config: Config = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse config {:?}", path))?;
    Ok(config)
}

/// Parse + resolve. The returned vector has one entry per source layer with
/// its `output_layer_name` looked up via `write_to`. Source paths are resolved
/// relative to the config file's directory.
pub fn parse_and_resolve(path: &Path) -> Result<Vec<SourceLayer>> {
    let config = parse(path)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let mut out = Vec::with_capacity(config.layers.len());
    for (name, l) in &config.layers {
        let output_name = l.write_to.clone().unwrap_or_else(|| name.clone());
        let shp_path: PathBuf = if Path::new(&l.source).is_absolute() {
            PathBuf::from(&l.source)
        } else {
            base_dir.join(&l.source)
        };
        out.push(SourceLayer {
            source_layer_name: name.clone(),
            output_layer_name: output_name,
            shp_path,
            minzoom: l.minzoom,
            maxzoom: l.maxzoom,
            source_columns: l.source_columns.clone(),
            simplify_below: l.simplify_below,
            simplify_level: l.simplify_level,
            simplify_length: l.simplify_length,
            simplify_ratio: l.simplify_ratio,
            simplify_algorithm: l.simplify_algorithm.clone(),
        });
    }
    Ok(out)
}
