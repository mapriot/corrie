//! Phase 1 driver: parallel-read SHPs, ordered-write to feature store.
//! Produces `features.bin` + `coords.bin` + `rtree.bin` + `manifest.bin`.
//!
//! Concurrency model: rayon parallel-iterates SHPs across all available
//! workers; each worker reads its SHP into a per-SHP `Vec<ReadFeature>`
//! buffer (no streaming inside an SHP) and sends the whole buffer as a
//! single `ShpResult` message. The writer (main thread) drains pending
//! results from a small `BTreeMap`, pulling the next-expected SHP index
//! whenever it becomes available and appending its features to the
//! on-disk builder in order.
//!
//! Memory model: peak RAM during ingest is bounded by `(num_workers +
//! pending) × max_shp_features × per-feature-bytes`. For the mapriot
//! planet dataset the worst case is a few tens of GB transiently — well
//! under the prod-box 100 GB cgroup. Steady-state RAM is the R-tree bbox
//! vec only (~24 B/feature).
//!
//! Ordering is deterministic: SHPs are appended to features.bin/coords.bin
//! in config order, regardless of which reader worker finished first.
//! Features within an SHP arrive in shapefile-file order from the GDAL
//! reader. The resulting feature store and R-tree are byte-reproducible
//! across runs of the same binary on the same inputs.

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;

use crate::attribute_rules::derive_attributes;
use crate::config::SourceLayer;
use crate::feature_store::{Builder, Manifest, compute_source_hash, write_manifest, MANIFEST_VERSION};
use crate::shapefile_read::{read_polylines_streaming, ReadFeature};
use crate::spatial_index::{build_rtree, save_rtree, FeatureBbox};

pub struct IngestStats {
    pub features: u64,
    pub vertices: u64,
}

/// One reader worker's complete output: all features parsed from one SHP.
/// Sent as a single message so the writer can drain SHPs atomically in
/// config order without per-feature tagging.
struct ShpResult {
    shp_index: u16,
    features: Vec<(ReadFeature, u16)>, // (feature, layer_id)
}

