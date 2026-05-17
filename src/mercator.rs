//! Web Mercator / latp helpers — match tilemaker's `src/coordinates.cpp`.
//!
//! `latp` is "Mercator-flattened latitude" — the y axis of EPSG:3857 expressed
//! in degree units. Tilemaker uses (lon_degrees, latp_degrees) as its native
//! geometry space so that DP epsilon stays scale-comparable between x and y.

use std::f64::consts::PI;

/// Sphere radius (metres) used by tilemaker. Not the WGS84 ellipsoid.
pub const RADIUS_M: f64 = 6_371_000.0;

/// WebMercator latitude limit. Beyond this, projection diverges.
pub const LAT_LIMIT: f64 = 85.051_128_779_806_59;

/// Geographic latitude (degrees) → Mercator-flattened latitude (degrees).
#[inline]
pub fn lat2latp(lat_deg: f64) -> f64 {
    let lat = lat_deg.clamp(-LAT_LIMIT, LAT_LIMIT);
    (180.0 / PI) * ((lat + 90.0) / 2.0 * PI / 180.0).tan().ln()
}

/// Mercator-flattened latitude (degrees) → geographic latitude (degrees).
#[inline]
pub fn latp2lat(latp_deg: f64) -> f64 {
    (180.0 / PI) * 2.0 * (latp_deg * PI / 180.0).exp().atan() - 90.0
}

/// Tile column x at zoom z → longitude (degrees) of its western edge.
#[inline]
pub fn tilex2lon(x: u32, z: u8) -> f64 {
    x as f64 * 360.0 / (1u64 << z) as f64 - 180.0
}

/// Tile row y at zoom z → latp (degrees) of its northern edge.
/// y=0 at the north. The y axis is linear in latp space.
#[inline]
pub fn tiley2latp(y: u32, z: u8) -> f64 {
    // World extent in latp is [-LAT_LIMIT_LATP, +LAT_LIMIT_LATP],
    // but tile system maps y∈[0, 2^z] linearly across [+LAT_LIMIT_LATP, -LAT_LIMIT_LATP].
    let latp_max = lat2latp(LAT_LIMIT);
    let n = (1u64 << z) as f64;
    latp_max - 2.0 * latp_max * (y as f64 / n)
}

/// Metres of ground distance → degrees of longitude (= degrees of latp at this row).
///
/// Tilemaker uses `RadiusMeter = 6_371_000` (sphere mean radius), and applies
/// the latitude `cos` correction so the same metric scale survives Mercator
/// stretching near the poles.
#[inline]
pub fn meter2degp(meters: f64, latp_deg: f64) -> f64 {
    let lat = latp2lat(latp_deg);
    (180.0 / PI) * (meters / RADIUS_M) / (lat * PI / 180.0).cos()
}

/// The asymmetric clip box tilemaker uses around a tile to maintain cross-tile
/// line continuity. Matches `coordinates_geom.cpp:95-99` exactly.
///
/// Returns `(min_lon, min_latp, max_lon, max_latp)` extended.
#[inline]
pub fn extend_box(
    min_lon: f64,
    min_latp: f64,
    max_lon: f64,
    max_latp: f64,
) -> (f64, f64, f64, f64) {
    let w = max_lon - min_lon;
    let h = max_latp - min_latp;
    let near = 8191.0 / 8192.0;
    (
        min_lon  - w * 2.0,
        min_latp - h * near,
        max_lon  + w * near,
        max_latp + h * 2.0,
    )
}
