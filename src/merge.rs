//! Merge contour PMTiles archives (one per country or region) into one.
//!
//! Tiling is per feature: every line is clipped, simplified and quantized on
//! its own and only grouped by (elevation, main, show) at the end. So a tile
//! built from the features of A and B together holds exactly the lines of
//! A's tile plus the lines of B's tile, and merging archives tile by tile
//! gives the same tiles as tiling all their sources at once (line order
//! inside a group aside).
//!
//! Inputs are walked in tile_id order. A tile present in one input is copied
//! as its stored gzip bytes; only tiles present in several inputs (borders)
//! are decoded, merged and re-encoded.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;

use crate::container::pmtiles::{
    gunzip, gzip_tile, PmtilesReader, PmtilesWriter, TileEntry, COMPRESSION_GZIP, TILE_TYPE_MVT,
};
use crate::mvt::{decode_tile, encode_tile, ContourFeature};

pub struct MergeStats {
    pub tiles: u64,
    /// Tiles present in more than one input (decoded and re-encoded).
    pub merged: u64,
}

/// Merge `inputs` into a new clustered archive at `output`.
pub fn merge_archives(inputs: &[PathBuf], output: &Path, layer_name: &str, elevation_attr: &str) -> Result<MergeStats> {
    Merge::open(inputs)?.write(output, layer_name, elevation_attr, |_| Ok(()))
}

/// Opened inputs with their tiles grouped by tile_id, ready to write.
pub struct Merge {
    readers: Vec<PmtilesReader>,
    /// (tile_id, input index, entry), sorted by tile_id then input.
    all: Vec<(u64, u32, TileEntry)>,
    /// Ranges of `all` that share a tile_id.
    groups: Vec<std::ops::Range<usize>>,
}

impl Merge {
    /// Open every input and read its directories.
    pub fn open(inputs: &[PathBuf]) -> Result<Self> {
        if inputs.is_empty() { bail!("no inputs"); }
        let readers: Vec<PmtilesReader> = inputs.iter().map(|p| PmtilesReader::open(p)).collect::<Result<_>>()?;
        for (r, p) in readers.iter().zip(inputs) {
            let h = r.header();
            if h.tile_type != TILE_TYPE_MVT || h.tile_compression != COMPRESSION_GZIP {
                bail!("{:?}: expected gzipped MVT tiles", p);
            }
        }

        let mut all: Vec<(u64, u32, TileEntry)> = Vec::new();
        for (i, r) in readers.iter().enumerate() {
            all.extend(r.entries().with_context(|| format!("directories of {:?}", inputs[i]))?
                .into_iter().map(|e| (e.tile_id, i as u32, e)));
        }
        all.par_sort_unstable_by_key(|&(id, i, _)| (id, i));
        let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
        let mut s = 0;
        for k in 1..=all.len() {
            if k == all.len() || all[k].0 != all[s].0 {
                groups.push(s..k);
                s = k;
            }
        }
        Ok(Self { readers, all, groups })
    }

    /// Write the merged archive. `created` is called right after `output`
    /// has been created (or truncated), before any tile is written.
    pub fn write(
        &self,
        output: &Path,
        layer_name: &str,
        elevation_attr: &str,
        created: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<MergeStats> {
        let hs: Vec<_> = self.readers.iter().map(|r| r.header().clone()).collect();
        let min_z = hs.iter().map(|h| h.min_zoom).min().unwrap();
        let max_z = hs.iter().map(|h| h.max_zoom).max().unwrap();
        let e7 = |v: i32| v as f64 / 1e7;
        let bounds = (
            e7(hs.iter().map(|h| h.min_lon_e7).min().unwrap()),
            e7(hs.iter().map(|h| h.min_lat_e7).min().unwrap()),
            e7(hs.iter().map(|h| h.max_lon_e7).max().unwrap()),
            e7(hs.iter().map(|h| h.max_lat_e7).max().unwrap()),
        );
        let mut writer = PmtilesWriter::create(
            output, min_z, max_z, bounds, layer_name, elevation_attr, self.groups.len() as u64,
        )?;
        created(output)?;

        let mut merged = 0u64;
        const BATCH: usize = 16_384;
        for batch in self.groups.chunks(BATCH) {
            let out: Vec<Result<(u64, Cow<[u8]>)>> = batch
                .par_iter()
                .map(|g| {
                    let parts = &self.all[g.clone()];
                    let id = parts[0].0;
                    if parts.len() == 1 {
                        let (_, i, e) = parts[0];
                        return Ok((id, Cow::Borrowed(self.readers[i as usize].tile_bytes(&e))));
                    }
                    let raws: Vec<Vec<u8>> = parts.iter()
                        .map(|&(_, i, e)| gunzip(self.readers[i as usize].tile_bytes(&e)))
                        .collect::<Result<_>>()?;
                    let mvt = merge_tiles(&raws, layer_name, elevation_attr)?;
                    Ok((id, Cow::Owned(gzip_tile(&mvt)?)))
                })
                .collect();
            for (r, g) in out.into_iter().zip(batch) {
                let (id, bytes) = r?;
                if g.len() > 1 { merged += 1; }
                writer.add_tile_gzipped(id, &bytes)?;
            }
        }
        writer.finalize()?;
        Ok(MergeStats { tiles: self.groups.len() as u64, merged })
    }
}

/// Merge raw (uncompressed) contour MVTs into one: lines with the same
/// (elevation, main, show) end up in one MultiLineString feature, features
/// ordered as `process_tile` orders them.
pub fn merge_tiles(raws: &[Vec<u8>], layer_name: &str, elevation_attr: &str) -> Result<Vec<u8>> {
    // Key matches process_tile's (elevation, flags) sort: flags = main | show << 1.
    let mut groups: BTreeMap<(i32, u8), Vec<Vec<(i32, i32)>>> = BTreeMap::new();
    for raw in raws {
        for f in decode_tile(raw, elevation_attr)? {
            let flags = f.main as u8 | (f.show as u8) << 1;
            groups.entry((f.elevation, flags)).or_default().extend(f.lines);
        }
    }
    let features: Vec<ContourFeature> = groups.into_iter()
        .map(|((elevation, flags), lines)| ContourFeature {
            lines, elevation, main: flags & 1 != 0, show: flags & 2 != 0,
        })
        .collect();
    Ok(encode_tile(&features, layer_name, elevation_attr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cf(elevation: i32, main: bool, show: bool, lines: Vec<Vec<(i32, i32)>>) -> ContourFeature {
        ContourFeature { lines, elevation, main, show }
    }

    #[test]
    fn merge_tiles_unions_lines_per_attribute_group() {
        let a = encode_tile(&[
            cf(100, true, true, vec![vec![(0, 0), (5, 5)]]),
            cf(110, false, false, vec![vec![(1, 1), (2, 2)]]),
        ], "contour", "elevation");
        let b = encode_tile(&[
            cf(100, true, true, vec![vec![(9, 9), (8, 8)], vec![(7, 7), (6, 6)]]),
            cf(105, false, false, vec![vec![(3, 3), (4, 4)]]),
        ], "contour", "elevation");
        let merged = decode_tile(&merge_tiles(&[a, b], "contour", "elevation").unwrap(), "elevation").unwrap();
        let summary: Vec<(i32, bool, bool, usize)> =
            merged.iter().map(|f| (f.elevation, f.main, f.show, f.lines.len())).collect();
        assert_eq!(summary, vec![(100, true, true, 3), (105, false, false, 1), (110, false, false, 1)]);
    }
}
