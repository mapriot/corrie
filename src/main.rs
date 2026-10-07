use std::collections::BTreeSet;

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rayon::prelude::*;

use corrie::container::Writer;

/// `println!` that ignores a closed stdout (e.g. `corrie compare … | head`).
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}
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
    /// Compare two contour PMTiles tile by tile: same tile set, same lines
    /// per (elevation, main, show) group. Line order inside a tile is ignored.
    Compare(CompareArgs),
    /// Merge contour PMTiles (e.g. one per country) into one clustered archive.
    Merge(MergeArgs),
}

#[derive(clap::Args, Debug)]
struct MergeArgs {
    /// Output .pmtiles
    output: std::path::PathBuf,
    /// Input .pmtiles archives
    #[arg(required = true)]
    inputs: Vec<std::path::PathBuf>,
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

#[derive(clap::Args, Debug)]
struct CompareArgs {
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    /// How many differing tiles to print.
    #[arg(long, default_value = "10")]
    show: usize,
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
    /// Persistent cache directory. If omitted, ingest writes to a temp dir
    /// that's deleted on exit (no cache reuse). Pass this if you want
    /// subsequent runs with the same SHPs to skip the ingest pass.
    #[arg(long)]
    cache_dir: Option<std::path::PathBuf>,
    /// Force re-ingest even if cache appears valid (only meaningful with
    /// `--cache-dir`; ignored otherwise).
    #[arg(long)]
    rebuild_cache: bool,
    /// Build one archive per group of sources (e.g. per country folder) in
    /// this directory, reusing archives whose inputs are unchanged, then
    /// merge them into OUTPUT. The temp cache only ever holds one group.
    #[arg(long, value_name = "DIR")]
    shards: Option<std::path::PathBuf>,
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
                lines: vec![deduped],
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
        Cmd::Compare(a) => run_compare(a),
        Cmd::Merge(a) => run_merge(a),
    }
}

fn run_tile(args: TileArgs) -> anyhow::Result<()> {
    use corrie::build::{tile_sources, Cache};
    use corrie::config::{parse, parse_and_resolve};

    if let Some(w) = args.workers {
        rayon::ThreadPoolBuilder::new().num_threads(w).build_global()
            .context("rayon")?;
    }
    corrie::cleanup::check_output(&args.output)?;
    let cfg = parse(&args.config)?;
    let sources = parse_and_resolve(&args.config)?;
    // All sources go into one MVT layer: the output layer of the first
    // source in config order (mapriot: "contour").
    let layer = sources.first().context("no layers in config")?.output_layer_name.clone();
    let t0 = std::time::Instant::now();

    match &args.shards {
        Some(shard_dir) => {
            if args.cache_dir.is_some() {
                anyhow::bail!("--shards builds each group with its own temp cache; drop --cache-dir");
            }
            let base_dir = args.config.parent().unwrap_or(std::path::Path::new("."));
            let s = corrie::shards::build(&sources, &cfg.settings, &layer, base_dir, shard_dir, &args.output)?;
            corrie::log!("Done in {:.1}s: {} groups ({} built, {} reused), {} tiles",
                t0.elapsed().as_secs_f64(), s.groups, s.built, s.reused, s.tiles);
        }
        None => {
            let cache = match &args.cache_dir {
                Some(dir) => Cache::Persistent { dir, rebuild: args.rebuild_cache },
                None => Cache::Ephemeral,
            };
            tile_sources(&sources, &cfg.settings, &layer, &args.output, cache, &[])?;
            corrie::log!("Done in {:.1}s", t0.elapsed().as_secs_f64());
        }
    }
    Ok(())
}

fn run_merge(args: MergeArgs) -> anyhow::Result<()> {
    if let Some(w) = args.workers {
        rayon::ThreadPoolBuilder::new().num_threads(w).build_global().context("rayon")?;
    }
    let t0 = std::time::Instant::now();
    corrie::cleanup::check_output(&args.output)?;
    let merge = corrie::merge::Merge::open(&args.inputs)?;
    let stats = merge.write(&args.output, "elevation")?;
    corrie::log!("Merged {} archives: {} tiles, {} present in several inputs, in {:.1}s",
        args.inputs.len(), stats.tiles, stats.merged, t0.elapsed().as_secs_f64());
    Ok(())
}

