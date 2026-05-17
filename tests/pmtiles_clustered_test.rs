//! Verify the existing `container::pmtiles::PmtilesWriter` produces clustered
//! output. If this test fails, fix the writer before continuing.

#[test]
fn writer_produces_clustered_output() {
    use corrie::container::pmtiles::PmtilesWriter;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smoke.pmtiles");
    let mut w = PmtilesWriter::create(
        &path, 0, 1, (-180.0, -85.0, 180.0, 85.0), "contour", "elevation",
    ).unwrap();

    // Three tiny tiles.
    let dummy = vec![0u8; 16];
    w.add_tile(0, 0, 0, &dummy).unwrap();
    w.add_tile(1, 0, 0, &dummy).unwrap();
    w.add_tile(1, 1, 1, &dummy).unwrap();
    w.finalize().unwrap();

    // Read the header bytes: the PMTiles v3 header has `clustered` at byte 96.
    // Byte layout (per the pmtiles-0.21 crate's header.rs write_to impl):
    //   0- 6  magic "PMTiles" (7 bytes)
    //   7      version
    //   8-15   root_offset
    //  16-23   root_length
    //  24-31   metadata_offset
    //  32-39   metadata_length
    //  40-47   leaf_offset
    //  48-55   leaf_length
    //  56-63   data_offset
    //  64-71   data_length
    //  72-79   n_addressed_tiles
    //  80-87   n_tile_entries
    //  88-95   n_tile_contents
    //  96      clustered  ← 0x01 = true
    //  97      internal_compression
    //  98      tile_compression
    //  99      tile_type
    // (Note: the PMTiles v3 spec document says byte 121, but the actual offset
    //  in this crate's serialisation is 96.)
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > 128, "header truncated");
    let clustered = bytes[96];
    assert_eq!(clustered, 1, "PMTiles header must have clustered=true (byte 96)");
}
