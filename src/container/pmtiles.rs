use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use pmtiles::{PmTilesStreamWriter, PmTilesWriter, TileCoord, TileType};

pub struct PmtilesWriter {
    inner: PmTilesStreamWriter<File>,
}

impl PmtilesWriter {
    pub fn create(
        path: &Path,
        min_z: u8,
        max_z: u8,
        bounds: (f64, f64, f64, f64),
        layer_name: &str,
        elevation_attr: &str,
    ) -> Result<Self> {
        let (west, south, east, north) = bounds;
        let center_lon = (west + east) / 2.0;
        let center_lat = (south + north) / 2.0;
        let center_zoom = ((min_z + max_z) / 2).max(min_z);

        // TileJSON 3.0 vector_layers — required by MapLibre for layer introspection
        let metadata = format!(
            r#"{{"vector_layers":[{{"id":"{layer_name}","fields":{{"{elevation_attr}":"Number","main":"Number","show":"Number"}},"minzoom":{min_z},"maxzoom":{max_z}}}]}}"#
        );

        let f = File::create(path).with_context(|| format!("create {:?}", path))?;
        let inner = PmTilesWriter::new(TileType::Mvt)
            .min_zoom(min_z)
            .max_zoom(max_z)
            .bounds(west, south, east, north)
            .center(center_lon, center_lat)
            .center_zoom(center_zoom)
            .metadata(&metadata)
            .create(f)
            .context("create PMTiles writer")?;
        Ok(Self { inner })
    }

    pub fn add_tile(&mut self, z: u8, x: u32, y_xyz: u32, data: &[u8]) -> Result<()> {
        let coord = TileCoord::new(z, x, y_xyz).context("TileCoord")?;
        self.inner.add_tile(coord, data).context("add_tile")
    }

    pub fn finalize(self) -> Result<()> {
        self.inner.finalize().context("finalize PMTiles")
    }
}
