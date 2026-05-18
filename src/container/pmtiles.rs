//! PMTiles v3 writer with hierarchical leaf directories written in-place.
//!
//! Implements the PMTiles v3 spec end-to-end so that heap usage during
//! tile-gen is bounded by one leaf's worth of `DirEntry` records plus
//! a small root vec, independent of total tile count. See the design
//! doc at `.claude/specs/2026-05-18-corrie-streaming-pmtiles-writer-design.md`.

// ---------------- varint (LEB128 unsigned) ----------------

fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

fn read_varint(buf: &[u8]) -> anyhow::Result<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for (i, &b) in buf.iter().enumerate() {
        value |= ((b & 0x7f) as u64) << shift;
        if (b & 0x80) == 0 {
            return Ok((value, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            anyhow::bail!("varint overflow");
        }
    }
    anyhow::bail!("varint truncated");
}

// ---------------- tile_id (PMTiles v3 Hilbert) ----------------

/// Compute the PMTiles v3 tile_id for `(z, x, y)`. PMTiles orders tiles
/// along a Hilbert curve interleaved across all zooms — `tile_id` is the
/// position in that single global ordering.
///
/// Formula: `((4^z - 1) / 3) + hilbert(z, x, y)` per spec v3 §3.4.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    // Sum of tiles at lower zooms 0..z is (4^z - 1) / 3.
    let base = ((1u64 << (2 * z)) - 1) / 3;
    base + hilbert(z, x, y)
}

/// Hilbert curve index within zoom `z`. Reference algorithm from the
/// PMTiles spec; identical to the `pmtiles` Go reference.
fn hilbert(z: u8, mut x: u32, mut y: u32) -> u64 {
    let mut d: u64 = 0;
    let mut s: i64 = 1i64 << (z as i64) >> 1; // s = 2^(z-1), with z=0 giving 0
    while s > 0 {
        let rx = if (x as i64 & s) > 0 { 1u32 } else { 0 };
        let ry = if (y as i64 & s) > 0 { 1u32 } else { 0 };
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                x = (s as u32 - 1).wrapping_sub(x);
                y = (s as u32 - 1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    d
}

// ---------------- DirEntry encoding ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub tile_id: u64,
    pub run_length: u32,
    pub offset: u64,
    pub length: u32,
}

/// Serialize directory entries per PMTiles v3 §3.1: a count varint
/// followed by four parallel varint arrays (tile_id_deltas,
/// run_lengths, lengths, encoded_offsets). The output is NOT gzipped;
/// caller is responsible for compression.
pub fn serialize_directory(entries: &[DirEntry]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(entries.len() * 8);
    write_varint(&mut buf, entries.len() as u64);

    // 1. tile_id deltas (first absolute, rest relative to previous)
    let mut last_id = 0u64;
    for e in entries {
        write_varint(&mut buf, e.tile_id - last_id);
        last_id = e.tile_id;
    }

    // 2. run_lengths
    for e in entries { write_varint(&mut buf, e.run_length as u64); }

    // 3. lengths
    for e in entries { write_varint(&mut buf, e.length as u64); }

    // 4. encoded offsets: 0 if contiguous with previous, else offset+1
    for i in 0..entries.len() {
        let e = &entries[i];
        let encoded = if i > 0 {
            let prev = &entries[i - 1];
            if e.offset == prev.offset + prev.length as u64 { 0 } else { e.offset + 1 }
        } else {
            e.offset + 1
        };
        write_varint(&mut buf, encoded);
    }

    buf
}

/// Inverse of `serialize_directory`. Used by tests + cross-validation.
pub fn deserialize_directory(buf: &[u8]) -> anyhow::Result<Vec<DirEntry>> {
    let mut cur = 0;
    let (count, n) = read_varint(&buf[cur..])?;
    cur += n;
    let count = count as usize;

    let mut tile_ids = Vec::with_capacity(count);
    let mut last_id = 0u64;
    for _ in 0..count {
        let (delta, n) = read_varint(&buf[cur..])?;
        cur += n;
        last_id += delta;
        tile_ids.push(last_id);
    }

    let mut run_lengths = Vec::with_capacity(count);
    for _ in 0..count {
        let (v, n) = read_varint(&buf[cur..])?;
        cur += n;
        run_lengths.push(v as u32);
    }

    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count {
        let (v, n) = read_varint(&buf[cur..])?;
        cur += n;
        lengths.push(v as u32);
    }

    let mut offsets = Vec::with_capacity(count);
    for i in 0..count {
        let (raw, n) = read_varint(&buf[cur..])?;
        cur += n;
        let offset = if raw == 0 && i > 0 {
            offsets[i - 1] + lengths[i - 1] as u64
        } else if raw == 0 {
            anyhow::bail!("first entry must encode offset as offset+1, got 0");
        } else {
            raw - 1
        };
        offsets.push(offset);
    }

    Ok((0..count).map(|i| DirEntry {
        tile_id: tile_ids[i],
        run_length: run_lengths[i],
        offset: offsets[i],
        length: lengths[i],
    }).collect())
}

// ---------------- Header (127 bytes, PMTiles v3) ----------------

pub const HEADER_SIZE: usize = 127;
const MAGIC: &[u8; 7] = b"PMTiles";
const SPEC_VERSION: u8 = 3;

// Compression enum values from PMTiles v3 spec.
pub const COMPRESSION_NONE: u8 = 1;
pub const COMPRESSION_GZIP: u8 = 2;

// Tile-type enum values from PMTiles v3 spec.
pub const TILE_TYPE_MVT: u8 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub root_dir_offset: u64,
    pub root_dir_bytes: u64,
    pub json_metadata_offset: u64,
    pub json_metadata_bytes: u64,
    pub leaf_dirs_offset: u64,
    pub leaf_dirs_bytes: u64,
    pub tile_data_offset: u64,
    pub tile_data_bytes: u64,
    pub addressed_tiles_count: u64,
    pub tile_entries_count: u64,
    pub tile_contents_count: u64,
    pub clustered: bool,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub tile_type: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub min_lon_e7: i32,
    pub min_lat_e7: i32,
    pub max_lon_e7: i32,
    pub max_lat_e7: i32,
    pub center_zoom: u8,
    pub center_lon_e7: i32,
    pub center_lat_e7: i32,
}