fn run_compare(args: CompareArgs) -> anyhow::Result<()> {
    use corrie::container::pmtiles::{gunzip, tile_id_to_zxy, PmtilesReader, TileEntry};
    use std::collections::BTreeMap;

    let ra = PmtilesReader::open(&args.a)?;
    let rb = PmtilesReader::open(&args.b)?;
    let ea = ra.entries()?;
    let eb = rb.entries()?;

    // Merge-join both entry lists (each sorted by tile_id).
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let mut both: Vec<(TileEntry, TileEntry)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < ea.len() || j < eb.len() {
        match (ea.get(i), eb.get(j)) {
            (Some(a), Some(b)) if a.tile_id == b.tile_id => { both.push((*a, *b)); i += 1; j += 1; }
            (Some(a), Some(b)) if a.tile_id < b.tile_id => { only_a.push(a.tile_id); i += 1; }
            (Some(_), Some(b)) => { only_b.push(b.tile_id); j += 1; }
            (Some(a), None) => { only_a.push(a.tile_id); i += 1; }
            (None, Some(b)) => { only_b.push(b.tile_id); j += 1; }
            (None, None) => unreachable!(),
        }
    }

    // Canonical tile content: lines grouped by attributes, each group sorted.
    type Canon = BTreeMap<(i32, bool, bool), Vec<Vec<(i32, i32)>>>;
    let canon = |raw: &[u8]| -> anyhow::Result<Canon> {
        let mut m = Canon::new();
        for f in corrie::mvt::decode_tile(&gunzip(raw)?, "elevation")? {
            m.entry((f.elevation, f.main, f.show)).or_default().extend(f.lines);
        }
        for lines in m.values_mut() { lines.sort(); }
        Ok(m)
    };
    let count_pts = |c: &Canon| -> usize { c.values().flatten().map(|l| l.len()).sum() };

    // Multiset inclusion of lines, group by group: is every line of `small` also in `big`?
    let contains = |big: &Canon, small: &Canon| -> bool {
        small.iter().all(|(k, ls)| {
            let Some(bl) = big.get(k) else { return false };
            let mut bi = bl.iter().peekable();
            ls.iter().all(|l| {
                while bi.peek().is_some_and(|b| *b < l) { bi.next(); }
                bi.next_if(|b| *b == l).is_some()
            })
        })
    };
    // How a differing tile differs: a has extra lines, b has extra lines, or lines changed.
    #[derive(Clone, Copy, PartialEq)]
    enum Diff { AExtra, BExtra, Changed }
    let results: Vec<anyhow::Result<Option<(u64, usize, usize, Diff)>>> = both
        .par_iter()
        .map(|(a, b)| {
            let (ba, bb) = (ra.tile_bytes(a), rb.tile_bytes(b));
            if ba == bb { return Ok(None); }
            let (ca, cb) = (canon(ba)?, canon(bb)?);
            if ca == cb { return Ok(None); }
            let kind = if contains(&ca, &cb) { Diff::AExtra } else if contains(&cb, &ca) { Diff::BExtra } else { Diff::Changed };
            Ok(Some((a.tile_id, count_pts(&ca), count_pts(&cb), kind)))
        })
        .collect();
    let mut differing = Vec::new();
    for r in results { if let Some(d) = r? { differing.push(d); } }

    let zxy = |id: u64| { let (z, x, y) = tile_id_to_zxy(id); format!("{z}/{x}/{y}") };
    out!("tiles: a={} b={} common={}", ea.len(), eb.len(), both.len());
    out!("only in a: {}{}", only_a.len(),
        only_a.iter().take(args.show).map(|&id| format!(" {}", zxy(id))).collect::<String>());
    out!("only in b: {}{}", only_b.len(),
        only_b.iter().take(args.show).map(|&id| format!(" {}", zxy(id))).collect::<String>());
    out!("common tiles with different lines: {}", differing.len());
    for (kind, label) in [
        (Diff::AExtra, "a has every line of b, plus more"),
        (Diff::BExtra, "b has every line of a, plus more"),
        (Diff::Changed, "lines changed (neither contains the other)"),
    ] {
        let of_kind: Vec<_> = differing.iter().filter(|d| d.3 == kind).collect();
        if of_kind.is_empty() { continue; }
        out!("  {label}: {}", of_kind.len());
        for (id, pa, pb, _) in of_kind.iter().take(args.show) {
            out!("    {}  points a={} b={}", zxy(*id), pa, pb);
        }
    }
    if only_a.is_empty() && only_b.is_empty() && differing.is_empty() {
        out!("IDENTICAL (same tiles, same lines)");
        Ok(())
    } else {
        anyhow::bail!("archives differ")
    }
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
    corrie::log!(
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
        corrie::log!("Sorting {} tiles by Hilbert ID...", tiles.len());
        use corrie::container::pmtiles::tile_id;
        tiles.sort_by_cached_key(|&(z, x, y)| {
            tile_id(z, x, y)
        });
    }

    let mut writer = Writer::open(
        &args.output,
        args.min_z,
        args.max_z,
        (west_lon, south_lat, east_lon, north_lat),
        &args.layer,
        &args.elevation_attr,
        tiles.len() as u64,
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
                    corrie::log!("Warning: tile {}/{}/{} failed: {:#}", z, x, y, e);
                    n_errors += 1;
                }
            }
        }
    }

    pb.finish_with_message("done");
    corrie::log!("{} non-empty tiles written", n_written);
    if n_errors > 0 {
        corrie::log!("Warning: {} tiles failed and were skipped", n_errors);
    }

    writer.finalize().context("finalize")?;

    let sz = std::fs::metadata(&args.output)?.len();
    corrie::log!(
        "Written {:?}  ({:.1} MB)",
        args.output,
        sz as f64 / 1_048_576.0
    );
    Ok(())
}
