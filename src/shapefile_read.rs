//! Streaming SHP+DBF reader specialised for contour-line shapefiles.
//!
//! Backed by GDAL/OGR. We previously used the pure-Rust `shapefile` crate
//! but it overflows i32 byte-offsets on `.shp` files larger than 2 GB, and
//! the mapriot dataset has multiple country tiles past that boundary
//! (e.g. `contours/au/final_contours_6_4_0_1.shp` at 2.5 GB).
//!
//! Each emitted feature carries the polyline coordinates already projected
//! from `(lon, lat)` WGS84 to `(lon, latp)` tilemaker space, and the integer
//! elevation.

use anyhow::{anyhow, Context, Result};
use gdal::vector::{Geometry, LayerAccess, OGRwkbGeometryType};
use gdal::Dataset;
use std::path::Path;

use crate::mercator::lat2latp;

#[derive(Debug, Clone)]
pub struct ReadFeature {
    /// `(lon, latp)` vertices, f64.
    pub coords: Vec<[f64; 2]>,
    /// Floor of the DBF elevation column, clamped to i16 range.
    pub elevation: i16,
}

/// Read polyline features from a shapefile via GDAL/OGR, projecting
/// lat→latp, and invoke `emit` once per emitted feature.
///
/// Streaming: no per-shapefile accumulator. Each feature is yielded to the
/// caller as soon as it has been parsed, so the caller can move that vec
/// straight onto the on-disk feature store without retaining it.
///
/// Reads the column named by `elevation_field` from the DBF and floors it
/// into an i16. Records of unsupported geometry types fail the whole shp.
/// Records with null elevation are skipped with a warning to stderr.
///
/// With `drop_seams`, a feature whose every vertex (over all its parts) lies
/// within that many degrees of a whole-degree meridian or parallel is
/// skipped (see [`on_degree_seams`]). Returns the number of such features.
pub fn read_polylines_streaming<F>(
    shp_path: &Path,
    elevation_field: &str,
    drop_seams: Option<f64>,
    mut emit: F,
) -> Result<u64>
where
    F: FnMut(ReadFeature) -> Result<()>,
{
    let ds = Dataset::open(shp_path)
        .with_context(|| format!("open {:?}", shp_path))?;
    let mut layer = ds.layer(0)
        .with_context(|| format!("open layer 0 of {:?}", shp_path))?;

    // Look up the elevation field index once. `field_as_double(idx)` is
    // cheaper per-feature than `field_index(name)` would be.
    let elev_idx = {
        let mut idx = None;
        for (i, fd) in layer.defn().fields().enumerate() {
            if fd.name() == elevation_field { idx = Some(i); break; }
        }
        idx.with_context(|| format!(
            "field {:?} not found in {:?}", elevation_field, shp_path
        ))?
    };

    let mut rec_idx: u64 = 0;
    let mut dropped: u64 = 0;
    for feature in layer.features() {
        let elev_f64 = match feature.field_as_double(elev_idx) {
            Ok(Some(v)) => v,
            Ok(None) => {
                crate::log!("warning: record {} of {:?} has null elevation", rec_idx, shp_path);
                rec_idx += 1;
                continue;
            }
            Err(e) => {
                crate::log!("warning: record {} of {:?} elevation read error: {}", rec_idx, shp_path, e);
                rec_idx += 1;
                continue;
            }
        };
        let elevation_i32 = elev_f64.floor() as i32;
        let elevation = elevation_i32.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        let geom = match feature.geometry() {
            Some(g) => g,
            None => { rec_idx += 1; continue; }
        };

        // Flatten Z/M variants so PolylineZ / PolylineM hit the same arm
        // as plain LineString.
        let flat = gdal::vector::geometry_type_flatten(geom.geometry_type());
        match flat {
            OGRwkbGeometryType::wkbLineString => {
                if drop_seams.is_some_and(|t| on_degree_seams(points(geom), t)) {
                    dropped += 1;
                } else {
                    emit_linestring(geom, elevation, &mut emit)?;
                }
            }
            OGRwkbGeometryType::wkbMultiLineString => {
                let parts: Vec<_> = (0..geom.geometry_count()).map(|i| geom.get_geometry(i)).collect();
                if drop_seams.is_some_and(|t| on_degree_seams(parts.iter().flat_map(|p| points(p)), t)) {
                    dropped += 1;
                } else {
                    for sub in &parts {
                        emit_linestring(sub, elevation, &mut emit)?;
                    }
                }
            }
            _ => {
                return Err(anyhow!(
                    "shapefile {:?} record {} has unsupported geometry type {}",
                    shp_path, rec_idx,
                    gdal::vector::geometry_type_to_name(geom.geometry_type())
                ));
            }
        }
        rec_idx += 1;
    }
    Ok(dropped)
}

/// The `(x, y)` vertices of a LineString.
fn points(geom: &Geometry) -> impl Iterator<Item = (f64, f64)> + '_ {
    (0..geom.point_count()).map(move |i| { let (x, y, _) = geom.get_point(i as i32); (x, y) })
}

/// Does every one of at least two vertices lie within `t` degrees of a
/// whole-degree meridian (in `x`) or parallel (in `y`)? Contour lines like
/// that run along the seams of 1° DEM tiles (e.g. JAXA AW3D30), where one
/// tile's data meets nodata, rather than along the terrain.
pub fn on_degree_seams(points: impl IntoIterator<Item = (f64, f64)>, t: f64) -> bool {
    let near = |v: f64| {
        let mut frac = (v - v.trunc()).abs();
        if frac > 0.5 { frac = 1.0 - frac; }
        frac < t
    };
    let mut n = 0;
    for (x, y) in points {
        if !near(x) && !near(y) { return false; }
        n += 1;
    }
    n >= 2
}

fn emit_linestring<F>(geom: &Geometry, elevation: i16, emit: &mut F) -> Result<()>
where
    F: FnMut(ReadFeature) -> Result<()>,
{
    let n = geom.point_count();
    if n < 2 { return Ok(()); }

    let mut coords: Vec<[f64; 2]> = Vec::with_capacity(n);
    for i in 0..n {
        let (x, y, _z) = geom.get_point(i as i32);
        coords.push([x, lat2latp(y)]);
    }

    emit(ReadFeature { coords, elevation })
}

#[cfg(test)]
mod tests {
    use super::on_degree_seams;

    #[test]
    fn degree_seam_lines() {
        let t = 0.001;
        // along a meridian, and along a parallel (either side of the line)
        assert!(on_degree_seams([(-78.9995, -1.2), (-79.0004, -1.5), (-78.9999, -1.9)], t));
        assert!(on_degree_seams([(14.2, 50.0002), (14.7, 49.9993)], t));
        // a staircase along both, across a corner
        assert!(on_degree_seams([(15.0003, 49.6), (15.0003, 50.0001), (15.4, 50.0001)], t));
        // one vertex off the seams keeps the line
        assert!(!on_degree_seams([(15.0003, 49.6), (15.2, 49.7), (15.4, 50.0001)], t));
        // just past the threshold, and half a degree, the farthest there is
        assert!(!on_degree_seams([(15.0011, 49.6), (14.9989, 49.7)], t));
        assert!(!on_degree_seams([(15.5, 49.5), (15.5, 49.6)], t));
        // fewer than two vertices is never a seam line
        assert!(!on_degree_seams([(15.0, 50.0)], t));
        assert!(!on_degree_seams(std::iter::empty(), t));
    }
}
