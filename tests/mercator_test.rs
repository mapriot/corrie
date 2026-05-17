use corrie::mercator::{lat2latp, latp2lat};

#[test]
fn lat2latp_round_trip() {
    for &lat in &[-80.0f64, -45.0, -1.0, 0.0, 1.0, 45.0, 60.0, 80.0] {
        let latp = lat2latp(lat);
        let back = latp2lat(latp);
        assert!((back - lat).abs() < 1e-9, "lat {} round trip {} (err {})", lat, back, (back - lat).abs());
    }
}

#[test]
fn lat2latp_known_values() {
    // At lat=0, latp=0
    assert!((lat2latp(0.0)).abs() < 1e-9);
    // At lat=45, latp ≈ 50.4990 (Mercator-flattened)
    assert!((lat2latp(45.0) - 50.498_986_7).abs() < 1e-4);
}

#[test]
fn lat2latp_clamped_at_poles() {
    // Beyond Web Mercator limit, must clamp to ±85.0511
    let high = lat2latp(89.9);
    let cap  = lat2latp(85.051_129);
    assert!((high - cap).abs() < 1e-6, "should clamp at the WebMercator limit");
}
