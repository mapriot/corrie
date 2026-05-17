//! Phase 1 driver: read all SHPs (parallel), apply attribute rules,
//! write features.bin + coords.bin + rtree.bin + manifest.bin.

use anyhow::{Context, Result};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use crate::attribute_rules::derive_attributes;
use crate::config::SourceLayer;
use crate::feature_store::{Builder, Manifest, compute_source_hash, write_manifest, MANIFEST_VERSION};
use crate::shapefile_read::{read_polylines, ReadFeature};
use crate::spatial_index::{build_rtree, save_rtree, FeatureBbox};

pub struct IngestStats {
    pub features: u64,
    pub vertices: u64,
}

/// Read all SHPs in parallel, then sequentially write the feature store.
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

    // Read SHPs in parallel. Each thread accumulates its own Vec<(u16 layer_id, ReadFeature)>.
    let per_thread: Vec<Result<Vec<(u16, ReadFeature)>>> = parallel_inputs
        .par_iter()
        .map(|(lid, s, col)| -> Result<_> {
            let feats = read_polylines(&s.shp_path, col)
                .with_context(|| format!("read {:?}", s.shp_path))?;
            Ok(feats.into_iter().map(|f| (*lid, f)).collect())
        })
        .collect();

    // Surface first error; flatten ok.
    let mut all_features: Vec<(u16, ReadFeature)> = Vec::new();
    for r in per_thread {
        all_features.extend(r?);
    }

    // Write feature store sequentially (Builder is not parallel-safe).
    let mut builder = Builder::create(cache_dir)?;
    let mut bbox_entries: Vec<FeatureBbox> = Vec::with_capacity(all_features.len());

    for (idx, (layer_id, f)) in all_features.iter().enumerate() {
        let (minzoom, flags) = derive_attributes(f.elevation as i32);
        builder.add_feature(&f.coords, f.elevation, minzoom, flags, *layer_id)?;
        bbox_entries.push(FeatureBbox {
            idx: idx as u32,
            min: [f.bbox_min_lon as f32, f.bbox_min_latp as f32],
            max: [f.bbox_max_lon as f32, f.bbox_max_latp as f32],
        });
    }

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
