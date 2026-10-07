//! Write packs back out as GIS lines (FlatGeobuf, GeoPackage, GeoJSON or
//! Shapefile, chosen by the output's extension), e.g. to look at one or
//! several neighbouring countries in QGIS.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};
use gdal::vector::{Feature, FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};
use gdal::DriverManager;

use crate::attribute_rules::{derive_attributes, MAIN_FLAG, SHOW_FLAG};
use crate::mercator::{lat2latp, latp2lat};
use crate::pack::{grid_range, Pack};

pub struct UnpackStats {
    pub lines: u64,
    pub vertices: u64,
}

/// Write the lines of `packs` (only those touching `bbox`, `[min_lon,
/// min_lat, max_lon, max_lat]`, if given) to `out`, in WGS84, with fields
/// elevation, main, show, source (the pack's file name) and folder (the
/// pack's folder, i.e. its country in a contours/<cc>/ layout).
pub fn unpack(packs: &[PathBuf], out: &Path, bbox: Option<[f64; 4]>) -> Result<UnpackStats> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let driver_name = match ext.as_str() {
        "fgb" => "FlatGeobuf",
        "gpkg" => "GPKG",
        "geojson" | "json" => "GeoJSON",
        "shp" => "ESRI Shapefile",
        _ => bail!("unknown output format {:?}: use .fgb, .gpkg, .geojson or .shp", out),
    };
    if out.exists() {
        std::fs::remove_file(out).with_context(|| format!("replace {:?}", out))?;
    }
    let driver = DriverManager::get_driver_by_name(driver_name)?;
    let mut ds = driver.create_vector_only(out).with_context(|| format!("create {:?}", out))?;
    let stats = if driver_name == "GPKG" {
        // one transaction instead of one per feature
        let mut txn = ds.start_transaction()?;
        let stats = write_layer(&mut txn, packs, bbox)?;
        txn.commit()?;
        stats
    } else {
        write_layer(&mut ds, packs, bbox)?
    };
    ds.flush_cache()?;
    Ok(stats)
}

fn write_layer(ds: &mut gdal::Dataset, packs: &[PathBuf], bbox: Option<[f64; 4]>) -> Result<UnpackStats> {
    let mut srs = SpatialRef::from_epsg(4326)?;
    srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let layer = ds.create_layer(LayerOptions {
        name: "contours",
        srs: Some(&srs),
        ty: OGRwkbGeometryType::wkbLineString,
        options: None,
    })?;
    for (name, ty) in [("elevation", OGRFieldType::OFTInteger), ("main", OGRFieldType::OFTInteger),
                       ("show", OGRFieldType::OFTInteger), ("source", OGRFieldType::OFTString),
                       ("folder", OGRFieldType::OFTString)] {
        FieldDefn::new(name, ty)?.add_to_layer(&layer)?;
    }
    let defn = layer.defn();

    let query = bbox.map(|b| {
        let (x0, x1) = grid_range(b[0], b[2]);
        let (y0, y1) = grid_range(lat2latp(b[1]), lat2latp(b[3]));
        [x0, y0, x1, y1]
    });
    let mut stats = UnpackStats { lines: 0, vertices: 0 };
    let mut wkb: Vec<u8> = Vec::new();
    let mut verts: Vec<[f64; 2]> = Vec::new();
    for path in packs {
        let pack = Pack::open(path)?;
        let source = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let folder = path.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy().into_owned();
        // (line, first vertex, chunk) of every chunk to write, sorted so each
        // line's chunks come together and in order
        let chunks = pack.chunks();
        let mut wanted: Vec<(u32, u32, u32)> = match &query {
            None => chunks.iter().enumerate().map(|(i, c)| (c.line, c.first_vertex, i as u32)).collect(),
            Some(q) => {
                let mut lines = std::collections::HashSet::new();
                pack.query(q, |ci| { lines.insert(chunks[ci as usize].line); true });
                chunks.iter().enumerate().filter(|(_, c)| lines.contains(&c.line))
                    .map(|(i, c)| (c.line, c.first_vertex, i as u32)).collect()
            }
        };
        wanted.sort_unstable();
        let mut k = 0;
        while k < wanted.len() {
            let line = wanted[k].0;
            verts.clear();
            let mut first = true;
            while k < wanted.len() && wanted[k].0 == line {
                pack.decode_chunk(wanted[k].2, !first, &mut verts);
                first = false;
                k += 1;
            }
            // WKB LineString, little-endian, in lon/lat
            wkb.clear();
            wkb.push(1);
            wkb.extend_from_slice(&2u32.to_le_bytes());
            wkb.extend_from_slice(&(verts.len() as u32).to_le_bytes());
            for v in &verts {
                wkb.extend_from_slice(&v[0].to_le_bytes());
                wkb.extend_from_slice(&latp2lat(v[1]).to_le_bytes());
            }
            let elevation = pack.lines()[line as usize].elevation;
            let (_, flags) = derive_attributes(elevation as i32);
            let mut f = Feature::new(&defn)?;
            f.set_field_integer(0, elevation as i32)?;
            f.set_field_integer(1, (flags & MAIN_FLAG != 0) as i32)?;
            f.set_field_integer(2, (flags & SHOW_FLAG != 0) as i32)?;
            f.set_field_string(3, &source)?;
            f.set_field_string(4, &folder)?;
            f.set_geometry(Geometry::from_wkb(&wkb)?)?;
            f.create(&layer)?;
            stats.lines += 1;
            stats.vertices += verts.len() as u64;
        }
    }
    Ok(stats)
}
