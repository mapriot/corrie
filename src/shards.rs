//! Per-group builds: tile each group of sources (e.g. one country) into its
//! own archive, reuse archives whose inputs are unchanged, merge them all.
//!
//! This keeps the temp cache at the size of the largest group instead of the
//! whole input, and makes rebuilds incremental: only groups whose sources,
//! layer settings or tiling code changed are tiled again. Because tiling is
//! per feature, the merged archive holds the same tiles as one run over all
//! sources (see `merge`).
//!
//! Groups: the source files are grouped by the first directory below the
//! deepest directory that contains all of them, so `contours/cz/data/a.shp`
//! and `contours/at/b.cpk` form groups `cz` and `at`. Files lying directly in
//! that common directory form one group each, named after the file. Names
//! that differ only in case are rejected: on a case-insensitive file system
//! they would share one archive.
//!
//! Each group archive records a key in its metadata (`corrie_shard.key`): a
//! hash of `TILING_VERSION`, the zoom range, bounding box and output layer
//! name, and per source its layer settings plus the size and modification
//! time of its file (for an SHP, also of its .dbf; a pack holds everything
//! it needs). An archive whose key matches is reused as is.

use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::build::{tile_sources, Cache};
use crate::config::{Settings, SourceLayer};
use crate::container::pmtiles::PmtilesReader;
use crate::merge::Merge;

/// Bump whenever a change makes corrie write different tiles for the same
/// input (clipping, simplification, attributes, encoding), so group
/// archives built by an older corrie are rebuilt instead of reused.
pub const TILING_VERSION: u32 = 2;

/// Metadata field that holds a group archive's name and key.
const META_FIELD: &str = "corrie_shard";

pub struct Group {
    pub name: String,
    pub sources: Vec<SourceLayer>,
}

/// Split sources into groups (see the module docs), sorted by name.
pub fn group_sources(sources: &[SourceLayer]) -> Result<Vec<Group>> {
    if sources.is_empty() { bail!("no sources in config"); }
    let parents: Vec<&Path> = sources.iter()
        .map(|s| s.shp_path.parent().unwrap_or(Path::new("")))
        .collect();
    let mut common: Vec<Component> = parents[0].components().collect();
    for p in &parents[1..] {
        let n = common.iter().zip(p.components()).take_while(|(a, b)| *a == b).count();
        common.truncate(n);
    }
    let root: PathBuf = common.iter().collect();

    let mut groups: Vec<Group> = Vec::new();
    for s in sources {
        let rel = s.shp_path.strip_prefix(&root).unwrap_or(&s.shp_path);
        let mut comps = rel.components();
        let first = comps.next().context("source path is empty")?;
        let raw = if comps.next().is_some() {
            first.as_os_str().to_string_lossy().into_owned()
        } else {
            s.shp_path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
        };
        let name: String = raw.chars()
            .map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' })
            .collect();
        match groups.iter_mut().find(|g| g.name == name) {
            Some(g) => g.sources.push(s.clone()),
            None => groups.push(Group { name, sources: vec![s.clone()] }),
        }
    }
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    let mut lower = std::collections::HashMap::new();
    for g in &groups {
        if let Some(other) = lower.insert(g.name.to_ascii_lowercase(), &g.name) {
            bail!("groups {:?} and {:?} differ only in case; rename one folder or file", other, g.name);
        }
    }
    Ok(groups)
}

