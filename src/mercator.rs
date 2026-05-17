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
