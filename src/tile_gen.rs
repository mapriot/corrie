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

/// Every tile at zooms `zmin..=zmax` that holds data in `packs` and lies in
/// a zmin tile intersecting `limit` (the config's `bounding_box`,
/// `[min_lon, min_lat, max_lon, max_lat]` in WGS84). Walks the tile space as
/// a quadtree from zmin: a tile without data prunes its whole subtree.
///
/// The zmin seeds include tiles that only touch the data's bounding box: a
/// line ending exactly on a tile edge still clips into the tile beyond, and
/// a run over some of the sources (one group of `--shards`) must visit the
/// same tiles as a run over all of them. Seeds never leave the zmin tiles
/// that intersect `limit`, whichever sources a run holds.
pub fn enumerate_tiles_pruned(packs: &[Pack], limit: [f64; 4], zmin: u8, zmax: u8) -> Vec<TileCoord> {
    let mut data = [u32::MAX, u32::MAX, 0, 0];
    for p in packs.iter().filter(|p| !p.is_empty()) {
        let b = p.bbox();
        data = [data[0].min(b[0]), data[1].min(b[1]), data[2].max(b[2]), data[3].max(b[3])];
    }
    if data[0] > data[2] { return Vec::new(); }
    // grid cells as degrees: a cell's lower edge, the next cell's lower edge
    let edge = |g: u32| g as f64 / 4_294_967_296.0 * 360.0 - 180.0;
    let (d_min_lon, d_min_latp) = (edge(data[0]), edge(data[1]));
    let (d_max_lon, d_max_latp) = (edge(data[2]) + 360.0 / 4_294_967_296.0, edge(data[3]) + 360.0 / 4_294_967_296.0);

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
        let q = tile_grid_box(t);
        if !packs.iter().any(|p| p.any(&q)) {
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

/// The grid box a tile covers, one cell wider on each side (see
/// [`grid_range`]): a chunk outside it cannot touch the tile.
pub fn tile_grid_box(t: TileCoord) -> GridBox {
    let (x0, x1) = grid_range(tilex2lon(t.x, t.z), tilex2lon(t.x + 1, t.z));
    let (y0, y1) = grid_range(tiley2latp(t.y + 1, t.z), tiley2latp(t.y, t.z));
    [x0, y0, x1, y1]
}

/// In-place Hilbert-curve ordering. PMTiles requires directory entries to
/// be sorted by tile_id for `clustered=true`. Our `container::pmtiles::tile_id`
/// is cross-validated against the upstream crate in unit tests.
pub fn hilbert_sort(tiles: &mut [TileCoord]) {
    use crate::container::pmtiles::tile_id;
    tiles.sort_by_cached_key(|t| tile_id(t.z, t.x, t.y));
}

use crate::attribute_rules::derive_attributes;
use crate::clip::{clip_chain_spans_into, Box2, ClipScratch};
use crate::config::SourceLayer;
use crate::mvt::{encode_tile, ContourFeature, EXTENT};
use crate::pack::{boxes_intersect, from_grid, grid_range, GridBox, Pack, CHUNK_SEGMENTS};
use crate::simplify::simplify_into;
use std::cell::RefCell;
use std::collections::HashMap;

/// Per-worker reusable scratch buffers. Each rayon worker keeps one of these
/// in a thread_local; process_tile borrows it for the whole call and clears
/// the length (capacity retained) between tiles.
#[derive(Default)]
struct TileScratch {
    /// (line, first vertex, chunk) index hits of one pack for the current
    /// tile, sorted: each line's chunks together, in order along the line.
    hits: Vec<(u32, u32, u32)>,
    /// Decoded vertices of one run of consecutive chunks.
    run: Vec<[f64; 2]>,
    clip: ClipScratch,
    pts_f64: Vec<(f64, f64)>,
    simplified: Vec<(f64, f64)>,
    dp_stack: Vec<(usize, usize)>,
    dp_kept: Vec<bool>,
    groups: HashMap<(i16, u8), Vec<Vec<(i32, i32)>>>,
}

thread_local! {
    static SCRATCH: RefCell<TileScratch> = RefCell::new(TileScratch::default());
}

/// Decode the chunks of one line that the index returned for a tile and call
/// `emit` with each chain of the line that touches the tile. `hits` are that
/// line's (line, first vertex, chunk) hits, sorted by first vertex.
///
/// Each run of consecutive chunks is decoded and clipped as one piece. A
/// chunk missing from the hits has no segment touching the tile, so a chain
/// never continues across a gap between runs: the chains are exactly those a
/// clip of the whole line would produce. With `whole_line` (the line lies
/// inside the tile, so every chunk is a hit) clipping is skipped.
pub fn clip_line_chunks(
    pack: &Pack,
    hits: &[(u32, u32, u32)],
    clip: &Box2,
    whole_line: bool,
    run: &mut Vec<[f64; 2]>,
    scratch: &mut ClipScratch,
    mut emit: impl FnMut(&[[f64; 2]]),
) {
    let mut k = 0;
    while k < hits.len() {
        run.clear();
        pack.decode_chunk(hits[k].2, false, run);
        let mut prev_first = hits[k].1;
        k += 1;
        while k < hits.len() && hits[k].1 == prev_first + CHUNK_SEGMENTS as u32 {
            pack.decode_chunk(hits[k].2, true, run);
            prev_first = hits[k].1;
            k += 1;
        }
        if whole_line {
            emit(run);
        } else {
            clip_chain_spans_into(run, clip, scratch);
            for &(a, b) in &scratch.spans { emit(&run[a..b]); }
        }
    }
}

/// Per-tile output: raw MVT bytes (uncompressed), or None if empty.
/// `packs[i]` holds the lines of `sources[i]`.
pub fn process_tile(
    tile: TileCoord,
    packs: &[Pack],
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
        let q = tile_grid_box(tile);

        s.groups.clear();
        for (pi, pack) in packs.iter().enumerate() {
            let src = &sources[pi];
            if z < src.minzoom || z > src.maxzoom { continue; }
            if !boxes_intersect(&pack.bbox(), &q) { continue; }

            let chunks = pack.chunks();
            let mut hits = std::mem::take(&mut s.hits);
            hits.clear();
            pack.query(&q, |ci| {
                let c = &chunks[ci as usize];
                hits.push((c.line, c.first_vertex, ci));
                true
            });
            if hits.is_empty() { s.hits = hits; continue; }
            hits.sort_unstable();

            let eps = tile_eps_degp(
                z, src.simplify_below, src.simplify_level, src.simplify_length,
                src.simplify_ratio, tile_centre_latp).max(subpixel_eps);
            let lines = pack.lines();
            let (mut run, mut clip_scratch) = (std::mem::take(&mut s.run), std::mem::take(&mut s.clip));

            let mut i = 0;
            while i < hits.len() {
                let line = hits[i].0;
                let mut j = i + 1;
                while j < hits.len() && hits[j].0 == line { j += 1; }
                let line_hits = &hits[i..j];
                i = j;

                let rec = &lines[line as usize];
                let (minzoom, flags) = derive_attributes(rec.elevation as i32);
                if minzoom > z { continue; }
                // Decoded vertices sit at grid cell centres, so the line's
                // box in degrees is exact: inside the tile means no clipping.
                let b = rec.bbox;
                let whole_line = from_grid(b[0]) >= min_lon && from_grid(b[2]) <= max_lon
                              && from_grid(b[1]) >= min_latp && from_grid(b[3]) <= max_latp;

                let groups = &mut s.groups;
                let (pts_f64, simplified) = (&mut s.pts_f64, &mut s.simplified);
                let (dp_stack, dp_kept) = (&mut s.dp_stack, &mut s.dp_kept);
                clip_line_chunks(pack, line_hits, &clip, whole_line, &mut run, &mut clip_scratch, |chain| {
                    pts_f64.clear();
                    pts_f64.extend(chain.iter().map(|p| (p[0], p[1])));
                    simplified.clear();
                    simplify_into(pts_f64, eps, dp_stack, dp_kept, simplified);
                    if simplified.len() < 2 { return; }

                    // Quantize into a fresh Vec (will be moved into groups).
                    let mut points: Vec<(i32, i32)> = Vec::with_capacity(simplified.len());
                    for &(lon, latp) in simplified.iter() {
                        let px = ((lon - min_lon) * inv_xscale).floor() as i32;
                        let py = ((max_latp - latp) * inv_yscale).floor() as i32;
                        if points.last() == Some(&(px, py)) { continue; }
                        points.push((px, py));
                    }
                    if points.len() < 2 { return; }
                    groups.entry((rec.elevation, flags)).or_default().push(points);
                });
            }
            s.run = run;
            s.clip = clip_scratch;
            s.hits = hits;
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
    use crate::pack::PackWriter;

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

    fn make_pack(lines: &[Vec<[f64; 2]>]) -> (tempfile::TempDir, Pack) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.cpk");
        let mut w = PackWriter::create(&p).unwrap();
        for l in lines { w.add_line(l, 100).unwrap(); }
        w.finish(&serde_json::json!({})).unwrap();
        let pack = Pack::open(&p).unwrap();
        (dir, pack)
    }

    /// Decoding and clipping only the chunk runs the index returns gives
    /// exactly the chains of decoding and clipping each whole line, also for
    /// lines that leave and re-enter the tile and vertices on tile edges.
    #[test]
    fn chunk_runs_clip_like_the_whole_line() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let tile = TileCoord { z: 13, x: 4440, y: 2830 };
        let b = tile_box(tile);
        let (w, h) = (b.max_x - b.min_x, b.max_y - b.min_y);
        let q = tile_grid_box(tile);
        let mut scratch = ClipScratch::default();
        let (mut run, mut whole) = (Vec::new(), Vec::new());
        let (mut checked, mut checked_multi) = (0, 0);
        for batch in 0..30 {
            let lines: Vec<Vec<[f64; 2]>> = (0..2000).map(|case| {
                let n = 2 + (rng.next() % 1200) as usize;
                // A random walk around the tile, some steps big enough to leave it.
                let (mut x, mut y) = (b.min_x + rng.unit() * w, b.min_y + rng.unit() * h);
                let step = w * [0.01, 0.1, 0.5][(batch + case) % 3];
                (0..n).map(|_| {
                    x += (rng.unit() - 0.5) * step;
                    y += (rng.unit() - 0.5) * step;
                    match rng.next() % 20 {
                        0 => [if rng.next() % 2 == 0 { b.min_x } else { b.max_x }, y],
                        1 => [x, if rng.next() % 2 == 0 { b.min_y } else { b.max_y }],
                        _ => [x, y],
                    }
                }).collect()
            }).collect();
            let (_d, pack) = make_pack(&lines);

            let chunks = pack.chunks();
            let mut hits: Vec<(u32, u32, u32)> = Vec::new();
            pack.query(&q, |ci| { let c = &chunks[ci as usize]; hits.push((c.line, c.first_vertex, ci)); true });
            hits.sort_unstable();
            for li in 0..pack.lines().len() as u32 {
                whole.clear();
                pack.decode_line(li, &mut whole);
                clip_chain_spans_into(&whole, &b, &mut scratch);
                let want: Vec<Vec<[f64; 2]>> = scratch.spans.iter().map(|&(a, e)| whole[a..e].to_vec()).collect();

                let line_hits: Vec<(u32, u32, u32)> = hits.iter().copied().filter(|h| h.0 == li).collect();
                let mut got: Vec<Vec<[f64; 2]>> = Vec::new();
                clip_line_chunks(&pack, &line_hits, &b, false, &mut run, &mut scratch, |c| got.push(c.to_vec()));
                assert_eq!(got, want, "batch {batch} line {li}");
                checked += 1;
                let line_chunks = (whole.len() - 1).div_ceil(CHUNK_SEGMENTS);
                if line_chunks >= 3 && !line_hits.is_empty() && line_hits.len() < line_chunks {
                    checked_multi += 1;
                }
            }
        }
        assert_eq!(checked, 60_000);
        assert!(checked_multi > 1000, "too few cases with gaps between chunk runs: {checked_multi}");
    }

    /// A line inside the tile is emitted whole, without clipping.
    #[test]
    fn whole_line_inside_the_tile_is_emitted_unclipped() {
        let tile = TileCoord { z: 13, x: 4440, y: 2830 };
        let b = tile_box(tile);
        let line: Vec<[f64; 2]> = (0..400).map(|i| {
            let t = i as f64 / 400.0;
            [b.min_x + (b.max_x - b.min_x) * (0.2 + 0.6 * t), b.min_y + (b.max_y - b.min_y) * (0.5 + 0.2 * (t * 20.0).sin())]
        }).collect();
        let (_d, pack) = make_pack(&[line]);
        let mut hits = Vec::new();
        let chunks = pack.chunks();
        pack.query(&tile_grid_box(tile), |ci| { let c = &chunks[ci as usize]; hits.push((c.line, c.first_vertex, ci)); true });
        hits.sort_unstable();
        let mut all = Vec::new();
        pack.decode_line(0, &mut all);
        let (mut run, mut scratch, mut got) = (Vec::new(), ClipScratch::default(), Vec::new());
        clip_line_chunks(&pack, &hits, &b, true, &mut run, &mut scratch, |c| got.push(c.to_vec()));
        assert_eq!(got, vec![all]);
    }

    /// A run over only some groups of sources visits the same tiles as a run
    /// over all of them: a tile that only touches the data at its west or
    /// north edge is still seeded.
    #[test]
    fn pruned_enumeration_includes_tiles_touching_the_data_edge() {
        let t = TileCoord { z: 11, x: 1100, y: 690 };
        let edge_lon = tilex2lon(t.x, t.z); // west edge of t = east edge of x-1
        let edge_latp = tiley2latp(t.y, t.z); // north edge of t = south edge of y-1
        let mid_latp = (tiley2latp(t.y, t.z) + tiley2latp(t.y + 1, t.z)) / 2.0;
        let mid_lon = (tilex2lon(t.x, t.z) + tilex2lon(t.x + 1, t.z)) / 2.0;
        let world = [-180.0, -85.0, 180.0, 85.0];
        // Starts exactly on the west edge and runs east.
        let (_d, p) = make_pack(&[vec![[edge_lon, mid_latp], [mid_lon, mid_latp]]]);
        let tiles = enumerate_tiles_pruned(std::slice::from_ref(&p), world, 11, 11);
        assert!(tiles.contains(&TileCoord { z: 11, x: t.x - 1, y: t.y }), "{tiles:?}");
        // Starts exactly on the north edge and runs south.
        let (_d, p) = make_pack(&[vec![[mid_lon, edge_latp], [mid_lon, mid_latp]]]);
        let tiles = enumerate_tiles_pruned(std::slice::from_ref(&p), world, 11, 11);
        assert!(tiles.contains(&TileCoord { z: 11, x: t.x, y: t.y - 1 }), "{tiles:?}");
    }

    /// Tiles outside the zmin tiles that intersect the config box are never
    /// produced, even when the data continues past the box's edge and that
    /// edge lies exactly on a tile edge.
    #[test]
    fn pruned_enumeration_stays_inside_the_config_box() {
        let edge_lon = tilex2lon(1100, 11);
        let latp = (tiley2latp(690, 11) + tiley2latp(691, 11)) / 2.0;
        let (_d, p) = make_pack(&[vec![[edge_lon - 0.5, latp], [edge_lon + 0.5, latp]]]);
        let limit = [edge_lon, -85.0, 180.0, 85.0];
        let tiles = enumerate_tiles_pruned(std::slice::from_ref(&p), limit, 11, 13);
        assert!(!tiles.is_empty());
        assert!(tiles.iter().all(|t| tilex2lon(t.x, t.z) >= edge_lon), "a tile west of the box: {tiles:?}");
    }

    #[test]
    fn no_packs_no_tiles() {
        assert!(enumerate_tiles_pruned(&[], [-180.0, -85.0, 180.0, 85.0], 0, 5).is_empty());
    }
}
