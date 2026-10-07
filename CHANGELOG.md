# Changelog

## 0.3.0 — 2026-10-07

### Added

- `corrie pack`: turn contour sources (shapefiles or any vector file GDAL
  reads) into packs, `.cpk` files of about 4.6 bytes per vertex, 3.6× smaller
  than the SHP set. Vertices are stored on a 2^32 Web Mercator grid (they
  move by less than 1 cm), each line in 128-segment pieces with a spatial
  index, read in place. `--config` packs a tilemaker config's sources and
  writes a copy of the config that reads the packs, changing only the
  `source` strings; `--only GROUPS` does this for a few groups at a time.
- `corrie unpack`: write packs as FlatGeobuf, GeoPackage, GeoJSON or
  Shapefile, with `elevation`, `main`, `show`, `source` and `folder`, for
  example to check neighbouring countries in QGIS. `--bbox` limits it to
  the lines touching a box.
- `corrie compare --tolerance PX`: count lines as the same when they lie
  within `PX` tile pixels of each other (8 = one screen pixel), for builds by
  different versions. `--json FILE` writes every differing tile.

### Changed

- `tile` reads packs as sources, and packs any other source before tiling
  instead of ingesting it into a feature store. On a 506 M vertex test set:
  2.8× faster from SHPs and 3.9× faster from packs than 0.2.0, peak RSS
  12.4 → 2.7 GB. The temp cache is about 3.6× smaller; a config of packs
  needs none. Tiling from a source or from its pack gives byte-identical
  archives.
- `--cache-dir` keeps one pack per source and reuses each while its source
  is unchanged. Caches written by 0.2.0 are not read and can be deleted.
- The `--shards` key covers a pack's own file only, so deleting the SHPs a
  pack was made from does not rebuild the group.
- Tiling version 2: the tiles change (see Fixed), so `--shards` group
  archives built by 0.2.0 are tiled again once.
- Subcommands listed as tile, pack, unpack, merge, compare, build.

### Fixed

- Douglas-Peucker simplification measured each point's distance to the
  infinite line through the chord instead of to the chord itself. Where a
  contour runs out and back, the tip could be cut off depending on
  sub-centimetre differences in the input; on real data the drawn line
  strayed up to 30 tile pixels (about 4 screen pixels) from the source
  line, against at most the tolerance now. About 2% of tiles change by more
  than one screen pixel.

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
