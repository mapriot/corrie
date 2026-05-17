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