/// Stream all SHPs in parallel, write the feature store on a single writer.
pub fn run(
    sources: &[SourceLayer],
    cache_dir: &Path,
) -> Result<IngestStats> {
    std::fs::create_dir_all(cache_dir).with_context(|| format!("mkdir {:?}", cache_dir))?;

    // Build elevation-column lookup per source (default "elevation"; use first
    // entry of source_columns if explicit).
    let parallel_inputs: Vec<(u16, &SourceLayer, String)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let col = s.source_columns.first().cloned().unwrap_or_else(|| "elevation".to_string());
            (i as u16, s, col)
        })
        .collect();

    // Bounded channel — one slot per worker plus a small buffer. Each
    // message is an entire SHP's features (potentially hundreds of MB),
    // so we don't want many in flight.
    let chan_cap = (rayon::current_num_threads() + 2).max(2);
    let (tx, rx) = sync_channel::<ShpResult>(chan_cap);

    let mut builder = Builder::create(cache_dir)?;
    let mut bbox_entries: Vec<FeatureBbox> = Vec::new();
    let mut idx: u32 = 0;

    // Per-SHP progress bar. Ticks once per SHP fully read by the parallel
    // reader pool. Same template as the tile-generation bar so the two
    // phases look consistent. indicatif degrades cleanly to one line per
    // update when stderr is not a TTY (e.g. piped through `tee`).
    let pb = ProgressBar::new(parallel_inputs.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:45.green/blue} {pos:>4}/{len} SHPs  {shps_per_sec}/s  eta {eta}",
        )
        .unwrap()
        .with_key(
            "shps_per_sec",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{}", state.per_sec() as u64).unwrap();
            },
        ),
    );

    // `std::thread::scope` lets the reader pool borrow `parallel_inputs`
    // without 'static, and guarantees workers join before we exit the scope.
    let read_result: Result<()> = std::thread::scope(|scope| {
        // Spawn one coordinator thread; inside it, rayon par_iter's across
        // SHPs. Each worker reads its SHP fully, packs all features into a
        // single Vec, and sends one ShpResult tagged with the SHP index.
        let inputs_ref = &parallel_inputs;
        let pb_ref = &pb;
        let read_handle = scope.spawn(move || -> Result<()> {
            inputs_ref.par_iter().enumerate().try_for_each(|(i, (lid, s, col))| -> Result<()> {
                let lid = *lid;
                let shp_idx = i as u16;
                let mut shp_features: Vec<(ReadFeature, u16)> = Vec::new();
                read_polylines_streaming(&s.shp_path, col, |feature| {
                    shp_features.push((feature, lid));
                    Ok(())
                }).with_context(|| format!("read {:?}", s.shp_path))?;
                tx.send(ShpResult { shp_index: shp_idx, features: shp_features })
                    .map_err(|_| anyhow::anyhow!("writer thread terminated"))?;
                pb_ref.inc(1);
                Ok(())
            })?;
            // Drop `tx` so the channel closes once all readers are done.
            drop(tx);
            Ok(())
        });

        // Main thread is the writer. Drain results in SHP-config order via a
        // BTreeMap that always exposes the smallest pending key, while
        // accepting messages in arrival order.
        let total_shps = inputs_ref.len();
        let mut pending: BTreeMap<u16, Vec<(ReadFeature, u16)>> = BTreeMap::new();
        let mut next_expected: u16 = 0;

        while (next_expected as usize) < total_shps {
            // Drain whatever is ready in order.
            while pending.get(&next_expected).is_some() {
                let features = pending.remove(&next_expected).unwrap();
                for (f, lid) in features {
                    let (minzoom, flags) = derive_attributes(f.elevation as i32);
                    builder.add_feature(&f.coords, f.elevation, minzoom, flags, lid)?;
                    bbox_entries.push(FeatureBbox {
                        idx,
                        min: [f.bbox_min_lon as f32, f.bbox_min_latp as f32],
                        max: [f.bbox_max_lon as f32, f.bbox_max_latp as f32],
                    });
                    idx = idx.checked_add(1).context("feature count exceeds u32::MAX")?;
                }
                next_expected += 1;
            }
            if (next_expected as usize) >= total_shps { break; }

            // Wait for another worker to deliver an SHP.
            match rx.recv() {
                Ok(result) => { pending.insert(result.shp_index, result.features); }
                Err(_) => {
                    // Channel closed before SHP next_expected arrived. Means
                    // a reader errored out; the par_iter Err will surface via
                    // read_handle.join() below.
                    break;
                }
            }
        }

        // Surface any error from the reader pool (also catches the case where
        // the writer terminated early and readers got a SendError).
        match read_handle.join() {
            Ok(r) => r,
            Err(_) => anyhow::bail!("reader thread panicked"),
        }
    });
    read_result?;
    pb.finish_and_clear();

    let n_features = builder.n_features();
    let n_vertices = builder.n_vertices();
    builder.finalize()?;

    // Build and persist R-tree.
    let tree = build_rtree(bbox_entries);
    save_rtree(&tree, cache_dir)?;

    // Manifest: source hash + layer name table.
    let shp_paths: Vec<PathBuf> = sources.iter().map(|s| s.shp_path.clone()).collect();
    let source_hash = compute_source_hash(&shp_paths)?;
    // Distinct output layer names, preserving config order.
    let mut layer_names: Vec<String> = Vec::new();
    for s in sources {
        if !layer_names.contains(&s.output_layer_name) {
            layer_names.push(s.output_layer_name.clone());
        }
    }

    let m = Manifest {
        version: MANIFEST_VERSION,
        feature_count: n_features,
        vertex_count: n_vertices,
        layer_names,
        source_hash,
    };
    write_manifest(cache_dir, &m)?;

    Ok(IngestStats { features: n_features, vertices: n_vertices })
}