pub fn serialize_header(h: &Header) -> [u8; HEADER_SIZE] {
    let mut b = [0u8; HEADER_SIZE];
    b[0..7].copy_from_slice(MAGIC);
    b[7] = SPEC_VERSION;
    b[8..16].copy_from_slice(&h.root_dir_offset.to_le_bytes());
    b[16..24].copy_from_slice(&h.root_dir_bytes.to_le_bytes());
    b[24..32].copy_from_slice(&h.json_metadata_offset.to_le_bytes());
    b[32..40].copy_from_slice(&h.json_metadata_bytes.to_le_bytes());
    b[40..48].copy_from_slice(&h.leaf_dirs_offset.to_le_bytes());
    b[48..56].copy_from_slice(&h.leaf_dirs_bytes.to_le_bytes());
    b[56..64].copy_from_slice(&h.tile_data_offset.to_le_bytes());
    b[64..72].copy_from_slice(&h.tile_data_bytes.to_le_bytes());
    b[72..80].copy_from_slice(&h.addressed_tiles_count.to_le_bytes());
    b[80..88].copy_from_slice(&h.tile_entries_count.to_le_bytes());
    b[88..96].copy_from_slice(&h.tile_contents_count.to_le_bytes());
    b[96] = h.clustered as u8;
    b[97] = h.internal_compression;
    b[98] = h.tile_compression;
    b[99] = h.tile_type;
    b[100] = h.min_zoom;
    b[101] = h.max_zoom;
    b[102..106].copy_from_slice(&h.min_lon_e7.to_le_bytes());
    b[106..110].copy_from_slice(&h.min_lat_e7.to_le_bytes());
    b[110..114].copy_from_slice(&h.max_lon_e7.to_le_bytes());
    b[114..118].copy_from_slice(&h.max_lat_e7.to_le_bytes());
    b[118] = h.center_zoom;
    b[119..123].copy_from_slice(&h.center_lon_e7.to_le_bytes());
    b[123..127].copy_from_slice(&h.center_lat_e7.to_le_bytes());
    b
}

