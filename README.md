# corrie

Fast contour vector-tile builder. Two modes:

- **`corrie tile`** — read a tilemaker-format `config.json` plus a set of
  contour shapefiles, write a clustered **PMTiles** file. Drop-in replacement
  for the `tilemaker → mbtiles → pmtiles convert` pipeline for contour-only
  configs.
- **`corrie build`** — read a single elevation raster (any GDAL-supported
  format/CRS), run marching-squares contour extraction, and write tiles as
  PMTiles or MBTiles. No external pipeline required.

Output is ready to serve with [Martin](https://martin.maplibre.org/),
[go-pmtiles](https://github.com/protomaps/go-pmtiles), or directly to a
browser via [MapLibre GL JS](https://maplibre.org/).

## Install

```sh
cargo install --path .
```

The `build` subcommand needs GDAL libraries on your system. The `tile`
subcommand does not — it only reads shapefiles via the pure-Rust
`shapefile` crate.

---

## `corrie tile` — shapefiles to clustered PMTiles

The primary use case. Given a tilemaker config that lists `.shp` files as
sources (one or more layers, all using `write_to` to merge into a single
output layer is the common shape for contour data), `corrie tile` produces
a single clustered PMTiles file with the same layer schema.

```sh
corrie tile <CONFIG.json> <OUTPUT.pmtiles> [OPTIONS]
```

### Options

| Option | Description |
|---|---|
| `--cache-dir <PATH>` | Persist the ingest cache to `<PATH>` so subsequent runs with the same SHP set skip the parse pass. Without this flag the cache is written to a temp dir and **deleted at the end of the run** (no disk cost). |
| `--rebuild-cache` | Force re-ingest even when the cache hash matches. Only meaningful together with `--cache-dir`. |
| `-j, --workers <N>` | Worker thread count. Default: all available cores. |

### How the cache works

`corrie tile` runs in two phases: ingest (parse SHPs → packed binary
feature store + R-tree) and tile generation (per-tile spatial query → clip
→ simplify → MVT encode → PMTiles writer).

- **Default mode** (no `--cache-dir`): ingest writes to `tempfile::TempDir`,
  dropped when the process exits. No persistent state. Recommended for the
  typical workflow where you re-run after adding new SHPs (which invalidates
  the cache anyway).
- **`--cache-dir <path>`**: ingest writes `coords.bin`, `features.bin`,
  `rtree.bin`, `manifest.bin` to that directory. On subsequent runs with the
  same SHP file set (same paths, sizes, mtimes), ingest is skipped entirely
  and only the tile-generation phase runs.

Cache is invalidated if any SHP file's size or mtime changes, if a file is
added or removed from the config, or if you pass `--rebuild-cache`.

### Example

```sh
# Default: no cache persisted, ingest each run
corrie tile config-contours.json contours.pmtiles

# Persistent cache: first run does the ingest (slow), subsequent runs are warm
corrie tile config-contours.json contours.pmtiles --cache-dir /var/cache/corrie
```

### Supported `config.json` fields

`corrie tile` parses the subset of the tilemaker config relevant for the
SHP/contour path. Unknown fields (OSM keys, Lua hooks, etc.) are silently
ignored.

**Top-level `settings`:**

| Field | Notes |
|---|---|
| `minzoom`, `maxzoom` | Required. Inclusive range of zoom levels to write. |
| `basezoom` | Optional. Not used for tile output (informational). |
| `include_ids` | Optional. If false, MVT feature IDs are omitted. |
| `bounding_box` | Required. `[minLon, minLat, maxLon, maxLat]`. Tile enumeration spans this box; features outside it are ignored. |
| `compress` | Optional. Only `"gzip"` is supported (PMTiles default). |
| `name`, `version`, `description` | Written to PMTiles metadata. |

**Per layer in `layers`:**

| Field | Notes |
|---|---|
| `minzoom`, `maxzoom` | Required. Layer-level zoom gate (in addition to per-feature minzoom). |
| `source` | Required. Path to a `.shp` file. Relative paths resolve against the config file's directory. |
| `source_columns` | Optional. The first entry is used as the DBF column name for elevation (default `"elevation"`). |
| `write_to` | Optional. Aliases this layer's output into the named target layer. All layers writing to the same target are merged into one MVT layer. |
| `simplify_below` | Optional. Default 0. Douglas-Peucker simplification runs when `z < simplify_below`. |
| `simplify_level` | Optional. Default 0.01. Base DP epsilon in degrees of longitude/latp. |
| `simplify_length` | Optional. Default 0. Alternative base epsilon in **metres** (matches tilemaker's code). Takes precedence over `simplify_level` when set. |
| `simplify_ratio` | Optional. Default 2.0. Per-zoom scaling factor: epsilon at zoom `z` is `base * simplify_ratio^(simplify_below - 1 - z)`. |
| `simplify_algorithm` | Optional. Only Douglas-Peucker (default) is implemented; passing `"visvalingam"` errors out. |

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

One layer per `write_to` group (typically just `contour`). Each feature has:

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

## Performance and memory

Both subcommands stream features through the pipeline rather than loading
everything into RAM. For `corrie tile`, peak memory floors at the R-tree
input vec (≈24 bytes per source feature) plus a bounded channel between
SHP readers and the on-disk feature store writer. Memory is therefore
bounded by **feature count**, not vertex count, and stays at well under
1 GB even for whole-planet contour input.

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
