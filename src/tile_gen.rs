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

/// Every tile at zooms `zmin..=zmax` that holds data in `rtree` and lies in
/// a zmin tile intersecting `limit` (the config's `bounding_box`,
/// `[min_lon, min_lat, max_lon, max_lat]` in WGS84). Walks the tile space as
/// a quadtree from zmin: a tile without data prunes its whole subtree.
///
/// The zmin seeds include tiles that only touch the data's bounding box: a
/// line ending exactly on a tile edge still clips into the tile beyond, and
/// a run over some of the sources (one group of `--shards`) must visit the
/// same tiles as a run over all of them. Seeds never leave the zmin tiles
/// that intersect `limit`, whichever sources a run holds.
pub fn enumerate_tiles_pruned(
    rtree: &rstar::RTree<crate::spatial_index::FeatureBbox>,
    limit: [f64; 4],
    zmin: u8,
    zmax: u8,
) -> Vec<TileCoord> {
    let Some((d_min_lon, d_min_latp, d_max_lon, d_max_latp)) = crate::spatial_index::rtree_bbox(rtree) else {
        return Vec::new();
    };
    let n = 1i64 << zmin;
    let latp_max = tiley2latp(0, zmin);
    let fx = |lon: f64| (lon + 180.0) / 360.0 * n as f64;
    let fy = |latp: f64| (latp_max - latp) / (2.0 * latp_max) * n as f64;
    let clamp = |v: f64| (v as i64).clamp(0, n - 1);
    // zmin tiles whose area intersects the config box (a tile that only
    // touches its edge is outside)
    let (lx0, lx1) = (clamp(fx(limit[0]).floor()), clamp(fx(limit[2]).ceil() - 1.0));
    let (ly0, ly1) = (clamp(fy(lat2latp(limit[3])).floor()), clamp(fy(lat2latp(limit[1])).ceil() - 1.0));
    // zmin tiles that intersect or touch the data
    let (dx0, dx1) = (clamp(fx(d_min_lon).ceil() - 1.0), clamp(fx(d_max_lon).floor()));
    let (dy0, dy1) = (clamp(fy(d_max_latp).ceil() - 1.0), clamp(fy(d_min_latp).floor()));
    let (x0, x1, y0, y1) = (lx0.max(dx0), lx1.min(dx1), ly0.max(dy0), ly1.min(dy1));

    let mut stack: Vec<TileCoord> = Vec::new();
    for x in x0..=x1 {
        for y in y0..=y1 {
            stack.push(TileCoord { z: zmin, x: x as u32, y: y as u32 });
        }
    }
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
    query_aabb(min_lon, min_latp, max_lon, max_latp)
}

