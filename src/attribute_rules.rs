//! Per-feature attribute derivation. Matches mapriot's process-contours.lua exactly.
//!
//! Output values per contour line:
//!   `minzoom` — the lowest zoom at which this contour should appear.
//!   `flags`   — bit 0 = `main` (index contour, every 50 m),
//!               bit 1 = `show` (label-eligible contour, every 100 m).

pub const MAIN_FLAG: u8 = 0b01;
pub const SHOW_FLAG: u8 = 0b10;

/// Derive (minzoom, flags) from an integer elevation in metres.
///
/// Matches `process-contours.lua` exactly:
///   - elev % 100 == 0 → minzoom=11, main=1, show=1
///   - elev % 50  == 0 → minzoom=11, main=1
///   - elev % 20  == 0 → minzoom=12
///   - elev % 10  == 0 → minzoom=13
///   - else            → minzoom=14
pub fn derive_attributes(elevation: i32) -> (u8, u8) {
    let mut flags = 0u8;
    let minzoom = if elevation.rem_euclid(100) == 0 {
        flags |= MAIN_FLAG;
        flags |= SHOW_FLAG;
        11
    } else if elevation.rem_euclid(50) == 0 {
        flags |= MAIN_FLAG;
        11
    } else if elevation.rem_euclid(20) == 0 {
        12
    } else if elevation.rem_euclid(10) == 0 {
        13
    } else {
        14
    };
    (minzoom, flags)
}
