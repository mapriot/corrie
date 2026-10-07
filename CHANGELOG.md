# Changelog

## 0.2.0 — 2026-10-07

### Added

- `corrie tile --shards DIR`: tile each group of sources (each folder below
  the common source folder, for example one per country) into its own
  archive, reuse archives whose inputs are unchanged, and merge them into the
  output. The temp cache holds one group at a time instead of all input, and
  reruns only tile new or changed groups. The output has the same tiles and
  lines as a single run (`corrie compare`).
- `corrie merge`: merge contour archives into one. Tiles present in one input
  are copied as stored; tiles on borders are decoded, joined and encoded
  again. The output may be one of the inputs.
- `corrie compare`: check tile by tile that two archives hold the same lines,
  and report how differing tiles differ.

### Changed

- The spatial index holds 128-segment pieces of each line instead of whole
  lines, so a tile no longer scans every vertex of each long contour line
  that touches it. The tile phase is about 10× faster on real data, with the
  same tiles and lines (`corrie compare`). The index is about 2.4× larger
  (~0.5 bytes per input vertex on disk) and is streamed to and from disk.
- Tiles are gzipped on the worker threads instead of the single writer
  thread, and the writer no longer seeks before every tile.
- `tile` and `merge` write their output as `<name>.partial` and rename it
  when complete: an existing output stays valid until the new one is
  finished. The output path is checked before the run starts, a symlinked
  output is written through to the file it points to, only one run at a
  time can write a given output, and a failed final rename keeps the
  finished file.
- `bounding_box` selects the lowest-zoom tiles that intersect it; tiles are
  written within those. (0.1.0 started from the box intersected with the
  data's extent, which differed only at the box's edges.)
- Cache format version 2: caches written by 0.1.0 are rebuilt.
- CLI help for every argument; subcommands listed in workflow order.
- README rewritten: GDAL is required for `tile`, real memory and disk
  sizing, corrie writes one MVT layer, and the config fields that are read
  but not used.

### Fixed

- Stopping `tile` or `merge` with Ctrl+C, SIGTERM or SIGHUP left the temp
  cache behind (hundreds of GB on large inputs). The cache and the partial
  output are now removed and the process exits with status 130. A run under
  `nohup` still ignores SIGHUP.
- An interrupted re-ingest into `--cache-dir` kept the previous manifest, so
  the next run trusted a half-written cache; an SHP changed during ingest
  could also leave a cache that looked valid.
- A message written to a closed terminal or a broken pipe on stderr (for
  example a `tee` killed by Ctrl+C) crashed the run.
- A line ending exactly on the west or north edge of a lowest-zoom tile was
  missing its sliver in the neighbouring tile.

## 0.1.0

- `corrie tile`: tilemaker config + contour shapefiles → clustered PMTiles.
- `corrie build`: DEM raster → contour PMTiles or MBTiles.