/// The build key of one group archive (see the module docs).
pub fn group_key(group: &Group, settings: &Settings, layer_name: &str, base_dir: &Path) -> Result<String> {
    let file = |p: &Path| -> Result<serde_json::Value> {
        let m = std::fs::metadata(p).with_context(|| format!("stat {:?}", p))?;
        let mtime = m.modified().context("mtime")?
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        Ok(serde_json::json!({ "size": m.len(), "mtime_ns": mtime.to_string() }))
    };
    let mut sources = Vec::new();
    for s in &group.sources {
        let rel = s.shp_path.strip_prefix(base_dir).unwrap_or(&s.shp_path);
        let dbf = Some(&s.shp_path).filter(|p| !crate::build::is_pack(p)).map(|p| dbf_of(p));
        sources.push(serde_json::json!({
            "name": s.source_layer_name,
            "layer": s.output_layer_name,
            "shp": rel.to_string_lossy(),
            "shp_file": file(&s.shp_path)?,
            "dbf_file": match dbf.filter(|d| d.exists()) { Some(d) => file(&d)?, None => serde_json::Value::Null },
            "minzoom": s.minzoom,
            "maxzoom": s.maxzoom,
            "source_columns": s.source_columns,
            "simplify_below": s.simplify_below,
            "simplify_level": s.simplify_level,
            "simplify_length": s.simplify_length,
            "simplify_ratio": s.simplify_ratio,
            "simplify_algorithm": s.simplify_algorithm,
        }));
    }
    let doc = serde_json::json!({
        "tiling_version": TILING_VERSION,
        "layer": layer_name,
        "minzoom": settings.minzoom,
        "maxzoom": settings.maxzoom,
        "bounding_box": settings.bounding_box,
        "sources": sources,
    });
    let digest = Sha256::digest(serde_json::to_vec(&doc)?);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// The .dbf next to an SHP, whatever the case of its extension (GDAL finds
/// it either way); `<stem>.dbf` if there is none.
fn dbf_of(shp: &Path) -> PathBuf {
    let stem = shp.file_stem();
    shp.parent()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .and_then(|entries| entries.flatten().map(|e| e.path()).find(|p| {
            p.file_stem() == stem && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dbf"))
        }))
        .unwrap_or_else(|| shp.with_extension("dbf"))
}

/// The key stored in an existing group archive, if it opens and has one.
fn stored_key(path: &Path) -> Option<String> {
    let meta = PmtilesReader::open(path).ok()?.metadata_json().ok()?;
    let v: serde_json::Value = serde_json::from_str(&meta).ok()?;
    v.get(META_FIELD)?.get("key")?.as_str().map(str::to_string)
}

pub struct ShardStats {
    pub groups: usize,
    pub built: usize,
    pub reused: usize,
    pub tiles: u64,
    pub merged: u64,
}

/// Build or reuse one archive per group in `shard_dir`, then merge them all
/// into `output`. Every group uses `layer_name`, as a single run would.
pub fn build(
    sources: &[SourceLayer],
    settings: &Settings,
    layer_name: &str,
    base_dir: &Path,
    shard_dir: &Path,
    output: &Path,
) -> Result<ShardStats> {
    std::fs::create_dir_all(shard_dir).with_context(|| format!("create {:?}", shard_dir))?;
    let groups = group_sources(sources)?;
    let width = groups.iter().map(|g| g.name.len()).max().unwrap_or(0);
    crate::log!("{} groups in {:?}", groups.len(), shard_dir);

    let (mut built, mut reused) = (0, 0);
    let mut paths = Vec::with_capacity(groups.len());
    for (i, g) in groups.iter().enumerate() {
        let label = format!("[{:>w$}/{}] {:<width$}", i + 1, groups.len(), g.name, w = groups.len().to_string().len());
        let path = shard_dir.join(format!("{}.pmtiles", g.name));
        let key = group_key(g, settings, layer_name, base_dir)?;
        if stored_key(&path).as_deref() == Some(key.as_str()) {
            crate::log!("{label}  reused");
            reused += 1;
            paths.push(path);
            continue;
        }
        crate::log!("{label}  building from {} sources", g.sources.len());
        let t0 = Instant::now();
        // tile_sources writes <group>.pmtiles.partial and renames it when
        // complete, so an archive at the final path is always whole.
        let meta = serde_json::json!({ "group": g.name, "key": key, "tiling_version": TILING_VERSION });
        let s = tile_sources(&g.sources, settings, layer_name, &path, Cache::Ephemeral, &[(META_FIELD, meta)])
            .with_context(|| format!("group {}", g.name))?;
        crate::log!("{label}  built: {} features, {} tiles in {:.1}s",
            s.features, s.tiles, t0.elapsed().as_secs_f64());
        built += 1;
        paths.push(path);
    }

    let known: std::collections::HashSet<_> = paths.iter().cloned().collect();
    for entry in std::fs::read_dir(shard_dir)?.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "pmtiles") && !known.contains(&p) {
            crate::log!("note: {:?} is not a group of this config; not merged", p);
        }
        if p.extension().is_some_and(|e| e == "partial") {
            crate::log!("note: {:?} is left from a stopped run; delete it if no corrie is running", p);
        }
    }

    crate::log!("Merging {} group archives into {:?} ...", paths.len(), output);
    let t0 = Instant::now();
    let m = Merge::open(&paths)?.write(output, "elevation")?;
    crate::log!("Merged: {} tiles, {} joined from several groups, in {:.1}s",
        m.tiles, m.merged, t0.elapsed().as_secs_f64());
    Ok(ShardStats { groups: groups.len(), built, reused, tiles: m.tiles, merged: m.merged })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(name: &str, path: &str) -> SourceLayer {
        SourceLayer {
            source_layer_name: name.into(), output_layer_name: "contour".into(),
            shp_path: PathBuf::from(path), minzoom: 11, maxzoom: 14, source_columns: vec![],
            simplify_below: 14, simplify_level: 0.00005, simplify_length: 0.0, simplify_ratio: 2.0,
            simplify_algorithm: String::new(),
        }
    }

    fn names(groups: &[Group]) -> Vec<(String, usize)> {
        groups.iter().map(|g| (g.name.clone(), g.sources.len())).collect()
    }

    #[test]
    fn groups_by_first_directory_below_the_common_root() {
        let g = group_sources(&[
            src("cz1", "/m/contours/cz/data/merged_contours_1.shp"),
            src("at01", "/m/contours/at/final_contours_0_1.shp"),
            src("cz2", "/m/contours/cz/data/merged_contours_2.shp"),
            src("es", "/m/contours/es-ad-gi/final_contours_0_0.shp"),
        ]).unwrap();
        assert_eq!(names(&g), vec![("at".into(), 1), ("cz".into(), 2), ("es-ad-gi".into(), 1)]);
        // config order is kept inside a group
        assert_eq!(g[1].sources[0].source_layer_name, "cz1");
    }

    #[test]
    fn files_in_the_common_directory_are_one_group_each() {
        let g = group_sources(&[
            src("a", "/m/contours/mg/final_contours_0_1.shp"),
            src("b", "/m/contours/mg/final_contours_1_1.shp"),
        ]).unwrap();
        assert_eq!(names(&g), vec![("final_contours_0_1".into(), 1), ("final_contours_1_1".into(), 1)]);
    }

    #[test]
    fn group_names_differing_only_in_case_are_rejected() {
        let err = group_sources(&[
            src("a", "/m/contours/CZ.shp"),
            src("b", "/m/contours/cz/a.shp"),
        ]).err().expect("must fail");
        assert!(err.to_string().contains("differ only in case"), "{err}");
    }

    #[test]
    fn group_names_are_safe_file_names() {
        let g = group_sources(&[
            src("a", "/m/x y/a.shp"),
            src("b", "/m/z/b.shp"),
        ]).unwrap();
        assert_eq!(names(&g), vec![("x_y".into(), 1), ("z".into(), 1)]);
    }

    #[test]
    fn key_changes_with_inputs_and_settings_only() {
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("cz/a.shp");
        std::fs::create_dir_all(shp.parent().unwrap()).unwrap();
        std::fs::write(&shp, b"one").unwrap();
        std::fs::write(shp.with_extension("dbf"), b"dbf").unwrap();
        let group = Group { name: "cz".into(), sources: vec![src("a", shp.to_str().unwrap())] };
        let settings = Settings {
            minzoom: 11, maxzoom: 14, basezoom: None, include_ids: false,
            bounding_box: [-180.0, -85.0, 180.0, 85.0], compress: "gzip".into(),
            name: String::new(), version: String::new(), description: String::new(),
        };
        let k = group_key(&group, &settings, "contour", dir.path()).unwrap();
        assert_eq!(k, group_key(&group, &settings, "contour", dir.path()).unwrap(), "stable");

        let mut s2 = settings.clone();
        s2.maxzoom = 15;
        assert_ne!(k, group_key(&group, &s2, "contour", dir.path()).unwrap(), "zoom range");

        let mut g2 = Group { name: "cz".into(), sources: group.sources.clone() };
        g2.sources[0].simplify_level = 0.0001;
        assert_ne!(k, group_key(&g2, &settings, "contour", dir.path()).unwrap(), "layer settings");

        std::fs::write(&shp, b"three").unwrap();
        let k3 = group_key(&group, &settings, "contour", dir.path()).unwrap();
        assert_ne!(k, k3, "SHP size");

        assert_ne!(k3, group_key(&group, &settings, "contours", dir.path()).unwrap(), "layer name");

        // A .DBF / .Dbf is found too: changing it changes the key.
        std::fs::remove_file(shp.with_extension("dbf")).unwrap();
        std::fs::write(shp.with_extension("Dbf"), b"dbf").unwrap();
        let k4 = group_key(&group, &settings, "contour", dir.path()).unwrap();
        std::fs::write(shp.with_extension("Dbf"), b"dbf, edited").unwrap();
        assert_ne!(k4, group_key(&group, &settings, "contour", dir.path()).unwrap(), ".Dbf size");
    }

    /// A pack's key depends on the pack alone, so deleting the SHP set it was
    /// made from (whose .dbf has the same stem) does not rebuild its group.
    #[test]
    fn pack_key_ignores_files_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let cpk = dir.path().join("cz/a.cpk");
        std::fs::create_dir_all(cpk.parent().unwrap()).unwrap();
        std::fs::write(&cpk, b"pack").unwrap();
        std::fs::write(cpk.with_extension("dbf"), b"dbf").unwrap();
        let group = Group { name: "cz".into(), sources: vec![src("a", cpk.to_str().unwrap())] };
        let settings = Settings {
            minzoom: 11, maxzoom: 14, basezoom: None, include_ids: false,
            bounding_box: [-180.0, -85.0, 180.0, 85.0], compress: "gzip".into(),
            name: String::new(), version: String::new(), description: String::new(),
        };
        let k = group_key(&group, &settings, "contour", dir.path()).unwrap();
        std::fs::remove_file(cpk.with_extension("dbf")).unwrap();
        assert_eq!(k, group_key(&group, &settings, "contour", dir.path()).unwrap());
        std::fs::write(&cpk, b"pack, rebuilt").unwrap();
        assert_ne!(k, group_key(&group, &settings, "contour", dir.path()).unwrap());
    }
}
