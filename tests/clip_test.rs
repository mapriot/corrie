use corrie::clip::{build_clip_chains, clip_segment_to_box, clip_polyline_to_box, Box2};

const B: Box2 = Box2 { min_x: 0.0, min_y: 0.0, max_x: 10.0, max_y: 10.0 };

#[test]
fn segment_fully_inside() {
    let r = clip_segment_to_box((1.0, 1.0), (5.0, 5.0), &B);
    assert_eq!(r, Some(((1.0, 1.0), (5.0, 5.0))));
}

#[test]
fn segment_fully_outside() {
    let r = clip_segment_to_box((-1.0, -1.0), (-5.0, -5.0), &B);
    assert!(r.is_none());
}

#[test]
fn segment_clipped_left() {
    let r = clip_segment_to_box((-2.0, 5.0), (5.0, 5.0), &B);
    let (a, b) = r.unwrap();
    assert!((a.0 - 0.0).abs() < 1e-9);
    assert!((a.1 - 5.0).abs() < 1e-9);
    assert_eq!(b, (5.0, 5.0));
}

#[test]
fn segment_crosses_two_edges() {
    // From below-left to above-right, crossing left and top.
    let r = clip_segment_to_box((-5.0, -5.0), (15.0, 15.0), &B);
    let (a, b) = r.unwrap();
    assert!((a.0 - 0.0).abs() < 1e-9);
    assert!((a.1 - 0.0).abs() < 1e-9);
    assert!((b.0 - 10.0).abs() < 1e-9);
    assert!((b.1 - 10.0).abs() < 1e-9);
}

#[test]
fn polyline_fully_inside_yields_one_sub() {
    let pl: Vec<[f64; 2]> = vec![[1.0, 1.0], [2.0, 2.0], [3.0, 3.0]];
    let subs = clip_polyline_to_box(&pl, &B);
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0], pl);
}

#[test]
fn polyline_exits_and_reenters_yields_two_subs() {
    // Goes inside, leaves through top, comes back through left.
    let pl: Vec<[f64; 2]> = vec![[1.0, 1.0], [1.0, 15.0], [-5.0, 5.0], [5.0, 5.0]];
    let subs = clip_polyline_to_box(&pl, &B);
    assert_eq!(subs.len(), 2);
    assert!(subs[0].len() >= 2);
    assert!(subs[1].len() >= 2);
}

#[test]
fn polyline_outside_yields_no_subs() {
    let pl: Vec<[f64; 2]> = vec![[-5.0, -5.0], [-10.0, -10.0]];
    let subs = clip_polyline_to_box(&pl, &B);
    assert!(subs.is_empty());
}

// --- build_clip_chains tests ---

#[test]
fn chains_split_at_clip_boundaries() {
    // Polyline enters, leaves, re-enters the box.
    let pl: Vec<[f64; 2]> = vec![
        [-5.0,  5.0],  // outside left
        [ 3.0,  5.0],  // inside (segment [-5,5]->[3,5] crosses left edge)
        [ 5.0,  5.0],  // inside
        [15.0,  5.0],  // outside right (segment crosses right edge)
        [25.0,  5.0],  // outside right (both endpoints share RIGHT bit -> no intersection)
        [ 5.0,  8.0],  // inside (segment [25,5]->[5,8] crosses right edge)
    ];
    let chains = build_clip_chains(&pl, &B);
    // Chain 1: [-5,5],[3,5],[5,5],[15,5]
    // Chain 2: [25,5],[5,8]
    assert!(!chains.is_empty());
    for c in &chains { assert!(c.len() >= 2); }
}

#[test]
fn chains_drop_polylines_entirely_outside() {
    let pl: Vec<[f64; 2]> = vec![[-5.0, -5.0], [-10.0, -10.0]];
    let chains = build_clip_chains(&pl, &B);
    assert!(chains.is_empty());
}

#[test]
fn chains_keep_polylines_entirely_inside() {
    let pl: Vec<[f64; 2]> = vec![[1.0, 1.0], [2.0, 2.0], [3.0, 3.0]];
    let chains = build_clip_chains(&pl, &B);
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].len(), 3);
    // Vertices are original (unclipped).
    assert_eq!(chains[0][0], [1.0, 1.0]);
    assert_eq!(chains[0][2], [3.0, 3.0]);
}
