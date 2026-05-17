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

/// Read all polyline features from a shapefile, projecting lat→latp.
///
/// Reads the column named by `elevation_field` from the DBF and floors it
/// into an i16. Records of unsupported geometry types are returned as errors;
/// records missing the elevation column or with non-numeric values are skipped
/// with a warning to stderr.
pub fn read_polylines(shp_path: &Path, elevation_field: &str) -> Result<Vec<ReadFeature>> {
    let mut reader = Reader::from_path(shp_path)
        .with_context(|| format!("open {:?}", shp_path))?;

    let mut out: Vec<ReadFeature> = Vec::new();

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

        // Polylines may have multiple parts (rings/segments). Emit one
        // ReadFeature per part — each part is one MVT linestring.
        let parts: Vec<Vec<[f64; 2]>> = match shape {
            Shape::Polyline(pl) => pl.parts().iter().map(|part| {
                part.iter().map(|p| [p.x, lat2latp(p.y)]).collect()
            }).collect(),
            Shape::PolylineM(pl) => pl.parts().iter().map(|part| {
                part.iter().map(|p| [p.x, lat2latp(p.y)]).collect()
            }).collect(),
            Shape::PolylineZ(pl) => pl.parts().iter().map(|part| {
                part.iter().map(|p| [p.x, lat2latp(p.y)]).collect()
            }).collect(),
            Shape::NullShape => continue,
            other => {
                return Err(anyhow!(
                    "shapefile {:?} record {} has unsupported geometry type {:?}",
                    shp_path, i, other.shapetype()
                ));
            }
        };

        for coords in parts {
            if coords.len() < 2 { continue; }
            let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
            let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            for c in &coords {
                if c[0] < mnx { mnx = c[0]; }
                if c[1] < mny { mny = c[1]; }
                if c[0] > mxx { mxx = c[0]; }
                if c[1] > mxy { mxy = c[1]; }
            }
            out.push(ReadFeature {
                coords,
                elevation,
                bbox_min_lon: mnx,
                bbox_min_latp: mny,
                bbox_max_lon: mxx,
                bbox_max_latp: mxy,
            });
        }
    }
    Ok(out)
}
