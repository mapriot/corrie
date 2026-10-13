#!/usr/bin/env bash
# Runs the examples of README.md, in order, in a new scratch folder, on two
# neighbouring 1° tiles of the Copernicus GLO-30 DEM in the Alps (47–48°N;
# "west" 12–13°E, "east" 13–14°E). Fails on the first command that fails or
# on a result that is not what the README says.
#
#   examples/readme.sh                       # corrie from PATH
#   CORRIE_IMAGE=ghcr.io/mapriot/corrie examples/readme.sh   # corrie from the Docker image
#
# Needs curl and GDAL's command-line tools (gdal_contour, gdalwarp,
# gdalbuildvrt). The DEM tiles are downloaded once into $DATA.
#
# Copernicus DEM GLO-30: © DLR e.V. 2010-2014 and © Airbus Defence and Space
# GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all
# rights reserved.
set -euo pipefail

DATA=${DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/corrie-examples}
mkdir -p "$DATA"
for tile in N47_00_E012_00 N47_00_E013_00; do
  [ -s "$DATA/$tile.tif" ] || curl -sSfL -o "$DATA/$tile.tif" \
    "https://copernicus-dem-30m.s3.amazonaws.com/Copernicus_DSM_COG_10_${tile}_DEM/Copernicus_DSM_COG_10_${tile}_DEM.tif"
done

work=$(mktemp -d)
cd "$work"
echo "Working in $work"
cp "$DATA/N47_00_E012_00.tif" west.tif
cp "$DATA/N47_00_E013_00.tif" east.tif
cp west.tif dem.tif

if [ -n "${CORRIE_IMAGE:-}" ]; then
  # as in the README's Docker section: this folder mounted at /data
  corrie() { docker run --rm -v "$PWD:/data" "$CORRIE_IMAGE" "$@"; }
fi
run() { echo; echo "\$ $*"; "$@"; }
identical() {
  corrie compare "$1" "$2" > compare.out 2>&1 || true
  tail -1 compare.out | grep -q '^IDENTICAL' || { cat compare.out; echo "FAILED: $1 and $2 differ"; exit 1; }
  echo "   ok: $1 and $2 are identical"
}

echo; echo "### Quick start: from contour lines"
run gdalwarp -t_srs EPSG:4326 dem.tif dem-4326.tif
run gdal_contour -a elevation -i 10 dem-4326.tif contours.fgb
cat > config.json <<'EOF'
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
EOF
run corrie tile config.json contours.pmtiles

echo; echo "### Quick start: many regions, one file"
run mkdir -p contours/west contours/east
run gdal_contour -a elevation -i 10 west.tif contours/west/lines.fgb
run gdal_contour -a elevation -i 10 east.tif contours/east/lines.fgb
run corrie pack contours/west/*.fgb -o contours/west/west.cpk
run corrie pack contours/east/*.fgb -o contours/east/east.cpk
cat > world.json <<'EOF'
{
  "settings": { "minzoom": 11, "maxzoom": 14, "bounding_box": [-180, -85, 180, 85] },
  "layers": {
    "east": { "minzoom": 11, "maxzoom": 14, "source": "contours/east/east.cpk", "write_to": "contour",
              "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2 },
    "west": { "minzoom": 11, "maxzoom": 14, "source": "contours/west/west.cpk", "write_to": "contour",
              "simplify_below": 14, "simplify_level": 0.00005, "simplify_ratio": 2 }
  }
}
EOF
run corrie tile world.json world.pmtiles --shards groups/
# east's lines change: pack it again, tile again, only east is tiled
run corrie pack contours/east/*.fgb -o contours/east/east.cpk --force
run corrie tile world.json world.pmtiles --shards groups/

echo; echo "### corrie pack: moving a config to packs"
sed 's#contours/\([a-z]*\)/\1.cpk#contours/\1/lines.fgb#' world.json > lines.json
run corrie pack --config lines.json --only east
run diff lines.json lines-packs.json || true
run corrie tile lines-packs.json lines-packs.pmtiles --shards groups-packs/
identical world.pmtiles lines-packs.pmtiles

echo; echo "### corrie pack: one pack per group"
run corrie pack --config lines.json --per-group --out-config regions.json
run corrie tile regions.json regions.pmtiles --shards groups-regions/
identical world.pmtiles regions.pmtiles

echo; echo "### corrie unpack"
run corrie unpack border.fgb contours/west/west.cpk contours/east/east.cpk --bbox 12.95,47,13.05,48

echo; echo "### corrie merge"
run corrie merge both.pmtiles groups/west.pmtiles groups/east.pmtiles
identical world.pmtiles both.pmtiles
run cp groups/west.pmtiles atlas.pmtiles
run corrie merge atlas.pmtiles atlas.pmtiles groups/east.pmtiles
identical world.pmtiles atlas.pmtiles

echo; echo "### corrie compare"
run corrie compare world.pmtiles groups/west.pmtiles --show 2 || true
run corrie compare world.pmtiles regions.pmtiles --tolerance 8

echo; echo "### corrie build"
run corrie build dem.tif out.pmtiles
run corrie build dem.tif out.pmtiles --min-z 10 --max-z 15 --levels "10:100,12:50,14:10"
run corrie build dem.tif out.mbtiles --simplify 4.0
run gdalbuildvrt merged.vrt west.tif east.tif
run corrie build merged.vrt out.pmtiles

echo; echo "### Quick start, line by line"
run corrie build dem.tif contours.pmtiles
run corrie tile config.json contours.pmtiles
run corrie tile world.json world.pmtiles --shards groups/
run corrie pack contours/west/*.fgb -o contours/west/west.cpk
run cp world.pmtiles old.pmtiles
run cp both.pmtiles new.pmtiles
run corrie compare old.pmtiles new.pmtiles
run corrie unpack border.gpkg contours/west/west.cpk contours/east/east.cpk

echo; echo "All README examples ran. Files are in $work"
