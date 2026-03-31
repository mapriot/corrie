use std::cell::RefCell;
use std::path::Path;

use anyhow::{Context, Result};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::Dataset;

use crate::tile::{merc_to_wgs84, tile_bounds_3857};

/// Read the WGS84 bounding box of a GDAL dataset.
/// Returns (west_lon, south_lat, east_lon, north_lat).
pub fn dataset_wgs84_bounds(path: &Path) -> Result<(f64, f64, f64, f64)> {
    let ds = Dataset::open(path).context("open dataset")?;
    let gt = ds.geo_transform().context("geo_transform")?;
    let (w, h) = ds.raster_size();

    let ox = gt[0];
    let oy = gt[3];
    let ex = ox + gt[1] * w as f64;
    let ey = oy + gt[5] * h as f64;

    let mut xs = [ox, ex, ox, ex];
    let mut ys = [oy, oy, ey, ey];

    let mut src_srs = SpatialRef::from_wkt(&ds.projection()).context("source SRS for bounds")?;
    src_srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let mut wgs84 = SpatialRef::from_epsg(4326).context("EPSG:4326")?;
    wgs84.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let to_wgs84 = CoordTransform::new(&src_srs, &wgs84).context("coord transform")?;
    to_wgs84
        .transform_coords(&mut xs, &mut ys, &mut [] as &mut [f64])
        .context("transform corners to WGS84")?;

    let west = xs.iter().cloned().fold(f64::MAX, f64::min);
    let east = xs.iter().cloned().fold(f64::MIN, f64::max);
    let south = ys.iter().cloned().fold(f64::MAX, f64::min);
    let north = ys.iter().cloned().fold(f64::MIN, f64::max);

    eprintln!(
        "Input: {}x{} px  WGS84 bounds [{:.4}W {:.4}S {:.4}E {:.4}N]",
        w, h, west, south, east, north
    );
    Ok((west, south, east, north))
}

// ── Per-thread dataset cache ────────────────────────────────────────────────

struct DatasetCache {
    input_path: String,
    dataset: Dataset,
    gt: [f64; 6],
    src_w: usize,
    src_h: usize,
    nodata_base: f32,
    to_src: Option<CoordTransform>,
}

thread_local! {
    static TILE_CACHE: RefCell<Option<DatasetCache>> = RefCell::new(None);
}

fn init_dataset_cache(input_path: &str) -> Result<DatasetCache> {
    let dataset = Dataset::open(input_path).context("open dataset")?;
    let gt = dataset.geo_transform().context("geo_transform")?;
    let (src_w, src_h) = dataset.raster_size();
    let nodata_base = dataset
        .rasterband(1)
        .context("rasterband 1")?
        .no_data_value()
        .unwrap_or(-32_767.0) as f32;

    let mut src_srs = SpatialRef::from_wkt(&dataset.projection()).context("source SRS")?;
    src_srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);

    let src_is_wgs84 = src_srs.is_geographic()
        && src_srs.auth_name().as_deref() == Some("EPSG")
        && src_srs.auth_code().ok() == Some(4326);

    let to_src = if !src_is_wgs84 {
        let mut srs_3857 = SpatialRef::from_epsg(3857).context("EPSG:3857")?;
        srs_3857.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        Some(CoordTransform::new(&srs_3857, &src_srs).context("coord transform 3857->src")?)
    } else {
        None
    };

    Ok(DatasetCache {
        input_path: input_path.to_owned(),
        dataset,
        gt,
        src_w,
        src_h,
        nodata_base,
        to_src,
    })
}

// ── Elevation grid returned to caller ───────────────────────────────────────

pub struct ElevationGrid {
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub nodata: f32,
    /// Tile boundary positions in grid (buffer) coordinates — used to map grid
    /// coordinates to MVT coordinates.
    pub tile_left: f64,
    pub tile_right: f64,
    pub tile_top: f64,
    pub tile_bottom: f64,
}

/// Number of extra source pixels read on each side of the tile for contour
/// continuity across tile boundaries.
const BUFFER_PX: i64 = 8;
/// Maximum buffer dimension per axis — GDAL resamples if source window is larger.
const MAX_BUF: usize = 2048;

