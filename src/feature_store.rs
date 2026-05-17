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
