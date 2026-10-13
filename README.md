# corrie

Fast contour vector-tile generator from contour lines or elevation rasters.

Turns contour lines — FlatGeobuf, GeoPackage, shapefiles, anything GDAL reads — or a DEM into clustered [PMTiles](https://docs.protomaps.com/pmtiles/) vector tiles, ready for [MapLibre GL](https://maplibre.org/), [Martin](https://martin.maplibre.org/) or any PMTiles server. Scales from a single raster to a worldwide contour layer in one file.

Built as a fast Rust replacement for the [tilemaker](https://github.com/systemed/tilemaker) contour pipeline (`tilemaker → mbtiles → pmtiles convert`) — it reads the same `config.json`, uses all CPU cores via [rayon](https://github.com/rayon-rs/rayon), and keeps working where tilemaker runs out of memory and disk:

- **Builds per country, merges into one file.** `--shards` tiles each group of sources (a country folder) on its own, reuses every group whose inputs did not change, and merges them. Replacing one country re-tiles only that country.
- **Compact inputs.** `corrie pack` stores contour lines in `.cpk` packs, about 3.6× smaller than shapefiles and read in place — no ingest step, no temp cache.
- **Checkable.** `corrie compare` tells whether two builds hold the same lines in every tile, exactly or within N pixels.
- **Safe for long runs.** The output is written beside the live file and swapped in when complete; Ctrl+C or a closed terminal cleans up after itself.
- **Inspectable.** `corrie unpack` writes packs back out as GeoPackage, FlatGeobuf, GeoJSON or Shapefile for QGIS — neighbouring countries in one file, to check their lines meet at the border.

## Installation

Three ways to get corrie, easiest first.

### Docker — no GDAL or Rust needed

Only Docker is required. Build the image once, then run it:

```bash
git clone https://github.com/mapriot/corrie && cd corrie
docker build -t corrie .
docker run --rm -v "$PWD:/data" corrie tile /data/config.json /data/contours.pmtiles
```

Bind-mount the directory holding your contour lines, rasters and config (and where output goes) at `/data`; paths are resolved inside the container, and a config's sources relative to the config.

### From crates.io

Needs GDAL installed — see [prerequisites](#gdal-prerequisites) below.

```bash
cargo install corrie
```

### From source

```bash
git clone https://github.com/mapriot/corrie
cd corrie
cargo build --release   # binary at target/release/corrie
```

On macOS with Homebrew GDAL you may need `PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig" cargo build --release`.

### GDAL prerequisites

Native builds (crates.io / source) need GDAL on your system:

| Platform | Command |
|---|---|
| macOS | `brew install gdal` |
| Ubuntu / Debian | `sudo apt install libgdal-dev gdal-bin` |
| Fedora / RHEL | `sudo dnf install gdal-devel` |
| Windows | [OSGeo4W](https://trac.osgeo.org/osgeo4w/) or [Conda](https://anaconda.org/conda-forge/gdal) — ensure `gdal-config` is on your PATH *(untested)* |

**Supported GDAL versions: 3.5–3.12** — the versions the `gdal` Rust crate ships bindings for. GDAL **3.13 and newer are not yet supported** and will fail to build. If your package manager installs 3.13+ (current Homebrew `gdal` does), install a supported build via conda-forge — `conda install -c conda-forge 'gdal>=3.5,<3.13'` — use Docker, or build against the `gdal` crate's main branch:

```bash
git clone https://github.com/georust/gdal ../georust-gdal
cargo install --path . \
  --config 'patch.crates-io.gdal.path="../georust-gdal"' \
  --config 'patch.crates-io.gdal-sys.path="../georust-gdal/gdal-sys"'
```

## Usage

| Command | What it does |
|---|---|
| [`tile`](#corrie-tile--contour-lines-to-clustered-pmtiles) | Contour lines listed in a `config.json` → clustered PMTiles; `--shards` for per-country builds |
| [`pack`](#corrie-pack--compact-contour-sources) | Vector files → compact `.cpk` packs that `tile` reads in place |
| [`unpack`](#corrie-unpack--packs-to-gis-files) | Packs → GeoPackage, FlatGeobuf, GeoJSON or Shapefile, e.g. for QGIS |
| [`merge`](#corrie-merge--join-contour-archives) | Join archives written by `tile` (e.g. one per country) into one |
| [`compare`](#corrie-compare--same-tiles-same-lines) | Do two archives hold the same lines, tile by tile? |
| [`build`](#corrie-build--dem-direct-contour-tiles) | A DEM → contour tiles directly (PMTiles or MBTiles) |

`corrie <command> --help` lists every option.

### Quick start

```bash
# A DEM straight to contour tiles — 50 m lines from z11 down to 5 m at z14
corrie build dem.tif contours.pmtiles

# Contour lines (made with gdal_contour) listed in a tilemaker-style config.json
corrie tile config.json contours.pmtiles

# Many countries: one archive per country folder, reused while unchanged, merged into one file
corrie tile config.json world.pmtiles --shards groups/

# Pack a country's contour lines into one compact .cpk
corrie pack contours/cz/*.fgb -o contours/cz/cz.cpk

# Do two builds hold the same lines in every tile?
corrie compare old.pmtiles new.pmtiles

# Neighbouring countries' lines in one file for QGIS
corrie unpack border.gpkg contours/cz/cz.cpk contours/at/at.cpk
```

To look at a result, drop the `.pmtiles` file on [pmtiles.io](https://pmtiles.io), or style it with MapLibre (see [Using with MapLibre GL](#using-with-maplibre-gl)).

`build` contours one raster (or a VRT mosaic) tile by tile, any format and CRS GDAL reads, with configurable intervals (see [`corrie build`](#corrie-build--dem-direct-contour-tiles)). For large areas, built and updated country by country, contour with GDAL and use `tile`.

### From contour lines

Make the lines with GDAL (and clip or filter them as you like), then tile them. The lines must be in WGS84 longitude/latitude, with the elevation in a column (`elevation` by default):

```sh
gdalwarp -t_srs EPSG:4326 dem.tif dem-4326.tif      # if the DEM is in another CRS
gdal_contour -a elevation -i 10 dem-4326.tif contours.fgb
```

`config.json` (tilemaker's format, the fields corrie reads):

```json
{
  "settings": {
    "minzoom": 11,
    "maxzoom": 14,
    "bounding_box": [-180, -85, 180, 85]
  },
  "layers": {
    "contour": {
      "minzoom": 11, "maxzoom": 14,
      "source": "contours.fgb",
      "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2
    }
  }
}
```

```sh
corrie tile config.json contours.pmtiles
```

### Many countries, one file

Put each country's (or region's) lines in its own folder, pack each into one file, and list every pack in the config, all writing to one layer:

```sh
corrie pack contours/east/*.fgb -o contours/east/east.cpk
corrie pack contours/west/*.fgb -o contours/west/west.cpk
corrie tile world.json world.pmtiles --shards groups/
```

`world.json` has the same `settings` as above, and one layer per region:

```json
"layers": {
  "east": { "minzoom": 11, "maxzoom": 14, "source": "contours/east/east.cpk", "write_to": "contour",
            "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2 },
  "west": { "minzoom": 11, "maxzoom": 14, "source": "contours/west/west.cpk", "write_to": "contour",
            "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2 }
}
```

```
2 groups in "groups/"
[1/2] east  building from 1 source
[1/2] east  built: 753 features, 175 tiles in 0.0s
[2/2] west  building from 1 source
[2/2] west  built: 951 features, 262 tiles in 0.0s
Merging 2 group archives into "world.pmtiles" ...
Merged: 412 tiles, 25 joined from several groups, in 0.0s
Done in 0.1s: 2 groups (2 built, 0 reused), 412 tiles
```

When a region's lines change, pack it again and run the same `tile` command: only that region is tiled again, the others are reused, and everything is merged.

## Limitations

- **Fixed attributes.** `tile` writes one MVT layer whose lines carry
  `elevation`, `main` and `show`, with fixed zoom rules for 100, 50, 20, 10
  and 5 m lines (see [Attribute derivation](#attribute-derivation-compiled-in)).
  Other rules mean changing `src/attribute_rules.rs`. `build` has options for
  its intervals and `main`/`show`.
- **WGS84 input.** Lines for `tile` and `pack` must be in longitude/latitude
  (EPSG:4326); they are not reprojected.
- **Contours only.** corrie reads the contour subset of tilemaker's config
  (no OSM data, no Lua) and writes gzipped MVT.

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
booleans, based on its elevation in metres. The rules are hard-coded:

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

A minimal config is in the [Quick start](#from-contour-lines).

---

## `corrie pack` — compact contour sources

```sh
corrie pack <SOURCE>... [-o PACK] [--field elevation] [--drop-seams DEG] [--force] [-j N]
corrie pack --config <CONFIG.json> [--per-group] [--only GROUPS] [--out-config FILE] [--drop-seams DEG] [--force] [-j N]
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

Each source becomes `<name>.cpk` next to it, or with `-o PACK` all of them
become one pack, in the order given: a region tiled in pieces (a mosaic of
contour files) is best packed into one file. Sources can be packs too: their
lines are copied as stored, so packing packs again loses nothing
(`--drop-seams` applies to vector files only).

A pack that is up to date (made from the same sources, in the same order,
with the same sizes, modification times, elevation column and `--drop-seams`)
is kept unless `--force` is given; packs are written as `.partial` and renamed
when complete. The run ends with the sizes:

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

### One pack per group

`--per-group` packs each group's sources, files or packs, into one pack,
`<folder>/<group>.cpk`, and writes a config copy with one layer per group,
named after it. It turns a config of many files per country into one pack
per country, for example after shapefile size limits forced a country into
tiles:

```sh
corrie pack --config config-contours.json --per-group      # contours/cn/*.cpk -> contours/cn/cn.cpk, …
corrie tile config-contours-packs.json contours-new.pmtiles --shards contours-groups/
corrie compare contours.pmtiles contours-new.pmtiles       # IDENTICAL
```

A group's layers must agree on everything but their source; the rewritten
layers name their output layer (`write_to`) explicitly.

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

## Performance

corrie uses all cores, reads only the parts of each line a tile needs (lines are indexed in 128-segment pieces), and with packs has no ingest step at all. Highlights, on contours of Madagascar, Kosovo, Montenegro, Réunion and the Comoros (17 files, 506 M vertices, 163,365 tiles at z11–14), Apple M-series laptop, 10 workers:

| Run | Time | Peak RSS |
|---|---|---|
| `tile` from the shapefiles (packing them takes 2.7 s of it) | 10.9 s | 2.7 GB |
| `tile` from packs | **7.9 s** | 2.6 GB |
| `pack` of the 17 shapefiles (8.4 GB → 2.3 GB) | 2.2 s | 0.6 GB |

Both `tile` runs write byte-identical archives. Whole continents are tiled the same way with `--shards`, one country at a time.

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

## Using with MapLibre GL

PMTiles output includes `vector_layers` metadata, so MapLibre can introspect
the source. Serve the file with Martin or another PMTiles server, or read it
straight from static hosting with the
[PMTiles protocol](https://docs.protomaps.com/pmtiles/maplibre). A minimal
style, with the source named `terrain`:

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
