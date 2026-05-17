use std::collections::BTreeSet;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use pmtiles::{TileCoord, TileId};
use rayon::prelude::*;

use corrie::container::Writer;
use corrie::raster::{dataset_wgs84_bounds, read_elevation_grid};
use corrie::tile::{lat_to_tile_y_xyz, lon_to_tile_x};
use corrie::{contour, mvt, simplify};

/// A zoom-level / interval pair parsed from the --levels flag.
#[derive(Clone, Debug)]
struct Level {
    min_zoom: u8,
    interval: i32,
}

#[derive(clap::Parser, Debug)]
#[command(name = "corrie", version, about = "DEM → vector contour tile generator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// (legacy) DEM-direct path: single raster → PMTiles/MBTiles
    Build(BuildArgs),
    /// Read tilemaker config.json + SHPs, write clustered PMTiles.
    Tile(TileArgs),
}

#[derive(clap::Args, Debug)]
struct BuildArgs {
    /// Input elevation raster — GeoTIFF, VRT, or any GDAL-supported format and CRS.
    input: std::path::PathBuf,
    /// Output file — .pmtiles or .mbtiles
    output: std::path::PathBuf,
    #[arg(long, default_value = "11")]
    min_z: u8,
    #[arg(long, default_value = "14")]
    max_z: u8,
    #[arg(long, default_value = "11:50,12:20,13:10,14:5")]
    levels: String,
    #[arg(long, default_value = "50")]
    main_every: i32,
    #[arg(long, default_value = "100")]
    show_every: i32,
    #[arg(long, default_value = "contour")]
    layer: String,
    #[arg(long, default_value = "elevation")]
    elevation_attr: String,
    #[arg(long, allow_hyphen_values = true)]
    nodata: Option<f32>,
    #[arg(long, default_value = "2.0")]
    simplify: f64,
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

#[derive(clap::Args, Debug)]
struct TileArgs {
    /// Path to a tilemaker config.json
    config: std::path::PathBuf,
    /// Output .pmtiles
    output: std::path::PathBuf,
    /// Override cache directory (default: <config_dir>/.corrie-cache)
    #[arg(long)]
    cache_dir: Option<std::path::PathBuf>,
    /// Force re-ingest even if cache appears valid
    #[arg(long)]
    rebuild_cache: bool,
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

fn parse_levels(s: &str) -> Result<Vec<Level>> {
    s.split(',')
        .map(|pair| {
            let parts: Vec<&str> = pair.trim().split(':').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid level pair: {pair:?} — expected ZOOM:INTERVAL");
            }
            Ok(Level {
                min_zoom: parts[0].parse().context("parse zoom")?,
                interval: parts[1].parse().context("parse interval")?,
            })
        })
        .collect()
}

/// Compute the set of contour elevations (in metres) for a given zoom level
/// and elevation range.
fn contour_levels(min_elev: f32, max_elev: f32, zoom: u8, levels: &[Level]) -> Vec<i32> {
    let mut set = BTreeSet::new();
    for lv in levels {
        if zoom >= lv.min_zoom {
            let iv = lv.interval;
            let lo = (min_elev as f64 / iv as f64).ceil() as i32;
            let hi = (max_elev as f64 / iv as f64).floor() as i32;
            for i in lo..=hi {
                let elev = i * iv;
                // Skip 0 m — sea level contour is rarely useful
                if elev != 0 {
                    set.insert(elev);
                }
            }
        }
    }
    set.into_iter().collect()
}

/// Process a single tile: read DEM, extract contours, simplify, encode MVT.
fn process_tile(
    input_path: &str,
    z: u8,
    x: u32,
    y: u32,
    levels: &[Level],
    main_every: i32,
    show_every: i32,
    layer: &str,
    elevation_attr: &str,
    simplify_eps: f64,
    nodata: Option<f32>,
) -> Result<Option<Vec<u8>>> {
    let grid = match read_elevation_grid(input_path, z, x, y, nodata)? {
        Some(g) => g,
        None => return Ok(None),
    };

    // Find elevation range (excluding nodata)
    let is_nd = |v: f32| (v - grid.nodata).abs() < 0.5 || v.is_nan();
    let mut min_elev = f32::MAX;
    let mut max_elev = f32::MIN;
    for &v in &grid.data {
        if !is_nd(v) {
            min_elev = min_elev.min(v);
            max_elev = max_elev.max(v);
        }
    }
    if min_elev > max_elev {
        return Ok(None);
    }

    let elevations = contour_levels(min_elev, max_elev, z, levels);
    if elevations.is_empty() {
        return Ok(None);
    }

    // Grid → MVT coordinate mapping
    let extent = mvt::EXTENT as f64;
    let tw = grid.tile_right - grid.tile_left;
    let th = grid.tile_bottom - grid.tile_top;

    let mut features: Vec<mvt::ContourFeature> = Vec::new();

    for &elev in &elevations {
        let polylines =
            contour::extract_contours(&grid.data, grid.width, grid.height, grid.nodata, elev as f64);

        for pl in polylines {
            if pl.len() < 2 {
                continue;
            }

            // Convert grid coords (col, row) to MVT coords
            let mvt_pts: Vec<(f64, f64)> = pl
                .iter()
                .map(|&(gx, gy)| {
                    let mx = (gx - grid.tile_left) / tw * extent;
                    let my = (gy - grid.tile_top) / th * extent;
                    (mx, my)
                })
                .collect();

            // Simplify
            let simplified = if simplify_eps > 0.0 {
                simplify::simplify(&mvt_pts, simplify_eps)
            } else {
                mvt_pts
            };

            if simplified.len() < 2 {
                continue;
            }

            // Round to integers
            let points: Vec<(i32, i32)> = simplified
                .iter()
                .map(|&(x, y)| (x.round() as i32, y.round() as i32))
                .collect();

            // Deduplicate consecutive identical points
            let mut deduped: Vec<(i32, i32)> = Vec::with_capacity(points.len());
            for &p in &points {
                if deduped.last() != Some(&p) {
                    deduped.push(p);
                }
            }
            if deduped.len() < 2 {
                continue;
            }

            features.push(mvt::ContourFeature {
                points: deduped,
                elevation: elev,
                main: main_every > 0 && elev % main_every == 0,
                show: show_every > 0 && elev % show_every == 0,
            });
        }
    }

    if features.is_empty() {
        return Ok(None);
    }

    let tile_data = mvt::encode_tile(&features, layer, elevation_attr);
    Ok(Some(tile_data))
}

fn main() -> anyhow::Result<()> {
    use clap::Parser;
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Build(a) => run_build(a),
        Cmd::Tile(a) => run_tile(a),
    }
}

