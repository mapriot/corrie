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

/// Slack on the f32 feature bbox when testing "fully inside tile". The feature
/// bbox is stored in f32 (FeatureRecord) and can underestimate the true f64
/// bbox by up to half a ULP. Worst-case ULP at lon=180 is ~2.1e-5 degrees;
/// 5e-5 gives a safe margin while still capturing the vast majority of
/// features at z=14 (tile width ~0.022° at z=14, so we lose the fast path
/// only for features within ~5.5 m of a tile boundary).
const F32_BBOX_SLACK: f64 = 5e-5;

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

/// In-place Hilbert-curve ordering. PMTiles requires directory entries to
/// be sorted by tile_id for `clustered=true`. Our `container::pmtiles::tile_id`
/// is cross-validated against the upstream crate in unit tests.
pub fn hilbert_sort(tiles: &mut [TileCoord]) {
    use crate::container::pmtiles::tile_id;
    tiles.sort_by_cached_key(|t| tile_id(t.z, t.x, t.y));
}

use crate::clip::{build_clip_chains, Box2};
use crate::config::SourceLayer;
use crate::feature_store::{FeatureRecord, Reader};
use crate::mvt::{encode_tile, ContourFeature, EXTENT};
use crate::simplify::simplify;
use crate::spatial_index::{query_rtree, FeatureBbox};
use rstar::{RTree, AABB};

/// Per-tile output: raw MVT bytes (uncompressed), or None if empty.
/// The pmtiles writer applies gzip compression automatically for MVT tiles.
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

    let clip = Box2 { min_x: min_lon, min_y: min_latp, max_x: max_lon, max_y: max_latp };

    // Quantization scales.
    let xscale = (max_lon - min_lon) / EXTENT as f64;
    let yscale = (max_latp - min_latp) / EXTENT as f64;

    // Sub-pixel simplification floor: even when the source layer doesn't simplify
    // at this zoom (e.g. z >= simplify_below), removing vertices that lie within
    // half an MVT pixel of the chord between their neighbours has no visual
    // effect — they all quantise to the same pixel — and saves real bytes.
    // xscale == yscale in (lon, latp) space because tile y is linear in latp.
    let subpixel_eps = 0.5 * xscale;

    // R-tree query: use the exact tile clip box so only features whose bbox
    // intersects the actual tile are included.
    let query = AABB::from_corners([min_lon as f32, min_latp as f32], [max_lon as f32, max_latp as f32]);
    let hits: Vec<&FeatureBbox> = query_rtree(rtree, query);
    if hits.is_empty() { return None; }

    // Per source-layer eps cache (one per unique simplify config encountered).
    let mut eps_cache: Vec<Option<f64>> = vec![None; sources.len()];

    // Group sub-linestrings by (elevation, flags) — all features in this tile
    // sharing the same attributes are packed into one MVT MultiLineString
    // feature. This dramatically reduces tile size for dense contour data
    // (each feature shares one tag table entry instead of one per sub-line).
    use std::collections::HashMap;
    let mut groups: HashMap<(i16, u8), Vec<Vec<(i32, i32)>>> = HashMap::new();
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

        // Compute / cache eps. Floor at half a pixel — anything finer collapses
        // to a single MVT pixel after quantisation anyway.
        let eps = match eps_cache[rec_layer_id as usize] {
            Some(v) => v,
            None => {
                let v = tile_eps_degp(
                    z, s.simplify_below, s.simplify_level, s.simplify_length,
                    s.simplify_ratio, tile_centre_latp).max(subpixel_eps);
                eps_cache[rec_layer_id as usize] = Some(v);
                v
            }
        };

        let coords = reader.coords_of(rec);

        // Fast path: if the feature's stored f32 bbox (expanded by slack to
        // cover possible f32 rounding) sits comfortably inside the tile's
        // f64 clip box, we know clipping would emit one chain == the whole
        // polyline. Skip outcode pass entirely. Significant at z=14 where
        // contour features are short and mostly fall inside a single tile.
        let fb_min_lon  = fb.min[0] as f64 - F32_BBOX_SLACK;
        let fb_max_lon  = fb.max[0] as f64 + F32_BBOX_SLACK;
        let fb_min_latp = fb.min[1] as f64 - F32_BBOX_SLACK;
        let fb_max_latp = fb.max[1] as f64 + F32_BBOX_SLACK;
        let fully_inside = fb_min_lon  >= min_lon
                        && fb_max_lon  <= max_lon
                        && fb_min_latp >= min_latp
                        && fb_max_latp <= max_latp;

        let chains: Vec<Vec<[f64; 2]>> = if fully_inside {
            vec![coords.to_vec()]
        } else {
            build_clip_chains(coords, &clip)
        };
        for chain in chains {
            // Simplify in (lon, latp) degp space. eps is always > 0 because of
            // the sub-pixel floor, so we always run DP.
            let pts: Vec<(f64, f64)> = chain.iter().map(|p| (p[0], p[1])).collect();
            let simplified: Vec<(f64, f64)> = simplify(&pts, eps);
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

            groups.entry((rec_elevation, rec_flags))
                .or_default()
                .push(points);
        }
    }

    if groups.is_empty() { return None; }

    // Materialise features in elevation order for deterministic output.
    let mut keys: Vec<(i16, u8)> = groups.keys().copied().collect();
    keys.sort();
    let features_out: Vec<ContourFeature> = keys.into_iter().map(|(elev, flags)| {
        let lines = groups.remove(&(elev, flags)).unwrap();
        ContourFeature {
            lines,
            elevation: elev as i32,
            main: (flags & 0b01) != 0,
            show: (flags & 0b10) != 0,
        }
    }).collect();

    let mvt = encode_tile(&features_out, output_layer_name, elevation_attr);
    Some(mvt) // raw MVT — pmtiles writer compresses
}

// Suppress unused-import warning if lat2latp isn't used yet.
const _: fn(f64) -> f64 = lat2latp;
