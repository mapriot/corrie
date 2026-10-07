//! Phase 1 driver: parallel-read SHPs, arrival-order write to feature store.
//! Produces `features.bin` + `coords.bin` + `rtree.bin` + `manifest.bin`.
//!
//! Concurrency model: rayon parallel-iterates SHPs across all available
//! workers; each worker reads its SHP into a per-SHP `Vec<ReadFeature>`
//! buffer (no streaming inside an SHP) and sends the whole buffer as a
//! single `ShpResult` message. The writer (main thread) appends each
//! message's features to the on-disk builder as they arrive.
//!
//! Memory model: peak RAM during ingest is bounded by `(num_workers +
//! chan_cap) × max_shp_in_memory`. For a 480-SHP planet build at
//! `-j 10`, that's roughly 20–30 GB peak — well under the prod-box
//! 100 GB cgroup. The index adds to that: 24 B per indexed chunk while
//! reading, then the R-tree built from it (~50 B per chunk) next to the
//! list. Contour data has ~2.3 chunks per line (Madagascar).
//!
//! Ordering: features land in `features.bin` / `coords.bin` in whichever
//! order rayon workers finish their SHPs. That order is NOT deterministic
//! across runs (work-stealing scheduling varies). PMTiles output is
//! visually identical across runs — each tile contains the same set of
//! features — but MVT byte layout inside tiles can differ, so the final
//! `.pmtiles` file is not byte-reproducible.
//!
//! Earlier versions of this driver buffered out-of-order arrivals in a
//! `BTreeMap` to preserve config order. That buffer grew unboundedly on
//! large datasets and caused cgroup-OOM around the half-way mark of
//! planet ingest when a slow head-of-line SHP let dozens of GBs pile up
//! behind it. Dropping the in-order constraint trades cross-run byte
//! reproducibility for hard memory bounds.

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::sync_channel;

use crate::attribute_rules::derive_attributes;
use crate::config::SourceLayer;
use crate::feature_store::{Builder, Manifest, compute_source_hash, write_manifest, MANIFEST_VERSION};
use crate::shapefile_read::{read_polylines_streaming, ReadFeature};
use crate::spatial_index::{build_rtree, push_chunk_bboxes, save_rtree, FeatureBbox};

pub struct IngestStats {
    pub features: u64,
    pub vertices: u64,
}

/// One reader worker's complete output: all features parsed from one SHP.
/// Sent as a single message so the writer doesn't need per-feature tagging.
struct ShpResult {
    features: Vec<(ReadFeature, u16)>, // (feature, layer_id)
}

/// Stream all SHPs in parallel, write the feature store on a single writer.
pub fn run(
    sources: &[SourceLayer],
    cache_dir: &Path,
) -> Result<IngestStats> {
    std::fs::create_dir_all(cache_dir).with_context(|| format!("mkdir {:?}", cache_dir))?;
    // The manifest is what marks a cache as complete and valid. Drop the old
    // one first, so an ingest that is interrupted halfway through rewriting a
    // persistent cache leaves it invalid instead of half-new under a
    // still-matching manifest.
    match std::fs::remove_file(cache_dir.join("manifest.bin")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context("remove old manifest.bin"),
        _ => {}
    }
    // Hash the SHP set (paths, sizes, mtimes) before reading it: an SHP that
    // changes while it is being read then no longer matches the manifest.
    let shp_paths: Vec<PathBuf> = sources.iter().map(|s| s.shp_path.clone()).collect();
    let source_hash = compute_source_hash(&shp_paths)?;

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
        // single Vec, and sends one ShpResult per SHP.
        let inputs_ref = &parallel_inputs;
        let pb_ref = &pb;
        let read_handle = scope.spawn(move || -> Result<()> {
            inputs_ref.par_iter().try_for_each(|(lid, s, col)| -> Result<()> {
                let lid = *lid;
                let mut shp_features: Vec<(ReadFeature, u16)> = Vec::new();
                read_polylines_streaming(&s.shp_path, col, |feature| {
                    shp_features.push((feature, lid));
                    Ok(())
                }).with_context(|| format!("read {:?}", s.shp_path))?;
                tx.send(ShpResult { features: shp_features })
                    .map_err(|_| anyhow::anyhow!("writer thread terminated"))?;
                pb_ref.inc(1);
                Ok(())
            })?;
            // Drop `tx` so the channel closes once all readers are done.
            drop(tx);
            Ok(())
        });

        // Writer (main thread). Drain SHPs in arrival order — no out-of-order
        // buffer. The IIFE lets us drop `rx` explicitly afterwards so any
        // worker still blocked on send unblocks via SendError; otherwise
        // std::thread::scope would hang trying to join workers waiting on a
        // receiver that's still in scope.
        let writer_result: Result<()> = (|| -> Result<()> {
            for result in rx.iter() {
                for (f, lid) in result.features {
                    let (minzoom, flags) = derive_attributes(f.elevation as i32);
                    builder.add_feature(&f.coords, f.elevation, minzoom, flags, lid)?;
                    push_chunk_bboxes(idx, &f.coords, &mut bbox_entries);
                    idx = idx.checked_add(1).context("feature count exceeds u32::MAX")?;
                }
            }
            Ok(())
        })();
        drop(rx);

        // Surface any error from the reader pool (also catches the case where
        // the writer terminated early and readers got a SendError). Writer
        // error wins over reader error since the writer error is usually the
        // root cause and the SendError is consequential.
        let join_result: Result<()> = match read_handle.join() {
            Ok(r) => r,
            Err(_) => anyhow::bail!("reader thread panicked"),
        };
        writer_result.and(join_result)
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
