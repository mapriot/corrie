//! Packed binary feature store. Two files on disk:
//!
//! * `features.bin` — flat `[FeatureRecord]` array, 36 bytes each.
//! * `coords.bin`   — flat `[[f64; 2]]` array of `(lon, latp)` vertices.
//!
//! Plus a `manifest.bin` (separate module concern) with the source hash.
//!
//! Layout is fixed and little-endian. `FeatureRecord` is `#[repr(C)]` so
//! `bytemuck::cast_slice` can read it directly from an mmap.

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

    /// Append a feature. `coords` is the polyline in `(lon, latp)` f64 pairs.
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

        // Write coords first (in case features.bin write fails halfway).
        let bytes: &[u8] = bytemuck::cast_slice(coords);
        self.coords.write_all(bytes).context("write coords")?;
        self.n_vertices += coords.len() as u64;

        // Compute bbox.
        let (mut mnx, mut mny) = (f64::INFINITY, f64::INFINITY);
        let (mut mxx, mut mxy) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in coords {
            if c[0] < mnx { mnx = c[0]; }
            if c[1] < mny { mny = c[1]; }
            if c[0] > mxx { mxx = c[0]; }
            if c[1] > mxy { mxy = c[1]; }
        }

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

    pub fn coords_of(&self, rec: &FeatureRecord) -> &[[f64; 2]] {
        let start_bytes = rec.coords_offset as usize * 16;
        let end_bytes   = start_bytes + rec.coords_len as usize * 16;
        let slice = &self.coords_mmap[start_bytes..end_bytes];
        bytemuck::cast_slice(slice)
    }
}
