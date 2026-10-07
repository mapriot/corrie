# corrie

Fast contour vector-tile builder. Six subcommands:

- **`corrie tile`** — read a tilemaker-format `config.json` plus the contour
  lines it lists (packs, shapefiles or other vector files), write a clustered
  **PMTiles** file. A drop-in replacement for the `tilemaker → mbtiles →
  pmtiles convert` pipeline for contour-only configs. With `--shards`, tiles
  each group of sources (for example each country) separately and only
  re-tiles groups whose inputs changed.
- **`corrie pack`** — turn contour shapefiles (or any vector file GDAL reads)
  into packs: compact `.cpk` files, about 3.6× smaller than the SHP set, that
  `tile` reads in place.
- **`corrie unpack`** — write packs back out as FlatGeobuf, GeoPackage,
  GeoJSON or Shapefile, for example to look at a country and its neighbours
  in QGIS.
- **`corrie merge`** — merge contour archives written by `corrie tile` (for
  example one per country) into one clustered archive.
- **`corrie compare`** — check that two contour archives hold the same tiles
  and the same lines, tile by tile, exactly or within a tolerance.
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

corrie needs the GDAL libraries: `pack` (and `tile`, for sources that are
not packs yet) reads vector files through GDAL/OGR, `unpack` writes them,
`build` reads rasters.

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

## `corrie tile` — contour lines to clustered PMTiles

