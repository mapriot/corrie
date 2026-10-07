# corrie

Fast contour vector-tile builder. Four subcommands:

- **`corrie tile`** — read a tilemaker-format `config.json` plus the contour
  shapefiles it lists, write a clustered **PMTiles** file. A drop-in
  replacement for the `tilemaker → mbtiles → pmtiles convert` pipeline for
  contour-only configs. With `--shards`, tiles each group of sources (for
  example each country) separately and only re-tiles groups whose inputs
  changed.
- **`corrie merge`** — merge contour archives written by `corrie tile` (for
  example one per country) into one clustered archive.
- **`corrie compare`** — check that two contour archives hold the same tiles
  and the same lines, tile by tile.
- **`corrie build`** — read a single elevation raster (any GDAL-supported
  format/CRS), run marching-squares contour extraction, and write tiles as
  PMTiles or MBTiles. No shapefiles involved.

Output is ready to serve with [Martin](https://martin.maplibre.org/),
[go-pmtiles](https://github.com/protomaps/go-pmtiles), or directly to a
browser via [MapLibre GL JS](https://maplibre.org/).

## Install

```sh
cargo install --path .
```

corrie needs the GDAL libraries: `tile` reads shapefiles through GDAL/OGR,
`build` reads rasters through GDAL.

The released `gdal` crate (0.19) does not build against GDAL 3.13 yet (for
example current Homebrew). Until a release supports it, build against the
crate's main branch without editing `Cargo.toml`:

```sh
git clone https://github.com/georust/gdal ../georust-gdal
cargo build --release \
  --config 'patch.crates-io.gdal.path="../georust-gdal"' \
  --config 'patch.crates-io.gdal-sys.path="../georust-gdal/gdal-sys"'
```

---

## `corrie tile` — shapefiles to clustered PMTiles

Given a tilemaker config that lists `.shp` files as sources (for contour
data, typically many layers that all `write_to` one output layer),
`corrie tile` produces a single clustered PMTiles file with the same layer
schema.

```sh
corrie tile <CONFIG.json> <OUTPUT.pmtiles> [OPTIONS]
```

| Option | Description |
|---|---|
| `--shards <DIR>` | Tile each group of sources into `DIR/<group>.pmtiles`, reuse group archives whose inputs are unchanged, then merge them into `OUTPUT`. See [Per-group builds](#per-group-builds---shards). |
| `--cache-dir <DIR>` | Keep the feature store cache in `DIR` and reuse it while the SHPs are unchanged. Without it, a temp cache is used and removed when the run ends. Not with `--shards`. |
| `--rebuild-cache` | Re-ingest even if the `--cache-dir` cache is valid. |
| `-j, --workers <N>` | Worker threads. Default: all cores. |

### How it works

1. **Ingest.** Every SHP is read once (in parallel) into a feature store
   cache: the line vertices in Web Mercator latitude, one record per line
   (elevation, zoom gate, flags), and a spatial index. Each line is indexed
   in pieces of 128 segments, so a tile only reads the parts of a long
   contour line that reach it.
2. **Tiles.** Starting from the lowest-zoom tiles that intersect the
   config's `bounding_box`, every tile that holds data is visited, then
   built in parallel: query the index, clip each line to the tile, simplify
   (Douglas-Peucker, tilemaker's per-zoom rules), quantize to the 4096 tile
   grid, group lines by attributes, encode MVT, gzip.
3. **Write.** Tiles are appended in Hilbert order into a clustered PMTiles v3
   archive with leaf directories (see [Outputs](#outputs)).

Every line is clipped, simplified and quantized on its own; lines are only
grouped by attributes at the end. This is what makes per-group builds and
`corrie merge` exact.

### The cache

- **Default (no `--cache-dir`):** the cache goes to a temp dir
  (`$TMPDIR/corrie-cache-<pid>-*`) that is removed when the run ends.
- **`--cache-dir <DIR>`:** the cache is kept in `DIR` and reused while the
  SHP set (paths, sizes, modification times) is unchanged. It is rebuilt
  when an SHP changes, is added or removed, or with `--rebuild-cache`.
- **Stopping a run** with Ctrl+C, SIGTERM or SIGHUP (a closed terminal or
  tmux pane) removes the temp cache and the run's `.partial` output, and
  exits with status 130, also when the terminal or the pipe behind stderr is
  already gone. A file already at `OUTPUT` is left as it was. A run started
  with `nohup` keeps ignoring SIGHUP. A kept `--cache-dir` cache stays; if
  the run was stopped while ingesting, the cache is marked incomplete and
  the next run ingests again. A run that fails with an error removes its
  `.partial` output too.
- **SIGKILL or an out-of-memory kill** leaves the temp cache (and the
  `.partial` file) behind; the next `corrie tile` run that uses a temp cache
  removes temp caches of processes that no longer run.

Disk: the cache takes about **16 bytes per input vertex** plus 36 bytes per
line (a world-wide set of 42 billion vertices needs about 690 GB). Use
`--shards` to keep it at the size of the largest group instead.

### Per-group builds (`--shards`)

```sh
corrie tile config-contours.json contours.pmtiles --shards contours-groups/
```

The config's sources are grouped by folder: the first directory below the
deepest directory that contains all the SHPs. With sources in
`contours/cz/data/merged_contours_1.shp` and `contours/at/final_contours_0_1.shp`
the groups are `cz` and `at`. SHPs that lie directly in that common folder
form one group each, named after the file.

For each group, in name order:

- If `DIR/<group>.pmtiles` exists and was built from the same inputs, it is
  **reused**. The archive's metadata holds a key over the group's layer
  settings, the zoom range and bounding box, the size and modification time
  of each `.shp` and `.dbf`, and corrie's tiling version (bumped when a
  release changes the tiles it writes).
- Otherwise the group is **tiled** with its own temp cache into
  `DIR/<group>.pmtiles.partial`, which is renamed to `<group>.pmtiles` when
  complete.

Then all group archives are merged into `OUTPUT` (see `corrie merge`): tiles
inside one group are copied as stored, tiles on group borders are joined.

```
181 groups in "contours-groups"
[  1/181] ae        reused
...
[ 48/181] cz        building from 5 SHPs
[ 48/181] cz        built: 2383086 features, 43211 tiles in 51.2s
...
Merging 181 group archives into "contours.pmtiles" ...
```

The output holds the same tiles and lines as a single run over all sources
(`corrie compare` reports them identical; line order inside a tile can
differ). The temp cache only ever holds one group, the group
archives stay in `DIR` (together about the size of the output), and a rerun
only tiles new or changed groups. Archives in `DIR` that belong to no group
of the config, and `.partial` files left by a killed run, are reported and
left alone.

### Outputs

`tile` and `merge` write `OUTPUT.partial` next to `OUTPUT` and rename it to
`OUTPUT` when it is complete:

- A file already at `OUTPUT` stays valid and readable until then, so a tile
  server can keep serving it, and `merge` can write over one of its inputs.
  The disk needs room for both the old and the new file.
- `OUTPUT` is checked before any work starts: its folder must exist, and an
  existing `OUTPUT` must be a regular file (not a folder or a device).
- If `OUTPUT` is a symlink, the new file is written next to the file it
  points to and replaces that file; the link stays. Other hard links to the
  old file keep the old content.
- The `.partial` file is locked while a run writes it. A second run writing
  the same `OUTPUT` (or two `--shards` runs on one `DIR`) stops with
  "another corrie is writing …".
- If the final rename fails, the finished file is kept at `OUTPUT.partial`
  and the error says so.

### Supported `config.json` fields

`corrie tile` parses the subset of the tilemaker config relevant for the
SHP/contour path. Unknown fields (OSM keys, Lua hooks, etc.) are silently
ignored.

**Top-level `settings`:**

| Field | Notes |
|---|---|
| `minzoom`, `maxzoom` | Required. Inclusive range of zoom levels to write. |
| `basezoom` | Optional. Not used for tile output (informational). |
| `include_ids` | Optional. Read but not used: features always carry an id. |
| `bounding_box` | Required. `[minLon, minLat, maxLon, maxLat]`. Tiles are written within the lowest-zoom (`minzoom`) tiles that intersect this box; data outside those tiles is ignored. |
| `compress` | Optional. Ignored: tiles are always gzipped (PMTiles default). |
| `name`, `version`, `description` | Read but not written to the output. |

**Per layer in `layers`:**

| Field | Notes |
|---|---|
| `minzoom`, `maxzoom` | Required. Layer-level zoom gate (in addition to per-feature minzoom). |
| `source` | Required. Path to a `.shp` file. Relative paths resolve against the config file's directory. |
| `source_columns` | Optional. The first entry is used as the DBF column name for elevation (default `"elevation"`). |
| `write_to` | Optional. Names this layer's output layer. corrie writes a single MVT layer: it is named after the output layer of the alphabetically first layer key, and every layer's lines go into it. |
| `simplify_below` | Optional. Default 0. Douglas-Peucker simplification runs when `z < simplify_below`. |
| `simplify_level` | Optional. Default 0.01. Base DP epsilon in degrees of longitude/latp. |
| `simplify_length` | Optional. Default 0. Alternative base epsilon in **metres** (matches tilemaker's code). Takes precedence over `simplify_level` when set. |
| `simplify_ratio` | Optional. Default 2.0. Per-zoom scaling factor: epsilon at zoom `z` is `base * simplify_ratio^(simplify_below - 1 - z)`. |
| `simplify_algorithm` | Optional. Only Douglas-Peucker is implemented; other values are ignored. |

At every zoom, including `simplify_below` and above, the tolerance is at
least half a tile pixel, so vertices closer together than that are dropped.

### Attribute derivation (compiled in)

Each contour line is tagged with `elevation`, plus `main` and `show`
booleans, based on its elevation in metres. The rules are hard-coded and
match the `process-contours.lua` from
[mapriot](https://github.com/pdrhlik/mapriot) exactly:

| Elevation | `minzoom` | `main` | `show` |
|---|---|---|---|
| `% 100 == 0` | 11 | 1 | 1 |
| `% 50 == 0` (not 100) | 11 | 1 | — |
| `% 20 == 0` (not 50) | 12 | — | — |
| `% 10 == 0` (not 20) | 13 | — | — |
| otherwise (e.g. 5 m steps) | 14 | — | — |

`minzoom` here is the per-feature zoom gate: the feature is emitted only at
tiles whose zoom is ≥ its computed minzoom **and** also within the layer's
own `minzoom`–`maxzoom` range.

### Output MVT schema

One layer, named after the output layer (`write_to`, or the layer key) of
the alphabetically first layer key in the config; for contour configs where
every layer writes to `contour`, that is `contour`. Each feature has:

- `elevation`: integer (metres)
- `main`: integer `1` when present (omitted otherwise)
- `show`: integer `1` when present (omitted otherwise)

Geometry is `LineString` or `MultiLineString` — sub-linestrings sharing the
same `(elevation, main, show)` attributes within a tile are packed into one
MultiLineString feature to keep tile size down.

### Example: minimal config

```json
{
  "settings": {
    "minzoom": 11, "maxzoom": 14, "basezoom": 14,
    "include_ids": false,
    "bounding_box": [12.0, 48.5, 18.9, 51.1],
    "compress": "gzip"
  },
  "layers": {
    "contour": {
      "minzoom": 11, "maxzoom": 14,
      "source": "data/contours_1.shp",
      "source_columns": ["elevation"],
      "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2
    },
    "contour_2": {
      "minzoom": 11, "maxzoom": 14,
      "source": "data/contours_2.shp",
      "source_columns": ["elevation"],
      "write_to": "contour",
      "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2
    }
  }
}
```

Run with:

```sh
corrie tile config.json out.pmtiles
```

---

## `corrie merge` — join contour archives

```sh
corrie merge <OUTPUT.pmtiles> <INPUT.pmtiles>... [-j N]
```

Merges archives written by `corrie tile`, for example one per country, into
one clustered archive. A tile present in one input is copied byte for byte.
A tile present in several inputs (on their borders) is decoded, its lines
are combined per `(elevation, main, show)`, and it is encoded again.

Because tiling is per line, merging archives tiled from separate sources
gives the same tiles and lines as tiling all the sources in one run. Build
the inputs with the same zoom range and layer settings. All inputs must use
the same layer name, which the output keeps; an input listed twice is
refused. The output's zoom range spans all inputs and its bounds are the
union of theirs.

The output is written as `OUTPUT.partial` and renamed when complete (see
[Outputs](#outputs)), so it can be one of the inputs:

```sh
corrie merge world.pmtiles world.pmtiles new-country.pmtiles
```

`merge` reads only corrie's contour schema; it is not a general PMTiles
merger. It holds the directory entries of all inputs in memory, about 60
bytes per tile (a world archive with 24 million tiles: about 1.5 GB).

## `corrie compare` — same tiles, same lines?

```sh
corrie compare <A.pmtiles> <B.pmtiles> [--show 10]
```

Matches tiles by z/x/y. Tiles with different bytes are decoded and their
lines compared per `(elevation, main, show)` group, ignoring line order.
Exits 0 when the archives are identical, 1 otherwise.

```
tiles: a=23918844 b=109272 common=109272
only in a: 23809572 11/688/151 11/689/151 11/690/150
only in b: 0
common tiles with different lines: 5479
  a has every line of b, plus more: 5479
    11/1152/702  points a=7598 b=1123
```

A differing tile is reported as one of:

- **a has every line of b, plus more** — e.g. comparing a world archive with
  a build of a few countries: the world's border tiles also hold the
  neighbours' lines.
- **b has every line of a, plus more** — the same, the other way round.
- **lines changed** — neither contains the other: lines moved or differ.

---

## `corrie build` — DEM-direct contour tiles

For a single elevation raster, skip the gdal_contour → shapefile step and
generate contour tiles directly. Uses marching-squares per tile.

```sh
corrie build <INPUT> <OUTPUT> [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `<INPUT>` | — | Elevation raster (GeoTIFF, VRT, any GDAL format/CRS) |
| `<OUTPUT>` | — | `.pmtiles` or `.mbtiles` (chosen by extension) |
| `--min-z <N>` | `11` | Minimum zoom |
| `--max-z <N>` | `14` | Maximum zoom |
| `--levels <PAIRS>` | `11:50,12:20,13:10,14:5` | Zoom:interval pairs |
| `--main-every <N>` | `50` | Tag contours divisible by N as `main=1` |
| `--show-every <N>` | `100` | Tag contours divisible by N as `show=1` |
| `--layer <NAME>` | `contour` | MVT layer name |
| `--elevation-attr <NAME>` | `elevation` | MVT attribute name for elevation |
| `--nodata <VALUE>` | *(from raster)* | Override nodata value |
| `--simplify <EPS>` | `2.0` | DP tolerance in MVT pixels (`0` = off) |
| `-j, --workers <N>` | *(all CPUs)* | Worker thread count |

The `--levels` flag takes comma-separated `ZOOM:INTERVAL` pairs. Each pair
says "draw a contour every `INTERVAL` metres starting at `ZOOM` and at all
higher zooms". With the default:

| Zoom | Active intervals | Lines drawn every |
|---|---|---|
| 11 | 50 | 50 m |
| 12 | 50, 20 | 20 m |
| 13 | 50, 20, 10 | 10 m |
| 14 | 50, 20, 10, 5 | 5 m |

### Examples

```sh
# Default: z11–z14 from a GeoTIFF
corrie build dem.tif out.pmtiles

# Custom zoom range, custom intervals
corrie build dem.tif out.pmtiles --min-z 10 --max-z 15 --levels "10:100,12:50,14:10"

# MBTiles output, more aggressive line simplification
corrie build dem.tif out.mbtiles --simplify 4.0

# Multi-raster mosaic via VRT
gdalbuildvrt merged.vrt tile1.tif tile2.tif tile3.tif
corrie build merged.vrt out.pmtiles
```

---

## Performance, memory and disk

Measured `corrie tile` runs, same input and workers for both versions, with
the same tiles and lines (`corrie compare`):

| Input | Machine, workers | 0.1.0 | 0.2.0 |
|---|---|---|---|
| Madagascar, 466 M vertices | Apple M-series laptop, 10 | 178 s | 26 s |
| Czechia + Austria + Slovakia, 629 M vertices | x86-64 server shared with another build, 3 | 902 s (tiles: 839 s) | 127 s (tiles: 79 s) |

Most of the gain is in the tile phase: 0.1.0 indexed whole contour lines, so
every tile a long line's bounding box touched scanned all of that line's
vertices (60–1,650× more vertex visits than vertices on the data above).
After it, ingest (reading the SHPs through GDAL) is the larger part.

- **RAM during ingest:** each worker reads one whole SHP into memory and up to
  `workers + 2` more wait for the writer, so the SHPs take up to about
  `(2 × workers + 2) ×` the largest SHP. Lower `-j` if SHPs are large. The
  index adds to that: its entry list grows while reading, and the R-tree is
  built from it at the end, about 2.2 × the size of `rtree.bin` at the peak.
- **RAM during tiling:** the R-tree is held in memory, about 1.45 × the size
  of `rtree.bin` (which is about 0.5 bytes per input vertex). The vertices
  are memory-mapped from the cache and served from the page cache.
- **Example:** a world-wide set of 42 billion vertices has an `rtree.bin` of
  about 20 GB: about 45 GB of RAM at the end of ingest and 30 GB while
  tiling. With `--shards` these apply to the largest group only.
- **Disk:** the cache is about 16 bytes per input vertex (see
  [The cache](#the-cache)). With `--shards`, only the largest group's cache
  exists at a time, plus the group archives.

Output is a clustered PMTiles file (Hilbert-ordered tile directory),
suitable for direct serving without re-clustering.

---

## Using output with MapLibre GL JS

PMTiles output includes `vector_layers` metadata so MapLibre can introspect
the source. A minimal style:

```json
{
  "id": "contours",
  "type": "line",
  "source": "terrain",
  "source-layer": "contour",
  "paint": {
    "line-color": "#8a7048",
    "line-width": ["case", ["==", ["get", "main"], 1], 1.5, 0.75],
    "line-opacity": 0.6
  }
}
```

Label the index (`main`) contours:

```json
{
  "id": "contour-labels",
  "type": "symbol",
  "source": "terrain",
  "source-layer": "contour",
  "filter": ["==", ["get", "main"], 1],
  "layout": {
    "symbol-placement": "line",
    "text-field": ["concat", ["to-string", ["get", "elevation"]], " m"]
  }
}
```

## License

MIT — see [LICENSE](LICENSE)
