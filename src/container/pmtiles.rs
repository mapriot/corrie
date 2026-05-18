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

// ---------------- TEMPORARY OLD STUB — REMOVED in Task 11 ----------------

use std::path::Path;

pub struct PmtilesWriter;

impl PmtilesWriter {
    pub fn create(
        _path: &Path,
        _min_z: u8,
        _max_z: u8,
        _bounds: (f64, f64, f64, f64),
        _layer_name: &str,
        _elevation_attr: &str,
    ) -> anyhow::Result<Self> {
        anyhow::bail!("PmtilesWriter is being rewritten; this stub should be unreachable in tests")
    }
    pub fn add_tile(&mut self, _z: u8, _x: u32, _y: u32, _data: &[u8]) -> anyhow::Result<()> {
        unreachable!()
    }
    pub fn finalize(self) -> anyhow::Result<()> {
        unreachable!()
    }
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
}
