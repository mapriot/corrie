use corrie::mercator::{lat2latp, latp2lat, tilex2lon, tiley2latp, meter2degp};

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

#[test]
fn tilex2lon_boundaries() {
    // At z=0 there is one tile from x=0 to x=1, covering -180..180.
    assert!((tilex2lon(0, 0) - (-180.0)).abs() < 1e-12);
    assert!((tilex2lon(1, 0) -  180.0).abs() < 1e-12);
    // At z=1, x=0..2; x=1 splits at lon=0.
    assert!((tilex2lon(1, 1)).abs() < 1e-12);
}

#[test]
fn tiley2latp_north_to_south() {
    // y=0 at top = LAT_LIMIT in latp space. y=2^z at bottom.
    let max_latp = tiley2latp(0, 0);
    let min_latp = tiley2latp(1, 0);
    assert!(max_latp > 0.0);
    assert!(min_latp < 0.0);
    assert!((max_latp + min_latp).abs() < 1e-9, "should be symmetric about latp=0");
}

#[test]
fn meter2degp_at_equator() {
    // At latp=0 (lat=0), 1 metre of longitude ≈ 360/(2πR) degrees
    let one_m_in_deg = 360.0 / (2.0 * std::f64::consts::PI * 6_371_000.0);
    let computed = meter2degp(1.0, 0.0);
    assert!((computed - one_m_in_deg).abs() < 1e-12);
}

#[test]
fn meter2degp_at_45_north() {
    // At lat=45 the degree-per-metre factor is multiplied by 1/cos(45) = √2.
    let latp45 = lat2latp(45.0);
    let base = 360.0 / (2.0 * std::f64::consts::PI * 6_371_000.0);
    let expected = base / (45.0f64.to_radians()).cos();
    let got = meter2degp(1.0, latp45);
    assert!((got - expected).abs() < 1e-12);
}
