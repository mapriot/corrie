//! Packed binary feature store. Two files on disk:
//!
//! * `features.bin` — flat `[FeatureRecord]` array, 36 bytes each.
//! * `coords.bin`   — flat `[[i32; 2]]` array of `(lon, latp)` vertices,
//!                    fixed-point @ scale [`COORDS_SCALE`] = 1e7.
//!
//! Plus a `manifest.bin` (separate module concern) with the source hash.
//!
//! Layout is fixed and little-endian. `FeatureRecord` is `#[repr(C)]` so
//! `bytemuck::cast_slice` can read it directly from an mmap.
//!
//! Coords were f64 pairs (16 B/vertex) until the 2026-05-21 prod-tr
//! profile revealed the bottleneck was memory-stall-bound (86% L3 miss).
//! Halving the bytes/vertex halves the memory traffic that the CPU stalls
//! on. Quantization error at 1e-7 deg is ~5 mm at equator — well below the
//! z=14 MVT sub-pixel (~30 cm), so visual output is identical though the
//! exact MVT bytes for vertices that land near pixel boundaries may differ.

/// Lon/latp scale factor. f64_value = i32_value * 1e-7. Max representable
/// lon is +/- 214.7°, latp +/- 214.7° — both well above the Web-Mercator
/// caps of +/-180° lon and +/-~85° latp.
pub const COORDS_SCALE: f64 = 1e7;
pub const COORDS_INV_SCALE: f64 = 1e-7;

use bytemuck::{Pod, Zeroable};

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C, packed(4))]
pub struct FeatureRecord {
    /// f32 bbox for R-tree query — lossy but sub-metre precision suffices.
    pub bbox_min_lon:  f32,  //  4
    pub bbox_min_latp: f32,  //  4
    pub bbox_max_lon:  f32,  //  4
    pub bbox_max_latp: f32,  //  4 (total 16, 8-byte aligned)
    /// Offset into `coords.bin`, in *vertex* units (= 16 bytes each).
    pub coords_offset: u64,  //  8 (offset 16, 8-byte aligned, no padding needed)
    pub coords_len:    u32,  //  4 (offset 24)
    pub layer_id:      u16,  //  2 (offset 28)
    pub elevation:     i16,  //  2 (offset 30)
    pub minzoom:       u8,   //  1 (offset 32)
    pub flags:         u8,   //  1 (offset 33)
    pub _pad:          [u8; 2], //  2 (offset 34-35)
}

const _: () = assert!(std::mem::size_of::<FeatureRecord>() == 36);

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Builds a feature_store directory (`features.bin` + `coords.bin`).
pub struct Builder {
    dir: PathBuf,
    features: BufWriter<File>,
    coords: BufWriter<File>,
    n_features: u64,
    n_vertices: u64,
}

impl Builder {
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {:?}", dir))?;
        let f = File::create(dir.join("features.bin"))?;
        let c = File::create(dir.join("coords.bin"))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            features: BufWriter::new(f),
            coords: BufWriter::new(c),
            n_features: 0,
            n_vertices: 0,
        })
    }

    /// Append a feature. `coords` is the polyline in `(lon, latp)` f64 pairs;
    /// stored on-disk as i32 fixed-point at [`COORDS_SCALE`].
    pub fn add_feature(
        &mut self,
        coords: &[[f64; 2]],
        elevation: i16,
        minzoom: u8,
        flags: u8,
        layer_id: u16,
    ) -> Result<()> {
        let offset_vertices = self.n_vertices;
        let len_vertices = coords.len() as u32;

        // Encode coords as i32 fixed-point. Compute the original-f64 bbox
        // in the same pass — it stays f32 in FeatureRecord for the R-tree.
        let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
        let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in coords {
            if c[0] < mnx { mnx = c[0]; }
            if c[1] < mny { mny = c[1]; }
            if c[0] > mxx { mxx = c[0]; }
            if c[1] > mxy { mxy = c[1]; }
            let q: [i32; 2] = [
                (c[0] * COORDS_SCALE).round() as i32,
                (c[1] * COORDS_SCALE).round() as i32,
            ];
            let q_bytes: &[u8] = bytemuck::bytes_of(&q);
            self.coords.write_all(q_bytes).context("write coords")?;
        }
        self.n_vertices += coords.len() as u64;

        let rec = FeatureRecord {
            bbox_min_lon:  mnx as f32,
            bbox_min_latp: mny as f32,
            bbox_max_lon:  mxx as f32,
            bbox_max_latp: mxy as f32,
            coords_offset: offset_vertices,
            coords_len:    len_vertices,
            layer_id,
            elevation,
            minzoom,
            flags,
            _pad: [0; 2],
        };
        let rec_bytes: &[u8] = bytemuck::bytes_of(&rec);
        self.features.write_all(rec_bytes).context("write feature")?;
        self.n_features += 1;
        Ok(())
    }

    /// Flush and close. Returns the number of features written.
    pub fn finalize(mut self) -> Result<u64> {
        self.features.flush()?;
        self.coords.flush()?;
        // Drop the BufWriters explicitly to release the fds.
        drop(self.features);
        drop(self.coords);
        Ok(self.n_features)
    }

    pub fn dir(&self) -> &Path { &self.dir }
    pub fn n_features(&self) -> u64 { self.n_features }
    pub fn n_vertices(&self) -> u64 { self.n_vertices }
}

