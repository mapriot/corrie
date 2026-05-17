//! Static R-tree over feature bboxes. Built once at ingest, mmap-loadable
//! (via serde + bincode) at tile time.

use rstar::{RTree, RTreeObject, AABB};
use serde::{Deserialize, Serialize};

/// One R-tree entry: bbox in (lon, latp) f32 + global feature index.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FeatureBbox {
    pub idx: u32,
    pub min: [f32; 2],
    pub max: [f32; 2],
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

/// Serialize the R-tree to bytes (bincode). Read back via `deserialize_rtree`.
pub fn serialize_rtree(tree: &RTree<FeatureBbox>) -> anyhow::Result<Vec<u8>> {
    let bytes = bincode::serialize(tree)?;
    Ok(bytes)
}

pub fn deserialize_rtree(bytes: &[u8]) -> anyhow::Result<RTree<FeatureBbox>> {
    let tree: RTree<FeatureBbox> = bincode::deserialize(bytes)?;
    Ok(tree)
}