Given a tilemaker config whose sources are contour lines (for contour data,
typically many layers that all `write_to` one output layer), `corrie tile`
produces a single clustered PMTiles file with the same layer schema. A
source is a pack (`.cpk`, see [`corrie pack`](#corrie-pack--compact-contour-sources)),
a shapefile, or any other vector file GDAL reads, in WGS84
longitude/latitude; one config can mix them.

```sh
corrie tile <CONFIG.json> <OUTPUT.pmtiles> [OPTIONS]
```

| Option | Description |
|---|---|
| `--shards <DIR>` | Tile each group of sources into `DIR/<group>.pmtiles`, reuse group archives whose inputs are unchanged, then merge them into `OUTPUT`. See [Per-group builds](#per-group-builds---shards). |
| `--cache-dir <DIR>` | Keep the packs made from sources that are not packs in `DIR`, and reuse each while its source is unchanged. Without it, they go to a temp dir that is removed when the run ends. Not with `--shards`. |
| `--rebuild-cache` | Pack the sources again even if their packs in `--cache-dir` are current. |
| `-j, --workers <N>` | Worker threads. Default: all cores. |

### How it works

1. **Packs.** Sources that are packs are opened as they are. Every other
   source is first packed (in parallel) into the cache (see
   [The cache](#the-cache)). A pack stores each line in pieces of 128
   segments with a spatial index over the pieces, so a tile only reads the
   parts of a long contour line that reach it.
2. **Tiles.** Starting from the lowest-zoom tiles that intersect the
   config's `bounding_box`, every tile that holds data is visited, then
   built in parallel: query each pack's index, decode the pieces, clip each
   line to the tile, simplify (Douglas-Peucker, tilemaker's per-zoom rules),
   quantize to the 4096 tile grid, group lines by attributes, encode MVT,
   gzip.
3. **Write.** Tiles are appended in Hilbert order into a clustered PMTiles v3
   archive with leaf directories (see [Outputs](#outputs)).

Every line is clipped, simplified and quantized on its own; lines are only
grouped by attributes at the end. This is what makes per-group builds and
`corrie merge` exact.

### The cache

The cache holds the packs of sources that are not packs. A config whose
sources are all packs needs none.

- **Default (no `--cache-dir`):** the packs go to a temp dir
  (`$TMPDIR/corrie-cache-<pid>-*`) that is removed when the run ends.
- **`--cache-dir <DIR>`:** the packs are kept in `DIR`
  (`<name>-<hash of the source path>.cpk`), and each is reused while its
  source has the same path, size, modification time and elevation column.
  `--rebuild-cache` packs every source again.
- **Stopping a run** with Ctrl+C, SIGTERM or SIGHUP (a closed terminal or
  tmux pane) removes the temp cache and the run's `.partial` output, and
  exits with status 130, also when the terminal or the pipe behind stderr is
  already gone. A file already at `OUTPUT` is left as it was. A run started
  with `nohup` keeps ignoring SIGHUP. In a kept `--cache-dir`, a pack is
  written as `.partial` and renamed when complete, so a stopped run leaves
  no half-written pack behind. A run that fails with an error removes its
  `.partial` output too.
- **SIGKILL or an out-of-memory kill** leaves the temp cache (and the
  `.partial` file) behind; the next `corrie tile` run that uses a temp cache
  removes temp caches of processes that no longer run.

Disk: a pack takes about **4.6 bytes per vertex** (a world-wide set of 42
billion vertices: about 190 GB). Use `--shards` to keep the temp cache at
the size of the largest group, or pack the sources once with `corrie pack`.

### Per-group builds (`--shards`)

```sh
corrie tile config-contours.json contours.pmtiles --shards contours-groups/
```

The config's sources are grouped by folder: the first directory below the
deepest directory that contains all the source files. With sources in
`contours/cz/data/merged_contours_1.shp` and `contours/at/final_contours_0_1.cpk`
the groups are `cz` and `at`. Files that lie directly in that common folder
form one group each, named after the file.

For each group, in name order:

- If `DIR/<group>.pmtiles` exists and was built from the same inputs, it is
  **reused**. The archive's metadata holds a key over the group's layer
  settings, the zoom range and bounding box, the path, size and
  modification time of each source file (and of an SHP's `.dbf`), and
  corrie's tiling version (bumped when a release changes the tiles it
  writes). Switching a source from its SHP to its pack changes the key, so
  that group is tiled once more; deleting the SHP afterwards does not.
- Otherwise the group is **tiled** with its own temp cache into
  `DIR/<group>.pmtiles.partial`, which is renamed to `<group>.pmtiles` when
  complete.

Then all group archives are merged into `OUTPUT` (see `corrie merge`): tiles
inside one group are copied as stored, tiles on group borders are joined.

```
181 groups in "contours-groups"
[  1/181] ae        reused
...
[ 48/181] cz        building from 5 sources
[ 48/181] cz        built: 2383086 features, 43211 tiles in 51.2s
...
Merging 181 group archives into "contours.pmtiles" ...
```

The output holds the same tiles and lines as a single run over all sources
(`corrie compare` reports them identical; line order inside a tile can
differ). The temp cache only ever holds one group's packs, the group
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

`corrie tile` parses the subset of the tilemaker config relevant for
contour lines. Unknown fields (OSM keys, Lua hooks, etc.) are silently
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
| `source` | Required. Path to a pack (`.cpk`), a `.shp` file or another vector file GDAL reads. Relative paths resolve against the config file's directory. |
| `source_columns` | Optional. The first entry is the column that holds the elevation (default `"elevation"`); not used for packs. |
| `write_to` | Optional. Names this layer's output layer. corrie writes a single MVT layer: it is named after the output layer of the alphabetically first layer key, and every layer's lines go into it. |
| `simplify_below` | Optional. Default 0. Douglas-Peucker simplification runs when `z < simplify_below`. |
| `simplify_level` | Optional. Default 0.01. Base DP epsilon in degrees of longitude/latp. |
| `simplify_length` | Optional. Default 0. Alternative base epsilon in **metres** (matches tilemaker's code). Takes precedence over `simplify_level` when set. |
| `simplify_ratio` | Optional. Default 2.0. Per-zoom scaling factor: epsilon at zoom `z` is `base * simplify_ratio^(simplify_below - 1 - z)`. |
| `simplify_algorithm` | Optional. Only Douglas-Peucker is implemented; other values are ignored. |

At every zoom, including `simplify_below` and above, the tolerance is at
least half a tile pixel, so vertices closer together than that are dropped.
Distances are measured to the segment between two kept vertices, so a line
that runs out and back keeps its tip when the tip lies farther than the
tolerance.

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

## `corrie pack` — compact contour sources

```sh
corrie pack <SOURCE>... [--field elevation] [--drop-seams DEG] [--force] [-j N]
corrie pack --config <CONFIG.json> [--only GROUPS] [--out-config FILE] [--drop-seams DEG] [--force] [-j N]
```

A pack (`.cpk`) holds one source's lines and their elevations, and nothing
else: about **4.6 bytes per vertex**, against about 16.6 for a shapefile set
(`.shp`, `.shx`, `.dbf`). `corrie tile` reads packs in place (memory-mapped),
with no ingest step and no temp cache, and gives the same tiles as from the
source the pack was made from.

- **Coordinates** are stored on a 2^32 grid over Web Mercator: a vertex moves
  by less than 1 cm. Vertices that fall into the same grid cell as the one
  before are merged.
- **Elevation** is the source column (`--field`, or with `--config` each
  layer's `source_columns`), rounded down to whole metres. Other columns are
  not kept.
- **Lines** are stored in pieces of 128 segments with a spatial index over
  the pieces, so a tile reads only the parts of a line that reach it.
- **`--drop-seams DEG`** leaves out every feature whose vertices, over all
  its parts, all lie within `DEG` degrees of a whole-degree meridian or
  parallel. Contouring a mosaic of 1° DEM tiles (JAXA AW3D30, for example)
  traces such lines where one tile's data meets nodata. A feature with one
  vertex elsewhere is kept whole. The run reports how many were left out.

Each source becomes `<name>.cpk` next to it. A pack that is up to date (made
from the same path, size, modification time, elevation column and
`--drop-seams`) is kept unless `--force` is given; packs are written as `.partial` and renamed when
complete. The run ends with the sizes:

```
17 packs: 8.39 GB of sources -> 2.31 GB of packs (3.6x smaller), 2.0s
```

### Moving a config to packs

With `--config`, `pack` packs the sources of a tilemaker config and writes a
copy of the config, `<config>-packs.json` (or `--out-config`, which may be
the config itself), whose sources are the packs. Only the `source` strings
change, so the copy keeps the config's layout and a diff shows just them.
`--only cz,at` packs and switches only those groups (the folder names
`--shards` uses), to move a few countries at a time:

```sh
corrie pack --config config-contours.json --only cz,at
diff config-contours.json config-contours-packs.json
corrie tile config-contours-packs.json contours-packs.pmtiles --shards contours-groups/
corrie compare contours.pmtiles contours-packs.pmtiles     # IDENTICAL when built by the same corrie
```

Once a config reads a pack, its source files are no longer needed for
tiling; with `--shards`, deleting them does not rebuild the group.

## `corrie unpack` — packs to GIS files

```sh
corrie unpack <OUTPUT> <PACK>... [--bbox min_lon,min_lat,max_lon,max_lat]
```

Writes the lines of packs to one file, in WGS84, the format chosen by the
extension: `.fgb` (FlatGeobuf), `.gpkg`, `.geojson` or `.shp`. Each line has
`elevation`, `main`, `show` (as in the tiles), `source` (the pack's file
name) and `folder` (the pack's folder, for example its country), so a country
and its neighbours can be checked in QGIS side by side, styled by `folder`:

```sh
corrie unpack border.fgb contours/xk/*.cpk contours/me/*.cpk --bbox 19.7,42.4,20.6,43.1
```

With `--bbox`, only lines touching the box are written, each one whole.

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
corrie compare <A.pmtiles> <B.pmtiles> [--show 10] [--tolerance PX] [--json FILE]
```

Matches tiles by z/x/y. Tiles with different bytes are decoded and their
lines compared per `(elevation, main, show)` group, ignoring line order.
Exits 0 when the archives are identical (or, with `--tolerance`, when every
difference is within it), 1 otherwise.

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

`--tolerance PX` compares archives whose lines are expected to move a
little, for example builds by two corrie versions. Lines then count as the
same when every vertex inside the tile lies within `PX` tile pixels of the
other side's lines, and the other way round. A tile is 4096 tile pixels and
is drawn 512 screen pixels wide, so `--tolerance 8` is one screen pixel.
Deviations are printed in both units, and two more kinds appear:

- **within the tolerance** — the lines differ, but by at most `PX`.
- **only on one side, a sliver at the tile edge** — a tile only one archive
  has, whose lines all lie within `PX` of the tile's edge: a line moved just
  across it. Such tiles count as matching and are not listed under "only in
  a" or "only in b".

`--json FILE` writes every differing tile with its kind, point counts and
largest deviation.

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

Measured `corrie tile` runs on Madagascar, Kosovo, Montenegro, Réunion and
the Comoros (17 SHPs, 506 M vertices, 163,365 tiles), Apple M-series laptop,
10 workers, files in the page cache:

| Run | Time | Peak RSS |
|---|---|---|
| 0.2.0, from the SHPs | 30.9 s | 12.4 GB |
| 0.3.0, from the SHPs (packs them first: 2.7 s) | 10.9 s | 2.7 GB |
| 0.3.0, from packs | 7.9 s | 2.6 GB |
| `corrie pack` of the 17 SHPs | 2.2 s | 0.6 GB |

Both 0.3.0 runs write byte-identical archives. Against 0.2.0 the lines move
by at most one screen pixel in all but 3,005 tiles; those are the line tips
0.2.0's simplification cut off (see the changelog).

Earlier, 0.2.0 made `tile` about 7× faster than 0.1.0 on Madagascar (178 s →
26 s; the tile phase about 10×) by indexing lines in 128-segment pieces
instead of whole.

- **RAM:** packs are memory-mapped, so the peak RSS above is mostly packs
  in the page cache, which the kernel can drop under pressure. Besides
  them, a run holds the list of tiles to build and the output's tile
  directory (tens of bytes per tile) and one tile's lines per worker. With
  `--shards`, one group's packs are open at a time.
- **Disk:** packs take about 4.6 bytes per vertex. A config of SHPs needs a
  temp cache of that size (see [The cache](#the-cache)); with `--shards`,
  only for the largest group at a time, plus the group archives.

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
