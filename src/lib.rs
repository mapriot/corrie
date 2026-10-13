//! corrie — contour vector tiles: contour lines (packs, SHPs, any GDAL vector
//! file) listed in a tilemaker config, or a DEM, to clustered PMTiles.

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

/// `n` and the word, plural unless `n` is 1: "1 source", "3 sources".
pub fn count(n: impl Into<u64>, word: &str) -> String {
    let n = n.into();
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

pub mod attribute_rules;
pub mod build;
pub mod cleanup;
pub mod clip;
pub mod compare;
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
pub mod unpack;
pub mod temp_cache;
