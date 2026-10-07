//! Static R-tree over polyline *chunks*. Built once at ingest, loaded (via
//! serde + bincode) at tile time.
//!
//! Each polyline is indexed as runs of `CHUNK_SEGMENTS` segments, one entry
//! per run, rather than one entry per polyline. Contour lines are long (the
//! longest 1% hold ~70% of all vertices) and their bboxes cover thousands of
//! z14 tiles; indexing whole polylines made every one of those tiles scan
//! every vertex of the line. With chunks a tile only touches the runs whose
//! bbox reaches it.

use rstar::{RTree, RTreeObject, AABB};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Segments per indexed chunk. Chunk `k` of a polyline covers vertices
/// `k*CHUNK_SEGMENTS ..= (k+1)*CHUNK_SEGMENTS` (clamped to the last vertex),
/// so consecutive chunks share one vertex and every segment belongs to
/// exactly one chunk.
pub const CHUNK_SEGMENTS: usize = 128;

/// One R-tree entry: bbox in (lon, latp) f32 of one chunk of a polyline.
/// The bbox is rounded outward from f64, so it always contains the chunk.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FeatureBbox {
    /// Global feature index.
    pub idx: u32,
    /// Chunk number within the feature.
    pub chunk: u32,
    pub min: [f32; 2],
    pub max: [f32; 2],
}

/// Vertex range `[start, end)` of chunk `chunk` in a polyline of `n_vertices`.
#[inline]
pub fn chunk_vertex_range(chunk: u32, n_vertices: usize) -> (usize, usize) {
    let start = chunk as usize * CHUNK_SEGMENTS;
    let end = (start + CHUNK_SEGMENTS + 1).min(n_vertices);
    (start, end)
}

/// Append one entry per chunk of `coords` (a polyline of >= 2 vertices).
pub fn push_chunk_bboxes(idx: u32, coords: &[[f64; 2]], out: &mut Vec<FeatureBbox>) {
    let n = coords.len();
    if n < 2 { return; }
    let n_chunks = (n - 1).div_ceil(CHUNK_SEGMENTS);
    for chunk in 0..n_chunks as u32 {
        let (start, end) = chunk_vertex_range(chunk, n);
        let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
        let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in &coords[start..end] {
            mnx = mnx.min(c[0]); mny = mny.min(c[1]);
            mxx = mxx.max(c[0]); mxy = mxy.max(c[1]);
        }
        out.push(FeatureBbox {
            idx,
            chunk,
            min: [f32_down(mnx), f32_down(mny)],
            max: [f32_up(mxx), f32_up(mxy)],
        });
    }
}

/// Largest f32 <= `v`.
#[inline]
pub fn f32_down(v: f64) -> f32 {
    let f = v as f32;
    if (f as f64) <= v { return f; }
    let b = f.to_bits();
    f32::from_bits(if f > 0.0 { b - 1 } else if f < 0.0 { b + 1 } else { 0x8000_0001 })
}

/// Smallest f32 >= `v`.
#[inline]
pub fn f32_up(v: f64) -> f32 {
    let f = v as f32;
    if (f as f64) >= v { return f; }
    let b = f.to_bits();
    f32::from_bits(if f > 0.0 { b + 1 } else if f < 0.0 { b - 1 } else { 0x0000_0001 })
}

impl RTreeObject for FeatureBbox {
    type Envelope = AABB<[f32; 2]>;
    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(self.min, self.max)
    }
}

/// Build a static R-tree with bulk load (Hilbert-style packing).
pub fn build_rtree(boxes: Vec<FeatureBbox>) -> RTree<FeatureBbox> {
    RTree::bulk_load(boxes)
}

/// Return all features whose bbox intersects `query`.
pub fn query_rtree<'a>(
    tree: &'a RTree<FeatureBbox>,
    query: AABB<[f32; 2]>,
) -> Vec<&'a FeatureBbox> {
    tree.locate_in_envelope_intersecting(&query).collect()
}

/// Streams through a buffered file: the planet tree is ~20 GB, and a full
/// byte copy next to the tree would double peak memory.
pub fn save_rtree(tree: &RTree<FeatureBbox>, dir: &Path) -> anyhow::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("rtree.bin"))?);
    bincode::serialize_into(&mut w, tree)?;
    w.flush()?;
    Ok(())
}

pub fn load_rtree(dir: &Path) -> anyhow::Result<RTree<FeatureBbox>> {
    let r = std::io::BufReader::new(std::fs::File::open(dir.join("rtree.bin"))?);
    Ok(bincode::deserialize_from(r)?)
}

/// Union bounding box of every feature in the tree, in `(lon, latp)` space.
/// Returns `None` for an empty tree. Reads directly from the root node — O(1).
pub fn rtree_bbox(tree: &RTree<FeatureBbox>) -> Option<(f64, f64, f64, f64)> {
    if tree.size() == 0 { return None; }
    let env = tree.root().envelope();
    let lo = env.lower();
    let hi = env.upper();
    Some((lo[0] as f64, lo[1] as f64, hi[0] as f64, hi[1] as f64))
}

/// Single-probe "is any feature in this envelope?". Faster than `query_rtree`
/// because it doesn't materialise a Vec — `Iterator::next().is_some()`
/// short-circuits on the first hit.
pub fn contains_any(tree: &RTree<FeatureBbox>, query: AABB<[f32; 2]>) -> bool {
    tree.locate_in_envelope_intersecting(&query).next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_rounding_brackets_the_f64() {
        for &v in &[0.0, 1e-30, -1e-30, 0.1, -0.1, 14.123456789012, -179.99999999999, 85.0511287798, 1e-46, -1e-46] {
            assert!((f32_down(v) as f64) <= v, "down {v}");
            assert!((f32_up(v) as f64) >= v, "up {v}");
            // Tight: up and down are the same f32 or adjacent ones.
            let (d, u) = (f32_down(v), f32_up(v));
            let steps = if d >= 0.0 && u >= 0.0 { u.to_bits() - d.to_bits() }
                        else if d < 0.0 && u <= 0.0 { d.to_bits() - u.to_bits() }
                        else { 1 }; // straddles zero: -min_subnormal .. 0 or 0 .. min_subnormal
            assert!(steps <= 1, "tight {v}: {d} .. {u}");
        }
    }

    #[test]
    fn chunks_cover_every_segment_once() {
        let n = CHUNK_SEGMENTS * 3 + 5;
        let coords: Vec<[f64; 2]> = (0..n).map(|i| [i as f64 * 0.001, (i as f64 * 0.37).sin()]).collect();
        let mut out = Vec::new();
        push_chunk_bboxes(7, &coords, &mut out);
        assert_eq!(out.len(), 4);
        let mut covered = vec![0u32; n - 1];
        for e in &out {
            assert_eq!(e.idx, 7);
            let (s, t) = chunk_vertex_range(e.chunk, n);
            for seg in s..t - 1 { covered[seg] += 1; }
            for c in &coords[s..t] {
                assert!(e.min[0] as f64 <= c[0] && c[0] <= e.max[0] as f64);
                assert!(e.min[1] as f64 <= c[1] && c[1] <= e.max[1] as f64);
            }
        }
        assert!(covered.iter().all(|&k| k == 1));
    }

    #[test]
    fn two_vertex_line_is_one_chunk() {
        let mut out = Vec::new();
        push_chunk_bboxes(0, &[[0.0, 0.0], [1.0, 1.0]], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(chunk_vertex_range(0, 2), (0, 2));
    }
}