/// Read an elevation grid for one tile from the DEM. Returns `None` if the
/// grid is entirely nodata.
pub fn read_elevation_grid(
    input_path: &str,
    z: u8,
    x: u32,
    y_xyz: u32,
    nodata_override: Option<f32>,
) -> Result<Option<ElevationGrid>> {
    use gdal::raster::ResampleAlg;

    TILE_CACHE.with(|cell| -> Result<Option<ElevationGrid>> {
        {
            let mut opt = cell.borrow_mut();
            if opt.as_ref().map_or(true, |c| c.input_path != input_path) {
                *opt = Some(init_dataset_cache(input_path)?);
            }
        }

        let cache_ref = cell.borrow();
        let cache = cache_ref.as_ref().unwrap();

        let nodata = nodata_override.unwrap_or(cache.nodata_base);
        let gt = cache.gt;
        let src_w = cache.src_w;
        let src_h = cache.src_h;
        let band = cache.dataset.rasterband(1).context("rasterband 1")?;

        // ── Tile extent in 3857 ─────────────────────────────────────────────
        let [west_m, south_m, east_m, north_m] = tile_bounds_3857(z, x, y_xyz);

        // ── Transform tile corners + midpoints to source SRS ────────────────
        let mid_x = (west_m + east_m) / 2.0;
        let mid_y = (south_m + north_m) / 2.0;
        let mut cx = [west_m, east_m, west_m, east_m, mid_x, west_m, east_m, mid_x];
        let mut cy = [south_m, south_m, north_m, north_m, mid_y, mid_y, mid_y, south_m];
        if let Some(ref t) = cache.to_src {
            t.transform_coords(&mut cx, &mut cy, &mut [] as &mut [f64])
                .context("transform corners")?;
        } else {
            for i in 0..cx.len() {
                let (lon, lat) = merc_to_wgs84(cx[i], cy[i]);
                cx[i] = lon;
                cy[i] = lat;
            }
        }

        let src_x_min = cx.iter().cloned().fold(f64::MAX, f64::min);
        let src_x_max = cx.iter().cloned().fold(f64::MIN, f64::max);
        let src_y_min = cy.iter().cloned().fold(f64::MAX, f64::min);
        let src_y_max = cy.iter().cloned().fold(f64::MIN, f64::max);

        // ── Source-SRS bbox → pixel indices ─────────────────────────────────
        let px_min = (src_x_min - gt[0]) / gt[1];
        let px_max = (src_x_max - gt[0]) / gt[1];
        let py_min = (src_y_max - gt[3]) / gt[5]; // gt[5] < 0
        let py_max = (src_y_min - gt[3]) / gt[5];

        // Tile boundary pixel positions (before buffer expansion)
        let tile_px_min = px_min;
        let tile_px_max = px_max;
        let tile_py_min = py_min;
        let tile_py_max = py_max;

        // Expand by BUFFER_PX for contour continuity; clamp to source bounds
        let rx0 = (px_min.floor() as i64 - BUFFER_PX).clamp(0, src_w as i64 - 1) as usize;
        let ry0 = (py_min.floor() as i64 - BUFFER_PX).clamp(0, src_h as i64 - 1) as usize;
        let rx1 =
            (px_max.ceil() as i64 + BUFFER_PX + 1).clamp(rx0 as i64 + 1, src_w as i64) as usize;
        let ry1 =
            (py_max.ceil() as i64 + BUFFER_PX + 1).clamp(ry0 as i64 + 1, src_h as i64) as usize;

        let rw = rx1 - rx0;
        let rh = ry1 - ry0;

        let bw = rw.min(MAX_BUF);
        let bh = rh.min(MAX_BUF);

        let buf = band
            .read_as::<f32>(
                (rx0 as isize, ry0 as isize),
                (rw, rh),
                (bw, bh),
                Some(ResampleAlg::NearestNeighbour),
            )
            .context("read_as")?;
        let data = buf.data().to_vec();

        let is_nd = |v: f32| (v - nodata).abs() < 0.5 || v.is_nan();
        if !data.iter().any(|&v| !is_nd(v)) {
            return Ok(None);
        }

        // Scale factors: source pixels → buffer pixels
        let sx = bw as f64 / rw as f64;
        let sy = bh as f64 / rh as f64;

        // Tile boundary positions in buffer (grid) coordinates
        let tile_left = (tile_px_min - rx0 as f64) * sx;
        let tile_right = (tile_px_max - rx0 as f64) * sx;
        let tile_top = (tile_py_min - ry0 as f64) * sy;
        let tile_bottom = (tile_py_max - ry0 as f64) * sy;

        Ok(Some(ElevationGrid {
            data,
            width: bw,
            height: bh,
            nodata,
            tile_left,
            tile_right,
            tile_top,
            tile_bottom,
        }))
    })
}
