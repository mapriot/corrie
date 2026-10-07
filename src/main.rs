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
#[command(name = "corrie", version, about = "Contour vector tiles: contour lines or a DEM to clustered PMTiles")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Tile the contour sources (packs or SHPs) of a tilemaker config.json into clustered PMTiles
    Tile(TileArgs),
    /// Pack contour sources (SHP, FlatGeobuf, …) into .cpk files that tile reads directly
    Pack(PackArgs),
    /// Write packs out as lines for QGIS (FlatGeobuf, GeoPackage, GeoJSON or SHP)
    Unpack(UnpackArgs),
    /// Merge contour PMTiles (e.g. one per country) into one clustered archive
    Merge(MergeArgs),
    /// Check that two contour PMTiles hold the same tiles and lines
    ///
    /// Tiles are matched by z/x/y, decoded, and their lines compared per
    /// (elevation, main, show) group; line order inside a tile is ignored.
    /// A tile that differs is reported as "a has every line of b, plus more",
    /// the reverse, or "lines changed". Exits 0 when both are identical, or
    /// with --tolerance when every difference is within it.
    Compare(CompareArgs),
    /// Contour a single DEM raster straight into PMTiles or MBTiles (no shapefiles)
    Build(BuildArgs),
}

#[derive(clap::Args, Debug)]
struct MergeArgs {
    /// Output .pmtiles, replaced once the new file is complete
    output: std::path::PathBuf,
    /// Input .pmtiles archives, as written by `corrie tile`. They must all
    /// use the same layer name; the output may be one of them.
    #[arg(required = true)]
    inputs: Vec<std::path::PathBuf>,
    /// Worker threads [default: all cores]
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

#[derive(clap::Args, Debug)]
struct PackArgs {
    /// Source files (SHP, FlatGeobuf, GeoPackage, anything GDAL reads); each
    /// becomes <name>.cpk next to it
    #[arg(required_unless_present = "config")]
    inputs: Vec<std::path::PathBuf>,
    /// Pack every source of this tilemaker config next to its file instead,
    /// and write a copy of the config whose sources are the packs
    #[arg(long, value_name = "CONFIG", conflicts_with = "inputs")]
    config: Option<std::path::PathBuf>,
    /// Where the config copy goes [default: <config>-packs.json next to it];
    /// may be the config itself
    #[arg(long, value_name = "FILE", requires = "config")]
    out_config: Option<std::path::PathBuf>,
    /// With --config: only these groups, comma-separated, named as --shards
    /// names them (the country folder: cz for contours/cz/data/x.shp). The
    /// config copy switches only their sources to packs.
    #[arg(long, value_name = "GROUPS", value_delimiter = ',', requires = "config")]
    only: Vec<String>,
    /// Elevation field of the inputs (with --config: each layer's
    /// source_columns, or elevation)
    #[arg(long, default_value = "elevation")]
    field: String,
    /// Pack again even when an up-to-date pack exists
    #[arg(long)]
    force: bool,
    /// Worker threads [default: all cores]
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

#[derive(clap::Args, Debug)]
struct UnpackArgs {
    /// Output file; the format follows the extension: .fgb, .gpkg, .geojson or .shp
    output: std::path::PathBuf,
    /// Packs to write (e.g. a country and its neighbours)
    #[arg(required = true)]
    inputs: Vec<std::path::PathBuf>,
    /// Only lines touching this box: min_lon,min_lat,max_lon,max_lat
    #[arg(long, value_name = "BOX", allow_hyphen_values = true)]
    bbox: Option<String>,
}

#[derive(clap::Args, Debug)]
struct CompareArgs {
    /// First archive (e.g. the live one)
    a: std::path::PathBuf,
    /// Second archive (e.g. a new build)
    b: std::path::PathBuf,
    /// How many tile ids to list per kind of difference
    #[arg(long, default_value = "10")]
    show: usize,
    /// Count lines as the same when every vertex inside the tile lies within
    /// this many tile pixels of the other side's lines (e.g. builds by
    /// different corrie versions). A tile is 4096 tile pixels and is drawn
    /// 512 screen pixels wide: 8 tile pixels = 1 screen pixel
    #[arg(long, value_name = "PX")]
    tolerance: Option<f64>,
    /// Also write every differing tile, with its kind and deviation, as JSON
    #[arg(long, value_name = "FILE")]
    json: Option<std::path::PathBuf>,
}

#[derive(clap::Args, Debug)]
struct BuildArgs {
    /// Input elevation raster — GeoTIFF, VRT, or any GDAL-supported format and CRS.
    input: std::path::PathBuf,
    /// Output file — .pmtiles or .mbtiles
    output: std::path::PathBuf,
    /// Lowest zoom to write
    #[arg(long, default_value = "11")]
    min_z: u8,
    /// Highest zoom to write
    #[arg(long, default_value = "14")]
    max_z: u8,
    /// Contour intervals as ZOOM:INTERVAL pairs; each interval (in metres)
    /// is drawn from its zoom upwards
    #[arg(long, default_value = "11:50,12:20,13:10,14:5")]
    levels: String,
    /// Tag elevations divisible by N as main=1 (0 = never)
    #[arg(long, default_value = "50")]
    main_every: i32,
    /// Tag elevations divisible by N as show=1 (0 = never)
    #[arg(long, default_value = "100")]
    show_every: i32,
    /// MVT layer name
    #[arg(long, default_value = "contour")]
    layer: String,
    /// MVT attribute name for the elevation
    #[arg(long, default_value = "elevation")]
    elevation_attr: String,
    /// Nodata value [default: the raster's own]
    #[arg(long, allow_hyphen_values = true)]
    nodata: Option<f32>,
    /// Douglas-Peucker tolerance in tile pixels (0 = off)
    #[arg(long, default_value = "2.0")]
    simplify: f64,
    /// Worker threads [default: all cores]
    #[arg(short = 'j', long)]
    workers: Option<usize>,
}

#[derive(clap::Args, Debug)]
struct TileArgs {
    /// Path to a tilemaker config.json
    config: std::path::PathBuf,
    /// Output .pmtiles, replaced once the new file is complete
    output: std::path::PathBuf,
    /// Keep the packs made from sources that are not packs (SHPs, …) in this
    /// directory and reuse each while its source is unchanged. Without it,
    /// they go to a temp dir removed when the run ends or is stopped.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<std::path::PathBuf>,
    /// Pack the sources again even if their packs in --cache-dir are current
    #[arg(long)]
    rebuild_cache: bool,
    /// Tile each group of sources (each folder below the common source
    /// folder, e.g. one per country) into DIR/<group>.pmtiles, reusing archives
    /// whose inputs are unchanged, then merge them all into OUTPUT. The temp
    /// cache holds one group's packs at a time.
    #[arg(long, value_name = "DIR", conflicts_with_all = ["cache_dir", "rebuild_cache"])]
    shards: Option<std::path::PathBuf>,
    /// Worker threads [default: all cores]
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
        Cmd::Pack(a) => run_pack(a),
        Cmd::Unpack(a) => run_unpack(a),
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
    // All sources go into one MVT layer: the output layer of the
    // alphabetically first layer key (mapriot: "contour").
    let layer = sources.first().context("no layers in config")?.output_layer_name.clone();
    let t0 = std::time::Instant::now();