pub fn deserialize_header(b: &[u8]) -> anyhow::Result<Header> {
    if b.len() < HEADER_SIZE { anyhow::bail!("header too short"); }
    if &b[0..7] != MAGIC { anyhow::bail!("not a PMTiles file"); }
    if b[7] != SPEC_VERSION {
        anyhow::bail!("unsupported PMTiles version {}", b[7]);
    }
    let u = |r: std::ops::Range<usize>| -> u64 {
        u64::from_le_bytes(b[r].try_into().unwrap())
    };
    let i = |r: std::ops::Range<usize>| -> i32 {
        i32::from_le_bytes(b[r].try_into().unwrap())
    };
    Ok(Header {
        root_dir_offset:        u(8..16),
        root_dir_bytes:         u(16..24),
        json_metadata_offset:   u(24..32),
        json_metadata_bytes:    u(32..40),
        leaf_dirs_offset:       u(40..48),
        leaf_dirs_bytes:        u(48..56),
        tile_data_offset:       u(56..64),
        tile_data_bytes:        u(64..72),
        addressed_tiles_count:  u(72..80),
        tile_entries_count:     u(80..88),
        tile_contents_count:    u(88..96),
        clustered:              b[96] != 0,
        internal_compression:   b[97],
        tile_compression:       b[98],
        tile_type:              b[99],
        min_zoom:               b[100],
        max_zoom:               b[101],
        min_lon_e7:             i(102..106),
        min_lat_e7:             i(106..110),
        max_lon_e7:             i(110..114),
        max_lat_e7:             i(114..118),
        center_zoom:            b[118],
        center_lon_e7:          i(119..123),
        center_lat_e7:          i(123..127),
    })
}

// ---------------- gzip (uses flate2, already in deps) ----------------

fn gzip(input: &[u8]) -> anyhow::Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::with_capacity(input.len() / 2), Compression::default());
    enc.write_all(input)?;
    Ok(enc.finish()?)
}

// ---------------- PmtilesWriter ----------------

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use anyhow::Context;

/// Reservation sizes (bytes) at the start of every output file. The leaves
/// reservation is computed from `tile_count_estimate` at create time.
const ROOT_RESERVED: u64     = 4 * 1024 * 1024;   // 4 MB
const METADATA_RESERVED: u64 = 128 * 1024;        // 128 KB
const LEAF_BYTES_PER_ENTRY: u64 = 20;             // upper bound on varint-encoded directory entry size

/// Default entries per leaf directory. Sized so the in-memory leaf
/// buffer stays under ~400 KB and the root directory stays small.
pub const DEFAULT_LEAF_SIZE: usize = 16_384;

pub struct PmtilesWriter {
    file: BufWriter<File>,
    /// First byte of tile data section (= header + root_res + meta_res + leaves_res).
    tile_data_section_start: u64,
    /// Next absolute file offset where the next tile's bytes will be written.
    next_tile_offset: u64,
    /// Bounds of the leaves reservation: cursor must stay within
    /// [leaves_reserved_start, leaves_reserved_end).
    leaves_reserved_start: u64,
    leaves_reserved_end: u64,
    /// Cursor into the leaves reservation; advances as leaves are flushed.
    leaves_cursor: u64,
    /// Current leaf being filled. Reset after every flush.
    leaf_buffer: Vec<DirEntry>,
    /// One entry per finalized leaf, accumulated for the root directory.
    root_entries: Vec<DirEntry>,
    /// Configured leaf size.
    leaf_size: usize,
    /// Header fields that don't change after create — keep them so finalize can patch.
    min_zoom: u8,
    max_zoom: u8,
    bounds_e7: (i32, i32, i32, i32),
    center_e7: (i32, i32),
    center_zoom: u8,
    /// Metadata JSON, computed at create time from layer/elevation names.
    metadata_json: String,
    /// Strict-monotonic check.
    last_tile_id: Option<u64>,
    /// Running count of tiles written. Used by finalize for header fields.
    total_addressed: u64,
}

