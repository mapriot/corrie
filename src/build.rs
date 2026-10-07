//! One tiling run: sources → packs → clustered PMTiles.
//! Used by `corrie tile` directly, and once per group by `shards`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::cleanup::PartialOutput;
use crate::config::{Settings, SourceLayer};
use crate::container::pmtiles::{gzip_tile, tile_id, PmtilesWriter};
use crate::pack::{is_current, pack_files, Pack, PackOptions};
use crate::tile_gen::{enumerate_tiles_pruned, hilbert_sort, process_tile};

/// Where packs of the sources that are not packs already (SHP, FlatGeobuf,
/// GeoPackage, …) are written for a run.
pub enum Cache<'a> {
    /// A temp dir deleted when the run ends (or is stopped with a signal).
    Ephemeral,
    /// A kept directory: a source's pack is reused while the source's size
    /// and modification time are unchanged.
    Persistent { dir: &'a Path, rebuild: bool },
}

pub struct TileStats {
    pub features: u64,
    pub vertices: u64,
    pub tiles: u64,
}

/// Tile `sources` into `output`, all into one MVT layer named `layer_name`.
/// `metadata` adds top-level fields to the archive's JSON metadata. The
/// output is written as `<output>.partial` and renamed when complete.
pub fn tile_sources(
    sources: &[SourceLayer],
    settings: &Settings,
    layer_name: &str,
    output: &Path,
    cache: Cache,
    metadata: &[(&str, serde_json::Value)],
) -> Result<TileStats> {
    // Hold the TempDir for the whole run so the packs survive until tile
    // generation completes.
    let _temp_cache: Option<tempfile::TempDir>;
    let (cache_dir, reuse): (PathBuf, bool) = match cache {
        Cache::Persistent { dir, rebuild } => {
            _temp_cache = None;
            (dir.to_path_buf(), !rebuild)
        }
        Cache::Ephemeral => {
            crate::temp_cache::reap_orphan_caches();
            let td = crate::temp_cache::create_ephemeral_cache()?;
            crate::cleanup::remove_on_signal(td.path())?;
            let path = td.path().to_path_buf();
            _temp_cache = Some(td);
            (path, false)
        }
    };

    let packs = packs_for(sources, &cache_dir, reuse)?;
    let mut stats = TileStats {
        features: packs.iter().map(|p| p.header().n_lines).sum(),
        vertices: packs.iter().map(|p| p.header().n_vertices).sum(),
        tiles: 0,
    };

    // Only tiles that hold data, within the lowest-zoom tiles that intersect
    // the config's bounding box (mapriot's is the whole world).
    crate::log!("Listing tiles ...");
    let mut tiles = enumerate_tiles_pruned(&packs, settings.bounding_box, settings.minzoom, settings.maxzoom);
    if tiles.is_empty() {
        crate::log!("No data within the config's bounding_box; nothing to do.");
    }
    crate::log!("{} tiles to process (quadtree-pruned)", tiles.len());
    hilbert_sort(&mut tiles);

    let [w, s, e, n] = settings.bounding_box;
    let out = PartialOutput::new(output)?;
    let mut writer = PmtilesWriter::create(
        out.path(), settings.minzoom, settings.maxzoom,
        (w, s, e, n), layer_name, "elevation", tiles.len() as u64,
    )?;
    for (k, v) in metadata {
        writer.set_metadata(k, v.clone());
    }

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

    // Workers build and gzip tiles in parallel; the main thread appends them
    // in Hilbert order (clustered PMTiles needs strictly increasing tile_id).
    const CHUNK: usize = 4096;
    for chunk in tiles.chunks(CHUNK) {
        let results: Vec<Result<Option<Vec<u8>>>> = chunk
            .par_iter()
            .map(|&t| {
                let r = process_tile(t, &packs, sources, layer_name, "elevation")
                    .map(|mvt| gzip_tile(&mvt))
                    .transpose();
                pb.inc(1);
                r
            })
            .collect();
        for (i, gz) in results.into_iter().enumerate() {
            if let Some(gz) = gz? {
                let t = chunk[i];
                writer.add_tile_gzipped(tile_id(t.z, t.x, t.y), &gz)?;
                stats.tiles += 1;
            }
        }
    }
    pb.finish();
    writer.finalize()?;
    out.commit()?;
    crate::log!("Wrote {} non-empty tiles", stats.tiles);
    if let Some(td) = _temp_cache {
        let path = td.path().to_path_buf();
        drop(td);
        crate::cleanup::forget_on_signal(&path);
    }
    Ok(stats)
}

/// Is this source a corrie pack (used as it is)?
pub fn is_pack(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cpk"))
}

/// One pack per source, in source order. Pack sources are opened as they
/// are; any other source is packed into `cache_dir` (in parallel), or with
/// `reuse` its earlier pack there is used while the source is unchanged.
pub fn packs_for(sources: &[SourceLayer], cache_dir: &Path, reuse: bool) -> Result<Vec<Pack>> {
    std::fs::create_dir_all(cache_dir).with_context(|| format!("create {:?}", cache_dir))?;
    let field = |s: &SourceLayer| s.source_columns.first().cloned().unwrap_or_else(|| "elevation".to_string());
    let paths: Vec<PathBuf> = sources.iter().map(|s| {
        if is_pack(&s.shp_path) { s.shp_path.clone() } else { cache_dir.join(cached_pack_name(&s.shp_path)) }
    }).collect();
    let todo: Vec<usize> = (0..sources.len())
        .filter(|&i| !is_pack(&sources[i].shp_path))
        .filter(|&i| !(reuse && is_current(&paths[i], &sources[i].shp_path, &field(&sources[i]), &PackOptions::default())))
        .collect();

    if !todo.is_empty() {
        crate::log!("Packing {} of {} sources into {:?} ...", todo.len(), sources.len(), cache_dir);
        let t0 = std::time::Instant::now();
        let jobs: Vec<(PathBuf, String, PathBuf)> = todo.iter()
            .map(|&i| (sources[i].shp_path.clone(), field(&sources[i]), paths[i].clone()))
            .collect();
        pack_files(&jobs, &PackOptions::default())?;
        crate::log!("Packed {} sources in {:.1}s", todo.len(), t0.elapsed().as_secs_f64());
    } else if sources.iter().any(|s| !is_pack(&s.shp_path)) {
        crate::log!("Using the packs in {:?}", cache_dir);
    }
    paths.iter().map(|p| Pack::open(p)).collect()
}

/// The cache file name of a source's pack: its stem plus a hash of its full
/// path, so two sources with the same file name never share a pack.
fn cached_pack_name(source: &Path) -> String {
    let full = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let h = Sha256::digest(full.to_string_lossy().as_bytes());
    let stem = source.file_stem().unwrap_or_default().to_string_lossy();
    format!("{}-{:02x}{:02x}{:02x}{:02x}.cpk", stem, h[0], h[1], h[2], h[3])
}
