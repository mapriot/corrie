//! corrie — DEM-to-contour vector tile generator and tilemaker SHP→PMTiles replacement.

/// `eprintln!` that never panics. A run can hold hours of work; a closed
/// terminal or tmux pane (EIO) or a reader that went away (EPIPE, e.g. a
/// killed `tee`) must not stop it, so write errors on stderr are ignored.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod attribute_rules;
pub mod build;
pub mod cleanup;
pub mod clip;
pub mod config;
pub mod container;
pub mod contour;
pub mod mercator;
pub mod merge;
pub mod mvt;
pub mod pack;
pub mod raster;
pub mod shapefile_read;
pub mod shards;
pub mod simplify;
pub mod tile;
pub mod tile_gen;
pub mod temp_cache;