    match &args.shards {
        Some(shard_dir) => {
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
    use corrie::compare::{compare, Kind, DEV_CAP};
    use corrie::container::pmtiles::tile_id_to_zxy;

    let r = compare(&args.a, &args.b, args.tolerance)?;
    let zxy = |id: u64| { let (z, x, y) = tile_id_to_zxy(id); format!("{z}/{x}/{y}") };
    let list = |ids: &mut dyn Iterator<Item = u64>| ids.take(args.show).map(|id| format!(" {}", zxy(id))).collect::<String>();
    out!("tiles: a={} b={} common={}", r.tiles_a, r.tiles_b, r.common);
    out!("only in a: {}{}", r.only_a.len(), list(&mut r.only_a.iter().copied()));
    out!("only in b: {}{}", r.only_b.len(), list(&mut r.only_b.iter().copied()));
    let changed: Vec<_> = r.diffs.iter().filter(|d| d.kind != Kind::EdgeSliver).collect();
    out!("common tiles with different lines: {}", changed.len());
    let dev = |d: Option<f64>| match d {
        Some(v) if v >= DEV_CAP => format!(" max {DEV_CAP}+ px"),
        Some(v) => format!(" max {v:.2} px ({:.2} screen px)", v / 8.0),
        None => " (a whole elevation group on one side only)".to_string(),
    };
    for (kind, label) in [
        (Kind::WithinTolerance, "within the tolerance"),
        (Kind::EdgeSliver, "only on one side, a sliver at the tile edge (within the tolerance)"),
        (Kind::AExtra, "a has every line of b, plus more"),
        (Kind::BExtra, "b has every line of a, plus more"),
        (Kind::Changed, "lines changed (neither contains the other)"),
    ] {
        let of_kind: Vec<_> = r.diffs.iter().filter(|d| d.kind == kind).collect();
        if of_kind.is_empty() { continue; }
        let worst = of_kind.iter().filter_map(|d| d.max_dev).fold(0.0f64, f64::max);
        let summary = if args.tolerance.is_some() && worst > 0.0 {
            format!(" (largest deviation {worst:.2} tile px = {:.2} screen px)", worst / 8.0)
        } else { String::new() };
        out!("  {label}: {}{summary}", of_kind.len());
        for d in of_kind.iter().take(args.show) {
            let dv = if args.tolerance.is_some() { dev(d.max_dev) } else { String::new() };
            out!("    {}  points a={} b={}{dv}", zxy(d.tile_id), d.points_a, d.points_b);
        }
    }
    if let Some(path) = &args.json {
        let tile = |id: u64| { let (z, x, y) = tile_id_to_zxy(id); serde_json::json!({"z": z, "x": x, "y": y}) };
        let doc = serde_json::json!({
            "a": args.a, "b": args.b, "tolerance": args.tolerance,
            "tiles_a": r.tiles_a, "tiles_b": r.tiles_b, "common": r.common,
            "only_a": r.only_a.iter().map(|&id| tile(id)).collect::<Vec<_>>(),
            "only_b": r.only_b.iter().map(|&id| tile(id)).collect::<Vec<_>>(),
            "diffs": r.diffs.iter().map(|d| {
                let (z, x, y) = tile_id_to_zxy(d.tile_id);
                serde_json::json!({"z": z, "x": x, "y": y, "kind": d.kind, "points_a": d.points_a, "points_b": d.points_b, "max_dev": d.max_dev})
            }).collect::<Vec<_>>(),
        });
        std::fs::write(path, serde_json::to_vec_pretty(&doc)?).with_context(|| format!("write {:?}", path))?;
    }
    if r.matches() {
        if r.diffs.is_empty() {
            out!("IDENTICAL (same tiles, same lines)");
        } else {
            out!("SAME within {} px (same tiles, lines within the tolerance)", args.tolerance.unwrap_or(0.0));
        }
        Ok(())
    } else {
        anyhow::bail!("archives differ")
    }
}

fn run_pack(args: PackArgs) -> anyhow::Result<()> {
    use corrie::pack::{is_current, pack_files};
    use std::path::{Path, PathBuf};

    if let Some(w) = args.workers {
        rayon::ThreadPoolBuilder::new().num_threads(w).build_global().context("rayon")?;
    }
    // (source, elevation field, pack) for every source to pack, and with
    // --config the layers whose source turns into its pack
    let mut all: Vec<(PathBuf, String, PathBuf)> = Vec::new();
    let mut switched: Vec<String> = Vec::new();
    match &args.config {
        Some(cfg) => {
            let sources = corrie::config::parse_and_resolve(cfg)?;
            let chosen = if args.only.is_empty() { sources } else {
                // the groups --shards builds, so the names match its archives
                let groups = corrie::shards::group_sources(&sources)?;
                for name in &args.only {
                    if !groups.iter().any(|g| &g.name == name) {
                        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
                        anyhow::bail!("--only: no group {:?} in {:?} (groups: {})", name, cfg, names.join(", "));
                    }
                }
                groups.into_iter().filter(|g| args.only.contains(&g.name)).flat_map(|g| g.sources).collect()
            };
            for s in chosen {
                if corrie::build::is_pack(&s.shp_path) { continue; }
                let field = s.source_columns.first().cloned().unwrap_or_else(|| "elevation".to_string());
                all.push((s.shp_path.clone(), field, s.shp_path.with_extension("cpk")));
                switched.push(s.source_layer_name);
            }
        }
        None => {
            for p in &args.inputs {
                all.push((p.clone(), args.field.clone(), p.with_extension("cpk")));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (src, _, out) in &all {
        if !seen.insert(out.clone()) {
            anyhow::bail!("two sources would write {:?} (e.g. {:?}); rename one", out, src);
        }
    }
    let todo: Vec<_> = all.iter().filter(|(src, field, out)| args.force || !is_current(out, src, field)).cloned().collect();
    let t0 = std::time::Instant::now();
    if !todo.is_empty() {
        corrie::log!("Packing {} sources ({} already up to date) ...", todo.len(), all.len() - todo.len());
        pack_files(&todo)?;
    }
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let source_bytes: u64 = all.iter().map(|(src, _, _)| {
        if src.extension().is_some_and(|e| e.eq_ignore_ascii_case("shp")) {
            ["shp", "shx", "dbf"].iter().map(|e| size(&src.with_extension(e))).sum()
        } else { size(src) }
    }).sum();
    let pack_bytes: u64 = all.iter().map(|(_, _, out)| size(out)).sum();
    corrie::log!("{} packs: {:.2} GB of sources -> {:.2} GB of packs ({:.1}x smaller), {:.1}s",
        all.len(), source_bytes as f64 / 1e9, pack_bytes as f64 / 1e9,
        source_bytes as f64 / pack_bytes.max(1) as f64, t0.elapsed().as_secs_f64());

    if let Some(cfg) = &args.config {
        let out = args.out_config.clone().unwrap_or_else(|| {
            let stem = cfg.file_stem().unwrap_or_default().to_string_lossy();
            cfg.with_file_name(format!("{stem}-packs.json"))
        });
        let text = std::fs::read_to_string(cfg).with_context(|| format!("read {:?}", cfg))?;
        let text = switch_sources(&text, &switched).with_context(|| format!("rewrite {:?}", cfg))?;
        let o = corrie::cleanup::PartialOutput::new(&out)?;
        std::fs::write(o.path(), text).with_context(|| format!("write {:?}", o.path()))?;
        o.commit()?;
        corrie::log!("Wrote {:?}: {} sources now read their packs", out, switched.len());
    }
    Ok(())
}

/// The config text with the source of each layer in `layers` ending in .cpk
/// instead of its extension. Only those strings change, so the copy keeps
/// the original's layout and a diff shows just the switched sources.
fn switch_sources(text: &str, layers: &[String]) -> anyhow::Result<String> {
    use std::path::Path;
    let doc: serde_json::Value = serde_json::from_str(text)?;
    let mut expected = doc.clone();
    let mut out = text.to_string();
    for name in layers {
        let src = doc["layers"][name]["source"].as_str()
            .with_context(|| format!("layer {name:?} has no source"))?;
        let packed = Path::new(src).with_extension("cpk").to_string_lossy().into_owned();
        let (from, to) = (serde_json::to_string(src)?, serde_json::to_string(&packed)?);
        match out.matches(&from).count() {
            1 => out = out.replacen(&from, &to, 1),
            n => anyhow::bail!("source {src:?} of layer {name:?} appears {n} times in the text; switch it by hand"),
        }
        expected["layers"][name]["source"] = packed.into();
    }
    // the text edit changed exactly those sources
    let got: serde_json::Value = serde_json::from_str(&out)?;
    anyhow::ensure!(got == expected, "the edited config does not parse back as expected");
    Ok(out)
}

fn run_unpack(args: UnpackArgs) -> anyhow::Result<()> {
    let bbox = match &args.bbox {
        None => None,
        Some(s) => {
            let v: Vec<f64> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()
                .context("--bbox: expected min_lon,min_lat,max_lon,max_lat")?;
            if v.len() != 4 { anyhow::bail!("--bbox: expected four numbers"); }
            Some([v[0], v[1], v[2], v[3]])
        }
    };
    let t0 = std::time::Instant::now();
    let s = corrie::unpack::unpack(&args.inputs, &args.output, bbox)?;
    corrie::log!("Wrote {} lines ({} vertices) to {:?} in {:.1}s", s.lines, s.vertices, args.output, t0.elapsed().as_secs_f64());
    Ok(())
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
