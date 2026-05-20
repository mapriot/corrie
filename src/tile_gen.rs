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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    }
    out
}

/// Same set of tiles as `enumerate_tiles` would produce, minus every (z,x,y)
/// whose extent contains no feature in `rtree`. Walks the tile space as a
/// quadtree: probe each tile with `contains_any`; if empty, prune its entire
/// subtree (4^(zmax-z) tiles skipped for free).
///
/// `bbox = [min_lon, min_lat, max_lon, max_lat]` in WGS84 degrees. zmin/zmax
/// inclusive on both ends.
pub fn enumerate_tiles_pruned(
    rtree: &rstar::RTree<crate::spatial_index::FeatureBbox>,
    bbox: [f64; 4],
    zmin: u8,
    zmax: u8,
) -> Vec<TileCoord> {
    // Start with all tiles at zmin intersecting bbox — reuse the existing
    // single-zoom enumeration to avoid duplicating bbox→tile math.
    let mut stack: Vec<TileCoord> = enumerate_tiles(bbox, zmin, zmin);
    let mut out: Vec<TileCoord> = Vec::new();

    while let Some(t) = stack.pop() {
        let bb = tile_to_aabb_f32(t);
        if !crate::spatial_index::contains_any(rtree, bb) {
            continue;
        }
        out.push(t);
        if t.z < zmax {
            let cx = t.x * 2;
            let cy = t.y * 2;
            stack.push(TileCoord { z: t.z + 1, x: cx,     y: cy });
            stack.push(TileCoord { z: t.z + 1, x: cx + 1, y: cy });
            stack.push(TileCoord { z: t.z + 1, x: cx,     y: cy + 1 });
            stack.push(TileCoord { z: t.z + 1, x: cx + 1, y: cy + 1 });
        }
    }
    out
}

fn tile_to_aabb_f32(t: TileCoord) -> rstar::AABB<[f32; 2]> {
    let min_lon  = tilex2lon(t.x, t.z);
    let max_lon  = tilex2lon(t.x + 1, t.z);
    let max_latp = tiley2latp(t.y, t.z);
    let min_latp = tiley2latp(t.y + 1, t.z);
    rstar::AABB::from_corners(
        [min_lon as f32, min_latp as f32],
        [max_lon as f32, max_latp as f32],
    )
}

/// In-place Hilbert-curve ordering. PMTiles requires directory entries to
/// be sorted by tile_id for `clustered=true`. Our `container::pmtiles::tile_id`
/// is cross-validated against the upstream crate in unit tests.
pub fn hilbert_sort(tiles: &mut [TileCoord]) {
    use crate::container::pmtiles::tile_id;
    tiles.sort_by_cached_key(|t| tile_id(t.z, t.x, t.y));
}

use crate::clip::{clip_chain_spans_into, Box2};
use crate::config::SourceLayer;
use crate::feature_store::{FeatureRecord, Reader};
use crate::mvt::{encode_tile, ContourFeature, EXTENT};
use crate::simplify::simplify_into;
use crate::spatial_index::FeatureBbox;
use rstar::{RTree, AABB};
use std::cell::RefCell;
use std::collections::HashMap;

/// Per-worker reusable scratch buffers. Each rayon worker keeps one of these
/// in a thread_local; process_tile borrows it for the whole call and clears
/// the length (capacity retained) between tiles.
#[derive(Default)]
struct TileScratch {
    clip: crate::clip::ClipScratch,
    pts_f64: Vec<(f64, f64)>,
    simplified: Vec<(f64, f64)>,
    dp_stack: Vec<(usize, usize)>,
    dp_kept: Vec<bool>,
    eps_cache: Vec<Option<f64>>,
    groups: HashMap<(i16, u8), Vec<Vec<(i32, i32)>>>,
}

