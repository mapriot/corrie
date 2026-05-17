//! Streaming SHP+DBF reader specialised for contour-line shapefiles.
//!
//! Each emitted feature carries the polyline coordinates already projected
//! from `(lon, lat)` WGS84 to `(lon, latp)` tilemaker space, the integer
//! elevation, and the bbox in `(lon, latp)`.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::mercator::lat2latp;

#[derive(Debug, Clone)]
pub struct ReadFeature {
    /// `(lon, latp)` vertices, f64.
    pub coords: Vec<[f64; 2]>,
    /// Floor of the DBF elevation column, clamped to i16 range.
    pub elevation: i16,
    /// Bounding box in `(lon, latp)`.
    pub bbox_min_lon: f64,
    pub bbox_min_latp: f64,
    pub bbox_max_lon: f64,
    pub bbox_max_latp: f64,
}
