//! Streaming SHP+DBF reader specialised for contour-line shapefiles.
//!
//! Backed by GDAL/OGR. We previously used the pure-Rust `shapefile` crate
//! but it overflows i32 byte-offsets on `.shp` files larger than 2 GB, and
//! the mapriot dataset has multiple country tiles past that boundary
//! (e.g. `contours/au/final_contours_6_4_0_1.shp` at 2.5 GB).
//!
//! Each emitted feature carries the polyline coordinates already projected
//! from `(lon, lat)` WGS84 to `(lon, latp)` tilemaker space, the integer
//! elevation, and the bbox in `(lon, latp)`.

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
    /// Bounding box in `(lon, latp)`.
    pub bbox_min_lon: f64,
    pub bbox_min_latp: f64,
    pub bbox_max_lon: f64,
    pub bbox_max_latp: f64,
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
pub fn read_polylines_streaming<F>(
    shp_path: &Path,
    elevation_field: &str,
    mut emit: F,
) -> Result<()>
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
    for feature in layer.features() {
        let elev_f64 = match feature.field_as_double(elev_idx) {
            Ok(Some(v)) => v,
            Ok(None) => {
                eprintln!("warning: record {} of {:?} has null elevation", rec_idx, shp_path);
                rec_idx += 1;
                continue;
            }
            Err(e) => {
                eprintln!("warning: record {} of {:?} elevation read error: {}", rec_idx, shp_path, e);
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
                emit_linestring(geom, elevation, &mut emit)?;
            }
            OGRwkbGeometryType::wkbMultiLineString => {
                let n = geom.geometry_count();
                for i in 0..n {
                    let sub = geom.get_geometry(i);
                    emit_linestring(&sub, elevation, &mut emit)?;
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
    Ok(())
}

fn emit_linestring<F>(geom: &Geometry, elevation: i16, emit: &mut F) -> Result<()>
where
    F: FnMut(ReadFeature) -> Result<()>,
{
    let n = geom.point_count();
    if n < 2 { return Ok(()); }

    let mut coords: Vec<[f64; 2]> = Vec::with_capacity(n);
    let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
    let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for i in 0..n {
        let (x, y, _z) = geom.get_point(i as i32);
        let latp = lat2latp(y);
        coords.push([x, latp]);
        if x < mnx { mnx = x; }
        if x > mxx { mxx = x; }
        if latp < mny { mny = latp; }
        if latp > mxy { mxy = latp; }
    }

    emit(ReadFeature {
        coords,
        elevation,
        bbox_min_lon: mnx,
        bbox_min_latp: mny,
        bbox_max_lon: mxx,
        bbox_max_latp: mxy,
    })
}
