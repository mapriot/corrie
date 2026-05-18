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

use shapefile::dbase::FieldValue;
use shapefile::{Reader, Shape};

/// Read polyline features from a shapefile, projecting lat→latp, and invoke
/// `emit` once per emitted feature.
///
/// Streaming: no per-shapefile accumulator. Each feature is yielded to the
/// caller as soon as it has been parsed and its coords vec allocated, then
/// the caller can move that vec straight onto the on-disk feature store
/// without retaining it.
///
/// Reads the column named by `elevation_field` from the DBF and floors it
/// into an i16. Records of unsupported geometry types fail the whole shp;
/// records missing the elevation column or with non-numeric values are
/// skipped with a warning to stderr.
pub fn read_polylines_streaming<F>(
    shp_path: &Path,
    elevation_field: &str,
    mut emit: F,
) -> Result<()>
where
    F: FnMut(ReadFeature) -> Result<()>,
{
    let mut reader = Reader::from_path(shp_path)
        .with_context(|| format!("open {:?}", shp_path))?;

    for (i, shape_rec) in reader.iter_shapes_and_records().enumerate() {
        let (shape, record) = shape_rec
            .with_context(|| format!("read record {} of {:?}", i, shp_path))?;

        // Elevation from DBF.
        let elev_f64 = match record.get(elevation_field) {
            Some(FieldValue::Numeric(Some(v))) => *v,
            Some(FieldValue::Integer(v)) => *v as f64,
            Some(other) => {
                eprintln!("warning: record {} elevation field has unsupported type {:?}", i, other);
                continue;
            }
            None => {
                eprintln!("warning: record {} missing elevation field {:?}", i, elevation_field);
                continue;
            }
        };
        let elevation_i32 = elev_f64.floor() as i32;
        let elevation = elevation_i32.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        // Iterate parts directly, projecting and emitting per-part without
        // a per-shapefile Vec.
        match shape {
            Shape::Polyline(pl) => {
                for part in pl.parts() {
                    let coords: Vec<[f64; 2]> = part.iter().map(|p| [p.x, lat2latp(p.y)]).collect();
                    emit_part(coords, elevation, &mut emit)?;
                }
            }
            Shape::PolylineM(pl) => {
                for part in pl.parts() {
                    let coords: Vec<[f64; 2]> = part.iter().map(|p| [p.x, lat2latp(p.y)]).collect();
                    emit_part(coords, elevation, &mut emit)?;
                }
            }
            Shape::PolylineZ(pl) => {
                for part in pl.parts() {
                    let coords: Vec<[f64; 2]> = part.iter().map(|p| [p.x, lat2latp(p.y)]).collect();
                    emit_part(coords, elevation, &mut emit)?;
                }
            }
            Shape::NullShape => continue,
            other => {
                return Err(anyhow!(
                    "shapefile {:?} record {} has unsupported geometry type {:?}",
                    shp_path, i, other.shapetype()
                ));
            }
        }
    }
    Ok(())
}

fn emit_part<F>(coords: Vec<[f64; 2]>, elevation: i16, emit: &mut F) -> Result<()>
where
    F: FnMut(ReadFeature) -> Result<()>,
{
    if coords.len() < 2 { return Ok(()); }
    let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
    let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for c in &coords {
        if c[0] < mnx { mnx = c[0]; }
        if c[1] < mny { mny = c[1]; }
        if c[0] > mxx { mxx = c[0]; }
        if c[1] > mxy { mxy = c[1]; }
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
