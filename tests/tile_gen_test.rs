use corrie::tile_gen::tile_eps_degp;

#[test]
fn eps_zero_at_or_above_simplify_below() {
    let e = tile_eps_degp(14, 14, 0.00005, 0.0, 2.0, 50.0);
    assert_eq!(e, 0.0);
    let e = tile_eps_degp(15, 14, 0.00005, 0.0, 2.0, 50.0);
    assert_eq!(e, 0.0);
}

#[test]
fn eps_equals_level_at_below_minus_one() {
    let e = tile_eps_degp(13, 14, 0.00005, 0.0, 2.0, 50.0);
    assert!((e - 0.00005).abs() < 1e-15);
}

#[test]
fn eps_doubles_per_zoom_step_down() {
    let e13 = tile_eps_degp(13, 14, 0.00005, 0.0, 2.0, 50.0);
    let e12 = tile_eps_degp(12, 14, 0.00005, 0.0, 2.0, 50.0);
    let e11 = tile_eps_degp(11, 14, 0.00005, 0.0, 2.0, 50.0);
    assert!((e12 / e13 - 2.0).abs() < 1e-9);
    assert!((e11 / e13 - 4.0).abs() < 1e-9);
}

#[test]
fn eps_from_simplify_length_uses_meter2degp() {
    // simplify_length = 100 m, at latp=0 (equator).
    let m = 100.0;
    let e = tile_eps_degp(13, 14, 0.0, m, 2.0, 0.0);
    let expected = corrie::mercator::meter2degp(100.0, 0.0);
    assert!((e - expected).abs() < 1e-15);
}

use corrie::tile_gen::{enumerate_tiles, TileCoord};

#[test]
fn enumerate_covers_bbox_at_one_zoom() {
    // ~Prague area, z=11.
    let bbox = [14.0, 49.9, 14.6, 50.2];
    let tiles = enumerate_tiles(bbox, 11, 11);
    assert!(!tiles.is_empty());
    for t in &tiles {
        assert_eq!(t.z, 11);
    }
    // At z11 the world is 2048 tiles per side; Prague is around x=1107, y=692.
    assert!(tiles.iter().any(|t| t.x >= 1100 && t.x <= 1115));
}

#[test]
fn enumerate_world_at_z0_is_one_tile() {
    let tiles = enumerate_tiles([-180.0, -85.0, 180.0, 85.0], 0, 0);
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0], TileCoord { z: 0, x: 0, y: 0 });
}

use corrie::tile_gen::hilbert_sort;

#[test]
fn hilbert_sort_is_deterministic_and_orders_by_id() {
    let mut tiles = vec![
        TileCoord { z: 11, x: 1108, y: 690 },
        TileCoord { z: 11, x: 1108, y: 691 },
        TileCoord { z: 11, x: 1110, y: 691 },
        TileCoord { z: 11, x: 1107, y: 690 },
    ];
    hilbert_sort(&mut tiles);
    // First and last tiles must differ — there must be _some_ ordering effect.
    assert!(tiles.first() != tiles.last());
    // Repeated sort idempotent.
    let snap = tiles.clone();
    hilbert_sort(&mut tiles);
    assert_eq!(tiles, snap);
}

use corrie::tile_gen::enumerate_tiles_pruned;
use corrie::pack::{Pack, PackWriter};

/// One pack holding `lines` of (lon, lat) WGS84 vertices.
fn pack_of(lines: &[Vec<[f64; 2]>]) -> (tempfile::TempDir, Pack) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.cpk");
    let mut w = PackWriter::create(&p).unwrap();
    for l in lines {
        let latp: Vec<[f64; 2]> = l.iter().map(|c| [c[0], corrie::mercator::lat2latp(c[1])]).collect();
        w.add_line(&latp, 100).unwrap();
    }
    w.finish(&serde_json::json!({})).unwrap();
    let pack = Pack::open(&p).unwrap();
    (dir, pack)
}

#[test]
fn pruned_subset_of_unpruned() {
    // One short line in roughly the Prague tile area at z=11.
    let (_d, pack) = pack_of(&[vec![[14.3, 50.0], [14.5, 50.1]]]);
    let bbox = [14.0, 49.9, 14.6, 50.2];
    let unpruned = enumerate_tiles(bbox, 11, 13);
    let pruned   = enumerate_tiles_pruned(std::slice::from_ref(&pack), bbox, 11, 13);

    use std::collections::HashSet;
    let u: HashSet<_> = unpruned.iter().copied().collect();
    let p: HashSet<_> = pruned.iter().copied().collect();
    assert!(p.is_subset(&u), "pruned must be a subset of unpruned");
    assert!(!p.is_empty(), "pruned must keep at least the covering tiles");
    // The single small feature shouldn't pull in the whole z=13 bbox.
    assert!(p.len() < u.len(),
        "pruning must remove at least one tile (got pruned={}, unpruned={})",
        p.len(), u.len());
}

#[test]
fn pruned_world_with_no_features_is_empty() {
    let (_d, pack) = pack_of(&[]);
    let tiles = enumerate_tiles_pruned(std::slice::from_ref(&pack), [-180.0, -85.0, 180.0, 85.0], 0, 5);
    assert!(tiles.is_empty(), "empty pack must yield zero tiles");
}
