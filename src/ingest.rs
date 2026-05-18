//! Phase 1 driver: stream SHPs (parallel readers), apply attribute rules,
//! write features.bin + coords.bin + rtree.bin + manifest.bin.
//!
//! Memory model: features are never accumulated in RAM. Reader threads parse
//! one SHP each in parallel and push features over a bounded channel; the
//! main thread is the sole writer to the on-disk builder. Peak RAM is
//! bounded by the channel buffer + the R-tree bbox vec (24 B/feature), so
//! ingest of a 30M-feature planet uses ~1 GB regardless of total vertex
//! count.

use anyhow::{Context, Result};
use rayon::prelude::*;
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

/// Per-feature payload sent over the reader→writer channel.
struct StreamItem {
    layer_id: u16,
    feature: ReadFeature,
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

    // Bounded channel — backpressures readers when the writer is slow. 4096
    // slots is a few MB of features in flight; tuned for SSD write throughput.
    let (tx, rx) = sync_channel::<StreamItem>(4096);

    let mut builder = Builder::create(cache_dir)?;
    let mut bbox_entries: Vec<FeatureBbox> = Vec::new();
    let mut idx: u32 = 0;

    // `std::thread::scope` lets the reader thread borrow `parallel_inputs`
    // without 'static, and guarantees it joins before we exit the scope.
    let read_result: Result<()> = std::thread::scope(|scope| {
        // Spawn one reader-coordinator thread; inside it, rayon parallelises
        // across SHPs. Each emit sends one feature through the bounded channel.
        let inputs_ref = &parallel_inputs;
        let read_handle = scope.spawn(move || -> Result<()> {
            inputs_ref.par_iter().try_for_each(|(lid, s, col)| -> Result<()> {
                let lid = *lid;
                let tx = tx.clone();
                read_polylines_streaming(&s.shp_path, col, |feature| {
                    tx.send(StreamItem { layer_id: lid, feature })
                        .map_err(|_| anyhow::anyhow!("writer thread terminated"))
                }).with_context(|| format!("read {:?}", s.shp_path))?;
                Ok(())
            })?;
            // Drop the last `tx` clone here so the channel closes when all
            // readers are done.
            drop(tx);
            Ok(())
        });

        // Main thread is the writer. Drain the channel one feature at a time;
        // append to coords.bin + features.bin and collect bbox entries for
        // the R-tree.
        while let Ok(item) = rx.recv() {
            let f = item.feature;
            let (minzoom, flags) = derive_attributes(f.elevation as i32);
            builder.add_feature(&f.coords, f.elevation, minzoom, flags, item.layer_id)?;
            bbox_entries.push(FeatureBbox {
                idx,
                min: [f.bbox_min_lon as f32, f.bbox_min_latp as f32],
                max: [f.bbox_max_lon as f32, f.bbox_max_latp as f32],
            });
            idx = idx.checked_add(1).context("feature count exceeds u32::MAX")?;
        }

        // Surface any error from the reader pool (also catches the case where
        // the writer terminated early and readers got a SendError).
        match read_handle.join() {
            Ok(r) => r,
            Err(_) => anyhow::bail!("reader thread panicked"),
        }
    });
    read_result?;

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
