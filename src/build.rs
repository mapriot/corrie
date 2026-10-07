//! One tiling run: SHP sources → feature store cache → clustered PMTiles.
//! Used by `corrie tile` directly, and once per group by `shards`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rayon::prelude::*;

use crate::cleanup::PartialOutput;
use crate::config::{Settings, SourceLayer};
use crate::container::pmtiles::{gzip_tile, tile_id, PmtilesWriter};
use crate::feature_store::{cache_valid, Reader};
use crate::spatial_index::load_rtree;
use crate::tile_gen::{enumerate_tiles_pruned, hilbert_sort, process_tile};

/// Where the feature store cache lives during a run.
pub enum Cache<'a> {
    /// A temp dir deleted when the run ends (or is stopped with a signal).
    Ephemeral,
    /// A kept directory, reused while its SHPs are unchanged.
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
    // Hold the TempDir for the whole run so the cache survives until tile
    // generation completes.
    let _temp_cache: Option<tempfile::TempDir>;
    let (cache_dir, persistent, rebuild): (PathBuf, bool, bool) = match cache {
        Cache::Persistent { dir, rebuild } => {
            _temp_cache = None;
            (dir.to_path_buf(), true, rebuild)
        }
        Cache::Ephemeral => {
            crate::temp_cache::reap_orphan_caches();
            let td = crate::temp_cache::create_ephemeral_cache()?;
            crate::cleanup::remove_on_signal(td.path())?;
            let path = td.path().to_path_buf();
            _temp_cache = Some(td);
            (path, false, false)
        }
    };

    let shp_paths: Vec<PathBuf> = sources.iter().map(|s| s.shp_path.clone()).collect();
    let mut stats = TileStats { features: 0, vertices: 0, tiles: 0 };
    if !persistent || rebuild || !cache_valid(&cache_dir, &shp_paths) {
        if persistent {
            crate::log!("Ingesting {} SHPs into {:?} ...", shp_paths.len(), cache_dir);
        } else {
            crate::log!("Ingesting {} SHPs (temp cache, removed at the end) ...", shp_paths.len());
        }
        let t0 = std::time::Instant::now();
        let s = crate::ingest::run(sources, &cache_dir)?;
        crate::log!("Ingested {} features ({} vertices) in {:.2}s",
            s.features, s.vertices, t0.elapsed().as_secs_f64());
        (stats.features, stats.vertices) = (s.features, s.vertices);
    } else {
        crate::log!("Using cached feature store in {:?}", cache_dir);
    }

    let reader = Reader::open(&cache_dir)?;
    let rtree = load_rtree(&cache_dir)?;

    // Only tiles that hold data, within the lowest-zoom tiles that intersect
    // the config's bounding box (mapriot's is the whole world).
    let mut tiles = enumerate_tiles_pruned(&rtree, settings.bounding_box, settings.minzoom, settings.maxzoom);
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
                let r = process_tile(t, &reader, &rtree, sources, layer_name, "elevation")
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