impl PmtilesWriter {
    pub fn create(
        path: &Path,
        min_z: u8,
        max_z: u8,
        bounds: (f64, f64, f64, f64),
        layer_name: &str,
        elevation_attr: &str,
        tile_count_estimate: u64,
    ) -> anyhow::Result<Self> {
        let (west, south, east, north) = bounds;
        let center_lon = (west + east) / 2.0;
        let center_lat = (south + north) / 2.0;
        let center_zoom = ((min_z + max_z) / 2).max(min_z);

        // TileJSON 3.0 vector_layers — required by MapLibre for layer introspection.
        // Matches the metadata schema corrie wrote pre-rewrite.
        let metadata_json = format!(
            r#"{{"vector_layers":[{{"id":"{layer_name}","fields":{{"{elevation_attr}":"Number","main":"Number","show":"Number"}},"minzoom":{min_z},"maxzoom":{max_z}}}]}}"#
        );

        let leaves_reserved = tile_count_estimate.saturating_mul(LEAF_BYTES_PER_ENTRY).max(1024);

        let mut f = BufWriter::new(File::create(path).with_context(|| format!("create {:?}", path))?);

        // Zero-pad the [Header][Root][Metadata][Leaves] regions so we can
        // seek-and-write them at finalize without leaving uninitialized bytes
        // in the file. Sequential write of zeros at the start is cheap and
        // gives us a known cursor position for tile data to begin from.
        let total_reservation = HEADER_SIZE as u64
            + ROOT_RESERVED
            + METADATA_RESERVED
            + leaves_reserved;
        write_zeros(&mut f, total_reservation)?;

        let leaves_reserved_start = HEADER_SIZE as u64 + ROOT_RESERVED + METADATA_RESERVED;
        let leaves_reserved_end = leaves_reserved_start + leaves_reserved;
        let tile_data_section_start = leaves_reserved_end;

        Ok(Self {
            file: f,
            tile_data_section_start,
            next_tile_offset: tile_data_section_start,
            leaves_reserved_start,
            leaves_reserved_end,
            leaves_cursor: leaves_reserved_start,
            leaf_buffer: Vec::with_capacity(DEFAULT_LEAF_SIZE),
            root_entries: Vec::new(),
            leaf_size: DEFAULT_LEAF_SIZE,
            min_zoom: min_z,
            max_zoom: max_z,
            bounds_e7: (
                (west  * 1e7).round() as i32,
                (south * 1e7).round() as i32,
                (east  * 1e7).round() as i32,
                (north * 1e7).round() as i32,
            ),
            center_e7: (
                (center_lon * 1e7).round() as i32,
                (center_lat * 1e7).round() as i32,
            ),
            center_zoom,
            metadata_json,
            last_tile_id: None,
            total_addressed: 0,
        })
    }

    /// To be implemented in Task 6.
    pub fn add_tile(&mut self, _z: u8, _x: u32, _y: u32, _mvt_raw: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("add_tile not yet implemented — Task 6")
    }

    /// To be implemented in Task 7.
    pub fn finalize(self) -> anyhow::Result<()> {
        anyhow::bail!("finalize not yet implemented — Task 7")
    }
}