/// f32 query box that contains the f64 box (rounded outward), so a chunk
/// touching the tile edge is never lost to f32 rounding.
fn query_aabb(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> rstar::AABB<[f32; 2]> {
    use crate::spatial_index::{f32_down, f32_up};
    rstar::AABB::from_corners([f32_down(min_x), f32_down(min_y)], [f32_up(max_x), f32_up(max_y)])
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
use crate::spatial_index::{chunk_vertex_range, FeatureBbox};
use rstar::RTree;
use std::cell::RefCell;
use std::collections::HashMap;

/// Per-worker reusable scratch buffers. Each rayon worker keeps one of these
/// in a thread_local; process_tile borrows it for the whole call and clears
/// the length (capacity retained) between tiles.
#[derive(Default)]
struct TileScratch {
    /// (feature idx, chunk) R-tree hits for the current tile, sorted.
    hits: Vec<(u32, u32)>,
    /// Clipped spans of the current feature, as vertex ranges into its coords.
    spans: Vec<(usize, usize)>,
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

/// Clip the chunks of one polyline that the index returned for a tile.
/// `hits` are that polyline's (feature, chunk) hits sorted by chunk; the
/// clipped spans are appended to `spans` as vertex ranges into `coords`.
///
/// Each run of consecutive chunks is clipped as one piece. A chunk missing
/// from the hits has no segment touching the tile, so a chain never
/// continues across a gap between runs: the spans are exactly those a clip
/// of the whole polyline would produce.
pub fn clip_chunk_runs(
    coords: &[[f64; 2]],
    hits: &[(u32, u32)],
    clip: &Box2,
    scratch: &mut crate::clip::ClipScratch,
    spans: &mut Vec<(usize, usize)>,
) {
    let n = coords.len();
    let mut k = 0;
    while k < hits.len() {
        let run_first = hits[k].1;
        let mut run_last = run_first;
        k += 1;
        while k < hits.len() && hits[k].1 == run_last + 1 { run_last += 1; k += 1; }
        let (v0, _) = chunk_vertex_range(run_first, n);
        let (_, v1) = chunk_vertex_range(run_last, n);
        clip_chain_spans_into(&coords[v0..v1], clip, scratch);
        spans.extend(scratch.spans.iter().map(|&(a, b)| (v0 + a, v0 + b)));
    }
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

        // R-tree query → (feature, chunk) hits. Sorting puts each feature's
        // chunks next to each other, in order along the line.
        let query = query_aabb(min_lon, min_latp, max_lon, max_latp);
        s.hits.clear();
        s.hits.extend(rtree.locate_in_envelope_intersecting(&query).map(|fb| (fb.idx, fb.chunk)));
        if s.hits.is_empty() { return None; }
        s.hits.sort_unstable();

        // Reset per-tile scratch (length=0, capacity retained).
        s.eps_cache.clear();
        s.eps_cache.resize(sources.len(), None);
        s.groups.clear();

        let feature_recs: &[FeatureRecord] = reader.features();

        let mut i = 0;
        while i < s.hits.len() {
            let idx = s.hits[i].0;
            let mut j = i + 1;
            while j < s.hits.len() && s.hits[j].0 == idx { j += 1; }
            let (first_hit, end_hit) = (i, j);
            i = j;

            let rec = &feature_recs[idx as usize];
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
            let n = coords.len();

            // Fast path: if the feature's stored f32 bbox (expanded by
            // F32_BBOX_SLACK to absorb f32 rounding) sits inside the tile's
            // f64 clip box, clipping would emit a single chain == whole
            // polyline. Skip the outcode pass entirely.
            let fully_inside = rec.bbox_min_lon  as f64 - F32_BBOX_SLACK >= min_lon
                            && rec.bbox_max_lon  as f64 + F32_BBOX_SLACK <= max_lon
                            && rec.bbox_min_latp as f64 - F32_BBOX_SLACK >= min_latp
                            && rec.bbox_max_latp as f64 + F32_BBOX_SLACK <= max_latp;

            s.spans.clear();
            if fully_inside {
                s.spans.push((0, n));
            } else {
                clip_chunk_runs(coords, &s.hits[first_hit..end_hit], &clip, &mut s.clip, &mut s.spans);
            }

            for si in 0..s.spans.len() {
                let (start, end) = s.spans[si];
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

        if s.groups.is_empty() { return None; }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::ClipScratch;
    use crate::spatial_index::{build_rtree, push_chunk_bboxes, FeatureBbox, CHUNK_SEGMENTS};
    use rstar::Envelope;

    /// Small deterministic generator (xorshift64*), no extra dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12; self.0 ^= self.0 << 25; self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545F4914F6CDD1D)
        }
        fn unit(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
    }

    fn tile_box(t: TileCoord) -> Box2 {
        Box2 {
            min_x: tilex2lon(t.x, t.z), max_x: tilex2lon(t.x + 1, t.z),
            min_y: tiley2latp(t.y + 1, t.z), max_y: tiley2latp(t.y, t.z),
        }
    }

    /// Clipping only the chunk runs the index returns gives exactly the spans
    /// of clipping the whole polyline, including lines that leave and
    /// re-enter the tile, vertices exactly on tile edges, and f32-exact
    /// coordinates.
    #[test]
    fn chunk_runs_clip_like_the_whole_polyline() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let tile = TileCoord { z: 13, x: 4440, y: 2830 };
        let b = tile_box(tile);
        let (w, h) = (b.max_x - b.min_x, b.max_y - b.min_y);
        let query = query_aabb(b.min_x, b.min_y, b.max_x, b.max_y);
        let mut scratch = ClipScratch::default();
        let (mut whole, mut runs) = (Vec::new(), Vec::new());
        let mut checked_multi = 0;
        for case in 0..60_000 {
            let n = 2 + (rng.next() % 1200) as usize;
            // A random walk around the tile, some steps big enough to leave it.
            let (mut x, mut y) = (b.min_x + rng.unit() * w, b.min_y + rng.unit() * h);
            let step = w * [0.01, 0.1, 0.5][case % 3];
            let mut coords = Vec::with_capacity(n);
            for _ in 0..n {
                x += (rng.unit() - 0.5) * step;
                y += (rng.unit() - 0.5) * step;
                let mut p = [x, y];
                match rng.next() % 20 {
                    0 => p[0] = if rng.next() % 2 == 0 { b.min_x } else { b.max_x },
                    1 => p[1] = if rng.next() % 2 == 0 { b.min_y } else { b.max_y },
                    2 => p = [p[0] as f32 as f64, p[1] as f32 as f64],
                    _ => {}
                }
                coords.push(p);
            }

            clip_chain_spans_into(&coords, &b, &mut scratch);
            whole.clear();
            whole.extend_from_slice(&scratch.spans);

            let mut chunks = Vec::new();
            push_chunk_bboxes(0, &coords, &mut chunks);
            let hits: Vec<(u32, u32)> = chunks.iter()
                .filter(|c: &&FeatureBbox| rstar::RTreeObject::envelope(*c).intersects(&query))
                .map(|c| (c.idx, c.chunk))
                .collect();
            runs.clear();
            clip_chunk_runs(&coords, &hits, &b, &mut scratch, &mut runs);
            assert_eq!(whole, runs, "case {case}: n={n}");
            if n > CHUNK_SEGMENTS * 2 && hits.len() > 1 && hits.len() < chunks.len() { checked_multi += 1; }
        }
        assert!(checked_multi > 1000, "too few cases with gaps between chunk runs: {checked_multi}");
    }

    /// A run over only some groups of sources visits the same tiles as a run
    /// over all of them: a tile that only touches the data at its west or
    /// north edge is still seeded.
    #[test]
    fn pruned_enumeration_includes_tiles_touching_the_data_edge() {
        let t = TileCoord { z: 11, x: 1100, y: 690 };
        let edge_lon = tilex2lon(t.x, t.z); // west edge of t = east edge of x-1
        let edge_latp = tiley2latp(t.y, t.z); // north edge of t = south edge of y-1
        assert_eq!(edge_lon as f32 as f64, edge_lon);
        let mid_latp = (tiley2latp(t.y, t.z) + tiley2latp(t.y + 1, t.z)) / 2.0;
        let mid_lon = (tilex2lon(t.x, t.z) + tilex2lon(t.x + 1, t.z)) / 2.0;
        let line = |a: [f64; 2], b: [f64; 2]| {
            let mut v = Vec::new();
            push_chunk_bboxes(0, &[a, b], &mut v);
            build_rtree(v)
        };
        let world = [-180.0, -85.0, 180.0, 85.0];
        // Starts exactly on the west edge and runs east.
        let rt = line([edge_lon, mid_latp], [mid_lon, mid_latp]);
        let tiles = enumerate_tiles_pruned(&rt, world, 11, 11);
        assert!(tiles.contains(&TileCoord { z: 11, x: t.x - 1, y: t.y }), "{tiles:?}");
        // Starts exactly on the north edge and runs south.
        let rt = line([mid_lon, edge_latp], [mid_lon, mid_latp]);
        let tiles = enumerate_tiles_pruned(&rt, world, 11, 11);
        assert!(tiles.contains(&TileCoord { z: 11, x: t.x, y: t.y - 1 }), "{tiles:?}");
    }

    /// Tiles outside the zmin tiles that intersect the config box are never
    /// produced, even when the data continues past the box's edge and that
    /// edge lies exactly on a tile edge.
    #[test]
    fn pruned_enumeration_stays_inside_the_config_box() {
        let edge_lon = tilex2lon(1100, 11);
        let latp = (tiley2latp(690, 11) + tiley2latp(691, 11)) / 2.0;
        let mut v = Vec::new();
        push_chunk_bboxes(0, &[[edge_lon - 0.5, latp], [edge_lon + 0.5, latp]], &mut v);
        let rt = build_rtree(v);
        let limit = [edge_lon, -85.0, 180.0, 85.0];
        let tiles = enumerate_tiles_pruned(&rt, limit, 11, 13);
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|t| tilex2lon(t.x, t.z) >= edge_lon), "a tile west of the box: {tiles:?}");
    }
}
