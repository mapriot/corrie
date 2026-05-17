use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use pmtiles::{PmTilesStreamWriter, PmTilesWriter, TileCoord, TileType};
use xxhash_rust::xxh3::xxh3_64;

/// Pre-compressed representation of an empty tile (gzip of zero bytes).
///
/// Tiles that were spatially addressed but contained no surviving features
/// after clipping/simplification are stored as this constant blob.  Because
/// the pmtiles crate's `add_tile` skips zero-length payloads, we use
/// `add_raw_tile` with this pre-compressed sentinel instead.  All callers
/// pass identical bytes, so the crate's hash-based dedup stores only one
/// copy; consecutive occurrences in Hilbert order are further collapsed by
/// the RLE run_length mechanism.
///
/// The 23-byte sequence is a DEFLATE "stored block" gzip stream for zero bytes,
/// matching the format written by `pmtiles convert` (go-pmtiles).  The exact
/// encoding does not matter for dedup — any constant would work — but using the
/// same bytes as go-pmtiles makes the output wire-identical for empty tiles.
const EMPTY_GZIP_TILE: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0xff, 0x01, 0x00, 0x00, 0xff, 0xff, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

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
        let coord = TileCoord::new(z, x, y_xyz).context("TileCoord")?;
        if data.is_empty() {
            // Empty sentinel: the tile was addressed but contained no features.
            // The pmtiles crate skips zero-length payloads in add_tile, so we
            // submit the pre-compressed constant directly via add_raw_tile.
            // All empty tiles share the same hash → stored once; consecutive
            // occurrences in Hilbert order collapse via the RLE run_length.
            let h = xxh3_64(EMPTY_GZIP_TILE);
            *self.seen_hashes.entry(h).or_insert(0) += 1;
            self.inner.add_raw_tile(coord, EMPTY_GZIP_TILE).context("add_raw_tile (empty)")
        } else {
            let h = xxh3_64(data);
            *self.seen_hashes.entry(h).or_insert(0) += 1;
            self.inner.add_tile(coord, data).context("add_tile")
        }
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
