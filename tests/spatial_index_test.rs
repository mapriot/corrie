use corrie::spatial_index::{FeatureBbox, build_rtree, query_rtree};
use rstar::AABB;

#[test]
fn build_and_query() {
    let boxes = vec![
        FeatureBbox { idx: 0, min: [0.0, 0.0], max: [1.0, 1.0] },
        FeatureBbox { idx: 1, min: [2.0, 2.0], max: [3.0, 3.0] },
        FeatureBbox { idx: 2, min: [0.5, 0.5], max: [2.5, 2.5] },
    ];
    let tree = build_rtree(boxes);

    let hits = query_rtree(&tree, AABB::from_corners([0.4, 0.4], [0.6, 0.6]));
    let mut indices: Vec<u32> = hits.iter().map(|f| f.idx).collect();
    indices.sort();
    assert_eq!(indices, vec![0, 2]);

    let none = query_rtree(&tree, AABB::from_corners([10.0, 10.0], [11.0, 11.0]));
    assert!(none.is_empty());
}