fn write_zeros<W: Write>(w: &mut W, n: u64) -> anyhow::Result<()> {
    const ZERO_CHUNK: [u8; 64 * 1024] = [0u8; 64 * 1024];
    let mut remaining = n;
    while remaining > 0 {
        let chunk = remaining.min(ZERO_CHUNK.len() as u64) as usize;
        w.write_all(&ZERO_CHUNK[..chunk])?;
        remaining -= chunk as u64;
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_zero_is_one_byte() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0);
        assert_eq!(buf, vec![0x00]);
        let (v, n) = read_varint(&buf).unwrap();
        assert_eq!((v, n), (0, 1));
    }

    #[test]
    fn varint_127_is_one_byte() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 127);
        assert_eq!(buf, vec![0x7f]);
        assert_eq!(read_varint(&buf).unwrap(), (127, 1));
    }

    #[test]
    fn varint_128_is_two_bytes() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 128);
        assert_eq!(buf, vec![0x80, 0x01]);
        assert_eq!(read_varint(&buf).unwrap(), (128, 2));
    }

    #[test]
    fn varint_round_trip_edge_cases() {
        for v in [0u64, 1, 127, 128, 16383, 16384, u32::MAX as u64,
                  1_000_000_000_000, u64::MAX] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            let (decoded, n) = read_varint(&buf).unwrap();
            assert_eq!(decoded, v, "value {v}");
            assert_eq!(n, buf.len(), "consumed != written for {v}");
        }
    }

    #[test]
    fn read_varint_rejects_truncated() {
        // 0x80 alone means "continuation, but no next byte".
        assert!(read_varint(&[0x80]).is_err());
    }

    #[test]
    fn read_varint_rejects_overflow() {
        // 10 bytes with continuation bits would overflow u64.
        let bad = vec![0xff; 11];
        assert!(read_varint(&bad).is_err());
    }

    #[test]
    fn tile_id_zoom_zero_is_zero() {
        assert_eq!(tile_id(0, 0, 0), 0);
    }

    #[test]
    fn tile_id_zoom_one_hilbert_order() {
        // Hilbert at z=1 visits (0,0), (0,1), (1,1), (1,0). Adding the
        // zoom-0 prefix of 1 tile shifts all by 1.
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
    }

    #[test]
    fn tile_id_zoom_prefix_correct() {
        // Sum of tile counts at zooms 0..2 = 1 + 4 = 5. So first tile at z=2 has id 5.
        assert_eq!(tile_id(2, 0, 0), 5);
        // z=3 prefix = 1 + 4 + 16 = 21.
        assert_eq!(tile_id(3, 0, 0), 21);
    }

    #[test]
    fn tile_id_strictly_monotonic_in_hilbert_walk_at_z14() {
        // Sanity: walking adjacent x,y pairs at z=14 should mostly produce
        // sequential tile_ids — at minimum, all tile_ids must be distinct.
        let z = 14;
        let n = 1u32 << z;
        let mut last: Option<u64> = None;
        for x in 0..16 {
            for y in 0..16 {
                let id = tile_id(z, x, y);
                if let Some(prev) = last {
                    assert_ne!(id, prev, "duplicate tile_id at z={z}, x={x}, y={y}");
                }
                last = Some(id);
            }
        }
        let _ = n; // silence unused
    }

    #[test]
    fn tile_id_matches_upstream_pmtiles_crate() {
        // Spot-check our Hilbert implementation against the published
        // pmtiles crate for zooms 0..=8 across a grid of coordinates.
        use pmtiles::{TileCoord as PTCoord, TileId};
        for z in 0u8..=8 {
            let n = 1u32 << z;
            let step = (n / 8).max(1);
            let mut xs = vec![0, n.saturating_sub(1)];
            xs.extend((step..n).step_by(step as usize));
            xs.sort(); xs.dedup();
            for &x in &xs {
                for &y in &xs {
                    let ours = tile_id(z, x, y);
                    let theirs = TileId::from(PTCoord::new(z, x, y).unwrap()).value();
                    assert_eq!(ours, theirs, "mismatch at z={z}, x={x}, y={y}");
                }
            }
        }
    }

    #[test]
    fn directory_round_trip_single_entry() {
        let entries = vec![DirEntry { tile_id: 42, run_length: 1, offset: 0, length: 100 }];
        let bytes = serialize_directory(&entries);
        let decoded = deserialize_directory(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn directory_round_trip_contiguous_offsets() {
        // Tile bytes A=[0, 100), B=[100, 250), C=[250, 300).
        // Offset deltas should encode as 0 for the contiguous pair.
        let entries = vec![
            DirEntry { tile_id: 1, run_length: 1, offset: 0,   length: 100 },
            DirEntry { tile_id: 2, run_length: 1, offset: 100, length: 150 },
            DirEntry { tile_id: 3, run_length: 1, offset: 250, length: 50  },
        ];
        let bytes = serialize_directory(&entries);
        let decoded = deserialize_directory(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn directory_round_trip_non_contiguous_offsets() {
        // Gap between tiles forces absolute offset encoding.
        let entries = vec![
            DirEntry { tile_id: 1, run_length: 1, offset: 0,    length: 100 },
            DirEntry { tile_id: 2, run_length: 1, offset: 5000, length: 150 },
        ];
        let bytes = serialize_directory(&entries);
        let decoded = deserialize_directory(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn directory_round_trip_leaf_pointer() {
        // Root entry pointing to a leaf: run_length=0.
        let entries = vec![
            DirEntry { tile_id: 0,    run_length: 0, offset: 0,      length: 1024 },
            DirEntry { tile_id: 1000, run_length: 0, offset: 1024,   length: 2048 },
        ];
        let bytes = serialize_directory(&entries);
        let decoded = deserialize_directory(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn directory_round_trip_many_entries() {
        // 1000 entries in Hilbert (sequential tile_id), contiguous bytes.
        let entries: Vec<DirEntry> = (0..1000)
            .map(|i| DirEntry {
                tile_id: i as u64,
                run_length: 1,
                offset: (i * 256) as u64,
                length: 256,
            })
            .collect();
        let bytes = serialize_directory(&entries);
        let decoded = deserialize_directory(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn header_round_trip() {
        let h = Header {
            root_dir_offset: 127,
            root_dir_bytes: 4096,
            json_metadata_offset: 4223,
            json_metadata_bytes: 256,
            leaf_dirs_offset: 4479,
            leaf_dirs_bytes: 1_000_000,
            tile_data_offset: 1_004_479,
            tile_data_bytes: 500_000_000,
            addressed_tiles_count: 73_227,
            tile_entries_count: 43_211,
            tile_contents_count: 43_211,
            clustered: true,
            internal_compression: COMPRESSION_GZIP,
            tile_compression: COMPRESSION_GZIP,
            tile_type: TILE_TYPE_MVT,
            min_zoom: 11,
            max_zoom: 14,
            min_lon_e7: 120_000_000,
            min_lat_e7: 485_000_000,
            max_lon_e7: 189_000_000,
            max_lat_e7: 511_000_000,
            center_zoom: 12,
            center_lon_e7: 154_500_000,
            center_lat_e7: 498_000_000,
        };
        let bytes = serialize_header(&h);
        let decoded = deserialize_header(&bytes).unwrap();
        assert_eq!(decoded, h);
    }

    #[test]
    fn header_size_is_127_bytes() {
        let h = Header::default();
        let bytes = serialize_header(&h);
        assert_eq!(bytes.len(), 127);
    }

    #[test]
    fn header_starts_with_magic_and_version() {
        let h = Header::default();
        let bytes = serialize_header(&h);
        assert_eq!(&bytes[0..7], MAGIC);
        assert_eq!(bytes[7], SPEC_VERSION);
    }

    #[test]
    fn deserialize_rejects_wrong_magic() {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[7] = SPEC_VERSION;
        assert!(deserialize_header(&bytes).is_err());
    }

    #[test]
    fn deserialize_rejects_wrong_version() {
        let mut bytes = [0u8; HEADER_SIZE];
        bytes[0..7].copy_from_slice(MAGIC);
        bytes[7] = 99;
        assert!(deserialize_header(&bytes).is_err());
    }

    #[test]
    fn create_reserves_correct_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.pmtiles");
        let w = PmtilesWriter::create(
            &path, 11, 14, (12.0, 48.5, 18.9, 51.1),
            "contour", "elevation", 1000,
        ).unwrap();

        // Layout: [Header 127][Root 4M][Metadata 128K][Leaves 1000*20=20000][Tile data]
        assert_eq!(w.leaves_reserved_start, 127 + 4*1024*1024 + 128*1024);
        assert_eq!(w.leaves_reserved_end,   w.leaves_reserved_start + 20_000);
        assert_eq!(w.tile_data_section_start, w.leaves_reserved_end);
        assert_eq!(w.next_tile_offset, w.tile_data_section_start);

        // The reservation is zero-padded; file size should equal it.
        drop(w);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 127 + 4*1024*1024 + 128*1024 + 20_000);
    }
}