use memmap2::Mmap;

/// Mmap-backed read-only view of a feature_store directory.
pub struct Reader {
    features_mmap: Mmap,
    coords_mmap:   Mmap,
}

impl Reader {
    pub fn open(dir: &Path) -> Result<Self> {
        let f = File::open(dir.join("features.bin"))
            .with_context(|| format!("open features.bin in {:?}", dir))?;
        let c = File::open(dir.join("coords.bin"))
            .with_context(|| format!("open coords.bin in {:?}", dir))?;
        let features_mmap = unsafe { Mmap::map(&f)? };
        let coords_mmap   = unsafe { Mmap::map(&c)? };
        Ok(Self { features_mmap, coords_mmap })
    }

    pub fn features(&self) -> &[FeatureRecord] {
        bytemuck::cast_slice(&self.features_mmap[..])
    }

    /// Raw i32 fixed-point coordinates for the feature. Each `[i32; 2]` is
    /// `[lon * COORDS_SCALE, latp * COORDS_SCALE]`. Multiply by
    /// [`COORDS_INV_SCALE`] for f64 values.
    pub fn coords_of(&self, rec: &FeatureRecord) -> &[[i32; 2]] {
        let start_bytes = rec.coords_offset as usize * 8;
        let end_bytes   = start_bytes + rec.coords_len as usize * 8;
        let slice = &self.coords_mmap[start_bytes..end_bytes];
        bytemuck::cast_slice(slice)
    }

    /// Decode a feature's coords into the caller-provided buffer as
    /// `[f64; 2]` pairs in source (lon, latp) units. The buffer is
    /// cleared and grown as needed; capacity is retained for reuse.
    /// This is the per-feature decode the hot loop calls.
    #[inline(always)]
    pub fn decode_coords_into(&self, rec: &FeatureRecord, out: &mut Vec<[f64; 2]>) {
        let q = self.coords_of(rec);
        let n = q.len();
        out.clear();
        out.reserve(n);
        // Unsafe ptr write to skip Vec::push's per-element capacity check;
        // we've reserved exactly n. This is the same pattern that mattered
        // in the pre-fuse clip code where push overhead dominated.
        unsafe {
            let dst = out.as_mut_ptr();
            for (i, src) in q.iter().enumerate() {
                dst.add(i).write([
                    src[0] as f64 * COORDS_INV_SCALE,
                    src[1] as f64 * COORDS_INV_SCALE,
                ]);
            }
            out.set_len(n);
        }
    }
}

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub version: u32,
    pub feature_count: u64,
    pub vertex_count: u64,
    pub layer_names: Vec<String>,
    pub source_hash: [u8; 32],
}

/// Compute SHA-256 over a sorted list of (path, size, mtime_nanos) tuples.
pub fn compute_source_hash(paths: &[PathBuf]) -> Result<[u8; 32]> {
    let mut tuples: Vec<(String, u64, i128)> = Vec::with_capacity(paths.len());
    for p in paths {
        let m = std::fs::metadata(p).with_context(|| format!("stat {:?}", p))?;
        let mtime = m.modified()
            .context("mtime")?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mtime_nanos = mtime.as_nanos() as i128;
        tuples.push((p.to_string_lossy().into_owned(), m.len(), mtime_nanos));
    }
    tuples.sort();
    let mut hasher = Sha256::new();
    for (path, size, mtime) in tuples {
        hasher.update(path.as_bytes());
        hasher.update(size.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

pub fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    let path = dir.join("manifest.bin");
    let bytes = bincode::serialize(m).context("serialize manifest")?;
    std::fs::write(&path, bytes).with_context(|| format!("write {:?}", path))?;
    Ok(())
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join("manifest.bin");
    let bytes = std::fs::read(&path).with_context(|| format!("read {:?}", path))?;
    let m: Manifest = bincode::deserialize(&bytes).context("deserialize manifest")?;
    Ok(m)
}

/// Returns true if the cache in `dir` matches the current SHP set.
pub fn cache_valid(dir: &Path, current_shps: &[PathBuf]) -> bool {
    let Ok(m) = read_manifest(dir) else { return false; };
    if m.version != MANIFEST_VERSION { return false; }
    let Ok(h) = compute_source_hash(current_shps) else { return false; };
    h == m.source_hash
}
