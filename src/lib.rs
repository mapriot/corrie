//! corrie — DEM-to-contour vector tile generator and tilemaker SHP→PMTiles replacement.

pub mod attribute_rules;
pub mod clip;
pub mod config;
pub mod container;
pub mod contour;
pub mod feature_store;
pub mod mercator;
pub mod mvt;
pub mod raster;
pub mod shapefile_read;
pub mod simplify;
pub mod spatial_index;
pub mod tile;
pub mod tile_gen;
pub mod ingest;
pub mod temp_cache;
