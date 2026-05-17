//! Per-tile generation: spatial-query → clip → simplify → quantise → encode MVT.

use crate::mercator::meter2degp;

/// Compute the per-tile Douglas-Peucker epsilon, in degrees of (lon, latp).
///
/// Matches tilemaker's `src/tile_worker.cpp:432-439`:
///   if z >= simplify_below:    eps = 0
///   else if simplify_length>0: base = meter2degp(simplify_length, tile_centre_latp)
///        else:                  base = simplify_level
///   eps = base * simplify_ratio^(simplify_below - 1 - z)
pub fn tile_eps_degp(
    z: u8,
    simplify_below: u8,
    simplify_level: f64,
    simplify_length: f64,
    simplify_ratio: f64,
    tile_centre_latp: f64,
) -> f64 {
    if z >= simplify_below { return 0.0; }
    let base = if simplify_length > 0.0 {
        meter2degp(simplify_length, tile_centre_latp)
    } else {
        simplify_level
    };
    let exp = (simplify_below as i32 - 1 - z as i32) as f64;
    base * simplify_ratio.powf(exp)
}

use crate::mercator::{lat2latp, tilex2lon, tiley2latp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileCoord { pub z: u8, pub x: u32, pub y: u32 }

/// All tiles at zooms `[zmin..=zmax]` whose extent intersects `bbox`.
/// `bbox = [min_lon, min_lat, max_lon, max_lat]` in WGS84 degrees.
pub fn enumerate_tiles(bbox: [f64; 4], zmin: u8, zmax: u8) -> Vec<TileCoord> {
    let (min_lon, min_lat, max_lon, max_lat) = (bbox[0], bbox[1], bbox[2], bbox[3]);
    let min_latp = lat2latp(min_lat);
    let max_latp = lat2latp(max_lat);
    let mut out = Vec::new();
    for z in zmin..=zmax {
        let n = (1u64 << z) as u32;
        let lon_to_x = |lon: f64| -> u32 {
            let v = ((lon + 180.0) / 360.0 * n as f64).floor() as i64;
            v.clamp(0, n as i64 - 1) as u32
        };
        let latp_to_y = |latp: f64| -> u32 {
            let latp_max = tiley2latp(0, z);
            let v = ((latp_max - latp) / (2.0 * latp_max) * n as f64).floor() as i64;
            v.clamp(0, n as i64 - 1) as u32
        };
        let x0 = lon_to_x(min_lon);
        let x1 = lon_to_x(max_lon);
        let y0 = latp_to_y(max_latp); // north has smaller y
        let y1 = latp_to_y(min_latp);
        for x in x0..=x1 {
            for y in y0..=y1 {
                out.push(TileCoord { z, x, y });
            }
        }
        // Silence unused warning while keeping the helper expressive.
        let _ = tilex2lon(0, z);
    }
    out
}

/// In-place Hilbert-curve ordering by `pmtiles::TileId`. PMTiles requires
/// directory entries to be sorted this way for `clustered=true`.
pub fn hilbert_sort(tiles: &mut [TileCoord]) {
    use pmtiles::TileCoord as PTCoord;
    use pmtiles::TileId;
    tiles.sort_by_cached_key(|t| {
        TileId::from(PTCoord::new(t.z, t.x, t.y).expect("valid coord")).value()
    });
}

use crate::clip::{clip_polyline_to_box, clip_segment_to_box, Box2};
use crate::config::SourceLayer;
use crate::feature_store::{FeatureRecord, Reader};
use crate::mercator::extend_box;
use crate::mvt::{encode_tile, ContourFeature, EXTENT};
use crate::simplify::simplify;
use crate::spatial_index::{query_rtree, FeatureBbox};
use rstar::{RTree, AABB};

/// Per-tile output: gzip-compressed MVT bytes, or None if empty.
pub fn process_tile(
    tile: TileCoord,
    reader: &Reader,
    rtree: &RTree<FeatureBbox>,
    sources: &[SourceLayer],
    output_layer_name: &str,
    elevation_attr: &str,
) -> Option<Vec<u8>> {
    let z = tile.z;

    // Tile bounds in (lon, latp).
    let min_lon  = tilex2lon(tile.x, z);
    let max_lon  = tilex2lon(tile.x + 1, z);
    let max_latp = tiley2latp(tile.y, z);
    let min_latp = tiley2latp(tile.y + 1, z);
    let tile_centre_latp = (min_latp + max_latp) * 0.5;

    let (emnx, emny, emxx, emxy) = extend_box(min_lon, min_latp, max_lon, max_latp);
    let extend = Box2 { min_x: emnx, min_y: emny, max_x: emxx, max_y: emxy };
    let clip = Box2 { min_x: min_lon, min_y: min_latp, max_x: max_lon, max_y: max_latp };

    // Quantization scales.
    let xscale = (max_lon - min_lon) / EXTENT as f64;
    let yscale = (max_latp - min_latp) / EXTENT as f64;

    // R-tree query: use the exact tile clip box so only features whose bbox
    // intersects the actual tile are included. The extend box is kept for the
    // geometry clip below (visual continuity across tile edges).
    let query = AABB::from_corners([min_lon as f32, min_latp as f32], [max_lon as f32, max_latp as f32]);
    let hits: Vec<&FeatureBbox> = query_rtree(rtree, query);
    if hits.is_empty() { return None; }

    // Per source-layer eps cache (one per unique simplify config encountered).
    let mut eps_cache: Vec<Option<f64>> = vec![None; sources.len()];

    let mut features_out: Vec<ContourFeature> = Vec::new();
    let feature_recs: &[FeatureRecord] = reader.features();

    for fb in hits {
        let rec = &feature_recs[fb.idx as usize];
        // Layer-level minzoom/maxzoom gate.
        // CRITICAL: rec is from mmap of a `packed(4)` struct. Copy fields by value.
        let rec_layer_id = rec.layer_id;
        let rec_minzoom = rec.minzoom;
        let rec_elevation = rec.elevation;
        let rec_flags = rec.flags;

        let s = &sources[rec_layer_id as usize];
        if z < s.minzoom || z > s.maxzoom { continue; }
        // Per-feature minzoom gate.
        if rec_minzoom > z { continue; }

        // Compute / cache eps.
        let eps = match eps_cache[rec_layer_id as usize] {
            Some(v) => v,
            None => {
                let v = tile_eps_degp(
                    z, s.simplify_below, s.simplify_level, s.simplify_length,
                    s.simplify_ratio, tile_centre_latp);
                eps_cache[rec_layer_id as usize] = Some(v);
                v
            }
        };

        let coords = reader.coords_of(rec);

        // Clip → multi-segments inside extend box.
        let subs = clip_polyline_to_box(coords, &extend);
        for sub in subs {
            // Drop sub-polylines that are entirely in the extend zone and never
            // cross the exact tile box — they carry no visual content for this tile.
            let crosses_tile = sub.windows(2).any(|w| {
                clip_segment_to_box((w[0][0], w[0][1]), (w[1][0], w[1][1]), &clip).is_some()
            });
            if !crosses_tile {
                continue; // entirely in the extend zone, not the actual tile
            }

            // Simplify in (lon, latp) degp space.
            let simplified = if eps > 0.0 {
                let pts: Vec<(f64, f64)> = sub.iter().map(|p| (p[0], p[1])).collect();
                let r = simplify(&pts, eps);
                r.into_iter().collect::<Vec<_>>()
            } else {
                sub.iter().map(|p| (p[0], p[1])).collect()
            };
            if simplified.len() < 2 { continue; }

            // Quantize to MVT extent (px, py), drop consecutive duplicates.
            let mut points: Vec<(i32, i32)> = Vec::with_capacity(simplified.len());
            for (lon, latp) in simplified {
                let px = ((lon - min_lon) / xscale).floor() as i32;
                let py = ((max_latp - latp) / yscale).floor() as i32;
                if let Some(&last) = points.last() {
                    if last == (px, py) { continue; }
                }
                points.push((px, py));
            }
            if points.len() < 2 { continue; }

            features_out.push(ContourFeature {
                points,
                elevation: rec_elevation as i32,
                main: (rec_flags & 0b01) != 0,
                show: (rec_flags & 0b10) != 0,
            });
        }
    }

    if features_out.is_empty() { return None; }

    let mvt = encode_tile(&features_out, output_layer_name, elevation_attr);
    Some(gzip_compress(&mvt))
}

fn gzip_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data).expect("gzip write");
    enc.finish().expect("gzip finish")
}

// Suppress unused-import warning if lat2latp isn't used yet.
const _: fn(f64) -> f64 = lat2latp;
