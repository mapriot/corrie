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
}