fn run_tile(_args: TileArgs) -> anyhow::Result<()> {
    anyhow::bail!("`corrie tile` not yet implemented (see Task 31)")
}

fn run_build(args: BuildArgs) -> anyhow::Result<()> {
    if let Some(w) = args.workers {
        rayon::ThreadPoolBuilder::new()
            .num_threads(w)
            .build_global()
            .context("build rayon thread pool")?;
    }

    let levels = parse_levels(&args.levels)?;

    let input_str = args
        .input
        .to_str()
        .context("input path is not valid UTF-8")?
        .to_owned();

    let (west_lon, south_lat, east_lon, north_lat) = dataset_wgs84_bounds(&args.input)?;

    // ── Build tile list ─────────────────────────────────────────────────────
    let mut tiles: Vec<(u8, u32, u32)> = Vec::new();
    for z in args.min_z..=args.max_z {
        let x0 = lon_to_tile_x(west_lon, z);
        let x1 = lon_to_tile_x(east_lon, z);
        let y0 = lat_to_tile_y_xyz(north_lat, z); // smaller y = north
        let y1 = lat_to_tile_y_xyz(south_lat, z);
        for x in x0..=x1 {
            for y in y0..=y1 {
                tiles.push((z, x, y));
            }
        }
    }
    eprintln!(
        "Zoom {}-{}:  {} candidate tiles  ({} threads)",
        args.min_z,
        args.max_z,
        tiles.len(),
        rayon::current_num_threads()
    );

    // ── Pre-sort by Hilbert ID for PMTiles streaming writer ─────────────────
    let needs_hilbert_sort =
        args.output.extension().and_then(|e| e.to_str()) == Some("pmtiles");
    if needs_hilbert_sort {
        eprintln!("Sorting {} tiles by Hilbert ID...", tiles.len());
        tiles.sort_by_cached_key(|&(z, x, y)| {
            TileId::from(TileCoord::new(z, x, y).expect("valid coord")).value()
        });
    }

    let mut writer = Writer::open(
        &args.output,
        args.min_z,
        args.max_z,
        (west_lon, south_lat, east_lon, north_lat),
        &args.layer,
        &args.elevation_attr,
    )?;

    // ── Chunked parallel tile generation ────────────────────────────────────
    const CHUNK_SIZE: usize = 4096;

    let pb = ProgressBar::new(tiles.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:45.cyan/blue} {pos:>6}/{len} tiles  {tiles_per_sec}/s  eta {eta}",
        )
        .unwrap()
        .with_key(
            "tiles_per_sec",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{}", state.per_sec() as u64).unwrap();
            },
        ),
    );

    let layer = &args.layer;
    let elevation_attr = &args.elevation_attr;
    let main_every = args.main_every;
    let show_every = args.show_every;
    let simplify_eps = args.simplify;
    let nodata = args.nodata;
    let mut n_written: u64 = 0;
    let mut n_errors: u64 = 0;

    for chunk in tiles.chunks(CHUNK_SIZE) {
        let chunk_results: Vec<Result<Option<Vec<u8>>>> = chunk
            .par_iter()
            .map(|&(z, x, y)| {
                let r = process_tile(
                    &input_str,
                    z,
                    x,
                    y,
                    &levels,
                    main_every,
                    show_every,
                    layer,
                    elevation_attr,
                    simplify_eps,
                    nodata,
                );
                pb.inc(1);
                r
            })
            .collect();

        for (i, result) in chunk_results.into_iter().enumerate() {
            match result {
                Ok(Some(tile)) => {
                    let (z, x, y) = chunk[i];
                    writer.add_tile(z, x, y, &tile).context("add_tile")?;
                    n_written += 1;
                }
                Ok(None) => {}
                Err(e) => {
                    let (z, x, y) = chunk[i];
                    eprintln!("Warning: tile {}/{}/{} failed: {:#}", z, x, y, e);
                    n_errors += 1;
                }
            }
        }
    }

    pb.finish_with_message("done");
    eprintln!("{} non-empty tiles written", n_written);
    if n_errors > 0 {
        eprintln!("Warning: {} tiles failed and were skipped", n_errors);
    }

    writer.finalize().context("finalize")?;

    let sz = std::fs::metadata(&args.output)?.len();
    eprintln!(
        "Written {:?}  ({:.1} MB)",
        args.output,
        sz as f64 / 1_048_576.0
    );
    Ok(())
}
