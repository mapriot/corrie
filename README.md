# corrie

Fast DEM-to-contour vector tile generator. Reads any GDAL-supported elevation raster and produces contour lines as tiled vector data in **PMTiles** or **MBTiles** format, ready to serve with [Martin](https://martin.maplibre.org/), [PMTiles server](https://github.com/protomaps/go-pmtiles), or directly in the browser via [MapLibre GL JS](https://maplibre.org/).

## Features

- Reads GeoTIFF, VRT, or any GDAL-supported raster format and CRS
- Outputs PMTiles or MBTiles with MVT (Mapbox Vector Tiles) encoding
- Configurable contour intervals per zoom level
- Index contour tagging (`main`, `show` attributes) for styled rendering
- Douglas-Peucker simplification tunable per zoom
- Parallel tile processing via Rayon

## Installation

Requires GDAL libraries to be installed on your system.

```sh
cargo install --path .
```

## Usage

```sh
corrie <INPUT> <OUTPUT> [OPTIONS]
```

### Arguments

| Argument | Description |
|----------|-------------|
| `INPUT`  | Elevation raster — GeoTIFF, VRT, or any GDAL-supported format |
| `OUTPUT` | Output file — `.pmtiles` or `.mbtiles` (format inferred from extension) |

### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--min-z <ZOOM>` | `11` | Minimum zoom level to generate |
| `--max-z <ZOOM>` | `14` | Maximum zoom level to generate |
| `--levels <PAIRS>` | `11:50,12:20,13:10,14:5` | Zoom:interval pairs (see below) |
| `--main-every <N>` | `50` | Tag contours divisible by N as `main=1` |
| `--show-every <N>` | `100` | Tag contours divisible by N as `show=1` |
| `--layer <NAME>` | `contour` | MVT layer name |
| `--elevation-attr <NAME>` | `elevation` | Attribute name for elevation value |
| `--nodata <VALUE>` | *(from raster)* | Override the nodata value |
| `--simplify <EPS>` | `2.0` | Douglas-Peucker tolerance in MVT units (0 = off) |
| `-j, --workers <N>` | *(all CPUs)* | Worker thread count |

### Examples

Basic usage with a GeoTIFF:

```sh
corrie dem.tif output.pmtiles
```

Custom zoom range and contour intervals:

```sh
corrie dem.tif output.pmtiles --min-z 10 --max-z 15 --levels "10:100,12:50,14:10"
```

Output as MBTiles with more aggressive simplification:

```sh
corrie dem.tif output.mbtiles --simplify 4.0
```

Multi-raster input via VRT:

```sh
gdalbuildvrt merged.vrt tile1.tif tile2.tif tile3.tif
corrie merged.vrt output.pmtiles
```

## How zoom levels and intervals work

The `--levels` flag takes comma-separated `ZOOM:INTERVAL` pairs. Each pair means: draw a contour line every `INTERVAL` metres starting at `ZOOM` and all higher zooms.

With the default `11:50,12:20,13:10,14:5`:

| Zoom | Active intervals | Lines drawn every |
|------|-----------------|-------------------|
| 11   | 50              | 50 m              |
| 12   | 50, 20          | 20 m              |
| 13   | 50, 20, 10      | 10 m              |
| 14   | 50, 20, 10, 5   | 5 m               |

At each zoom, all intervals whose `min_zoom` is less than or equal to the current zoom contribute elevations. The results are unioned, so each elevation appears only once per tile.

## Output attributes

Each contour feature carries:

| Attribute | Type | Description |
|-----------|------|-------------|
| `elevation` | integer | Elevation in metres |
| `main` | integer (1) | Present when elevation is divisible by `--main-every` |
| `show` | integer (1) | Present when elevation is divisible by `--show-every` |

## Using with MapLibre GL JS

The PMTiles output includes a `vector_layers` metadata entry so MapLibre can introspect the source automatically. A minimal style layer:

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

Label index contours by filtering on `main`:

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
