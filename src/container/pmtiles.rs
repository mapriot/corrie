use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use pmtiles::{PmTilesStreamWriter, PmTilesWriter, TileCoord, TileType};
use xxhash_rust::xxh3::xxh3_64;

pub struct PmtilesWriter {
    inner: PmTilesStreamWriter<File>,
    seen_hashes: HashMap<u64, u32>,
}

impl PmtilesWriter {
    pub fn create(
        path: &Path,
        min_z: u8,
        max_z: u8,
        bounds: (f64, f64, f64, f64),
        layer_name: &str,
        elevation_attr: &str,
    ) -> Result<Self> {
        let (west, south, east, north) = bounds;
        let center_lon = (west + east) / 2.0;
        let center_lat = (south + north) / 2.0;
        let center_zoom = ((min_z + max_z) / 2).max(min_z);

        // TileJSON 3.0 vector_layers — required by MapLibre for layer introspection
        let metadata = format!(
            r#"{{"vector_layers":[{{"id":"{layer_name}","fields":{{"{elevation_attr}":"Number","main":"Number","show":"Number"}},"minzoom":{min_z},"maxzoom":{max_z}}}]}}"#
        );

        let f = File::create(path).with_context(|| format!("create {:?}", path))?;
        let inner = PmTilesWriter::new(TileType::Mvt)
            .min_zoom(min_z)
            .max_zoom(max_z)
            .bounds(west, south, east, north)
            .center(center_lon, center_lat)
            .center_zoom(center_zoom)
            .metadata(&metadata)
            .create(f)
            .context("create PMTiles writer")?;
        Ok(Self {
            inner,
            seen_hashes: HashMap::new(),
        })
    }

    pub fn add_tile(&mut self, z: u8, x: u32, y_xyz: u32, data: &[u8]) -> Result<()> {
        let h = xxh3_64(data);
        *self.seen_hashes.entry(h).or_insert(0) += 1;
        let coord = TileCoord::new(z, x, y_xyz).context("TileCoord")?;
        self.inner.add_tile(coord, data).context("add_tile")
    }

    /// Returns (unique_blobs, total_tiles).
    pub fn dedup_stats(&self) -> (u64, u64) {
        let unique = self.seen_hashes.len() as u64;
        let total: u64 = self.seen_hashes.values().map(|&n| n as u64).sum();
        (unique, total)
    }

    pub fn finalize(self) -> Result<()> {
        self.inner.finalize().context("finalize PMTiles")
    }
}
