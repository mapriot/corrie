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