thread_local! {
    static SCRATCH: RefCell<TileScratch> = RefCell::new(TileScratch::default());
}

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
    SCRATCH.with(|cell| {
        let mut s = cell.borrow_mut();
        let s = &mut *s;

        let z = tile.z;
        let min_lon  = tilex2lon(tile.x, z);
        let max_lon  = tilex2lon(tile.x + 1, z);
        let max_latp = tiley2latp(tile.y, z);
        let min_latp = tiley2latp(tile.y + 1, z);
        let tile_centre_latp = (min_latp + max_latp) * 0.5;
        let clip = Box2 { min_x: min_lon, min_y: min_latp, max_x: max_lon, max_y: max_latp };
        let xscale = (max_lon - min_lon) / EXTENT as f64;
        let yscale = (max_latp - min_latp) / EXTENT as f64;
        // Reciprocals for the quantize hot loop — multiply is 3-5× cheaper
        // than divide on arm64 and the loop runs per-vertex per-feature per-tile.
        let inv_xscale = 1.0 / xscale;
        let inv_yscale = 1.0 / yscale;
        let subpixel_eps = 0.5 * xscale;

        // R-tree query — stream directly, no Vec<&FeatureBbox> materialisation.
        let query = AABB::from_corners([min_lon as f32, min_latp as f32], [max_lon as f32, max_latp as f32]);
        let mut any_hits = false;

        // Reset per-tile scratch (length=0, capacity retained).
        s.eps_cache.clear();
        s.eps_cache.resize(sources.len(), None);
        s.groups.clear();

        let feature_recs: &[FeatureRecord] = reader.features();

        for fb in rtree.locate_in_envelope_intersecting(&query) {
            any_hits = true;
            let rec = &feature_recs[fb.idx as usize];
            let rec_layer_id = rec.layer_id;
            let rec_minzoom = rec.minzoom;
            let rec_elevation = rec.elevation;
            let rec_flags = rec.flags;

            let src = &sources[rec_layer_id as usize];
            if z < src.minzoom || z > src.maxzoom { continue; }
            if rec_minzoom > z { continue; }

            let eps = match s.eps_cache[rec_layer_id as usize] {
                Some(v) => v,
                None => {
                    let v = tile_eps_degp(
                        z, src.simplify_below, src.simplify_level, src.simplify_length,
                        src.simplify_ratio, tile_centre_latp).max(subpixel_eps);
                    s.eps_cache[rec_layer_id as usize] = Some(v);
                    v
                }
            };

            let coords = reader.coords_of(rec);

            // Fast path: if the feature's stored f32 bbox (expanded by
            // F32_BBOX_SLACK to absorb f32 rounding) sits inside the tile's
            // f64 clip box, clipping would emit a single chain == whole
            // polyline. Skip the outcode pass entirely.
            let fb_min_lon  = fb.min[0] as f64 - F32_BBOX_SLACK;
            let fb_max_lon  = fb.max[0] as f64 + F32_BBOX_SLACK;
            let fb_min_latp = fb.min[1] as f64 - F32_BBOX_SLACK;
            let fb_max_latp = fb.max[1] as f64 + F32_BBOX_SLACK;
            let fully_inside = fb_min_lon  >= min_lon
                            && fb_max_lon  <= max_lon
                            && fb_min_latp >= min_latp
                            && fb_max_latp <= max_latp;

            s.clip.spans.clear();
            if fully_inside {
                s.clip.spans.push((0, coords.len()));
            } else {
                clip_chain_spans_into(coords, &clip, &mut s.clip);
            }

            // Plain Vec snapshot of spans so we can release the borrow on s.clip
            // and mutate s.pts_f64 / s.simplified inside the loop.
            // spans are typically 1-3 entries — this allocation is negligible
            // compared to what we used to do per chain.
            let spans_snap: Vec<(usize, usize)> = s.clip.spans.iter().copied().collect();

            for (start, end) in spans_snap.into_iter() {
                let chain = &coords[start..end];

                s.pts_f64.clear();
                s.pts_f64.reserve(chain.len());
                for p in chain { s.pts_f64.push((p[0], p[1])); }

                s.simplified.clear();
                simplify_into(&s.pts_f64, eps, &mut s.dp_stack, &mut s.dp_kept, &mut s.simplified);
                if s.simplified.len() < 2 { continue; }

                // Quantize into a fresh Vec (will be moved into groups).
                let mut points: Vec<(i32, i32)> = Vec::with_capacity(s.simplified.len());
                for &(lon, latp) in &s.simplified {
                    let px = ((lon - min_lon) * inv_xscale).floor() as i32;
                    let py = ((max_latp - latp) * inv_yscale).floor() as i32;
                    if let Some(&last) = points.last() {
                        if last == (px, py) { continue; }
                    }
                    points.push((px, py));
                }
                if points.len() < 2 { continue; }

                s.groups.entry((rec_elevation, rec_flags))
                    .or_default()
                    .push(points);
            }
        }

        if !any_hits || s.groups.is_empty() { return None; }

        // Materialise features in elevation order for deterministic output.
        let mut keys: Vec<(i16, u8)> = s.groups.keys().copied().collect();
        keys.sort();
        let features_out: Vec<ContourFeature> = keys.into_iter().map(|(elev, flags)| {
            let lines = s.groups.remove(&(elev, flags)).unwrap();
            ContourFeature {
                lines,
                elevation: elev as i32,
                main: (flags & 0b01) != 0,
                show: (flags & 0b10) != 0,
            }
        }).collect();

        let mvt = encode_tile(&features_out, output_layer_name, elevation_attr);
        Some(mvt)
    })
}
