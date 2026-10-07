//! Tile-by-tile comparison of two contour archives.
//!
//! Tiles are matched by tile_id. Tiles whose stored bytes differ are decoded
//! and their lines compared per (elevation, main, show) group, ignoring line
//! order. With a tolerance, lines that differ count as the same when every
//! vertex on either side lies within that many tile pixels of the other
//! side's lines (a symmetric, vertex-based Hausdorff distance), so archives
//! whose coordinates were rounded differently can be checked too.
//!
//! Distances are in tile pixels (the 4096 MVT extent). A tile is drawn 512
//! screen pixels wide, so 8 tile pixels make one screen pixel. Only vertices
//! inside the tile are measured: a line's vertices just past the tile's
//! edge (clipped chains keep one) are cut off when the tile is drawn.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use rayon::prelude::*;

use crate::container::pmtiles::{gunzip, PmtilesReader, TileEntry};
use crate::mvt::{decode_tile, EXTENT};

/// Lines of one tile: per (elevation, main, show), sorted.
type Canon = BTreeMap<(i32, bool, bool), Vec<Vec<(i32, i32)>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// a has every line of b, plus more
    AExtra,
    /// b has every line of a, plus more
    BExtra,
    /// neither contains the other
    Changed,
    /// lines differ, but by at most the tolerance
    WithinTolerance,
    /// a tile only one side has, and its lines stay within the tolerance of
    /// the tile's edge (a sliver of a line moved across the edge)
    EdgeSliver,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TileDiff {
    pub tile_id: u64,
    pub kind: Kind,
    pub points_a: usize,
    pub points_b: usize,
    /// Largest deviation in tile pixels (with a tolerance; capped at
    /// [`DEV_CAP`]), or None when a whole (elevation, main, show) group is
    /// missing on one side.
    pub max_dev: Option<f64>,
}

pub struct Report {
    pub tiles_a: usize,
    pub tiles_b: usize,
    pub common: usize,
    pub only_a: Vec<u64>,
    pub only_b: Vec<u64>,
    /// Tiles in both archives whose lines differ, and (with a tolerance)
    /// edge slivers that only one side has.
    pub diffs: Vec<TileDiff>,
}

impl Report {
    /// Same tiles and lines (within the tolerance, if one was given).
    pub fn matches(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty()
            && self.diffs.iter().all(|d| matches!(d.kind, Kind::WithinTolerance | Kind::EdgeSliver))
    }
}

/// Deviations are searched up to this many pixels; anything further is
/// reported as this value.
pub const DEV_CAP: f64 = 64.0;

pub fn compare(a: &Path, b: &Path, tolerance: Option<f64>) -> Result<Report> {
    let ra = PmtilesReader::open(a)?;
    let rb = PmtilesReader::open(b)?;
    let ea = ra.entries()?;
    let eb = rb.entries()?;

    // Merge-join both entry lists (each sorted by tile_id).
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    let mut both: Vec<(TileEntry, TileEntry)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < ea.len() || j < eb.len() {
        match (ea.get(i), eb.get(j)) {
            (Some(x), Some(y)) if x.tile_id == y.tile_id => { both.push((*x, *y)); i += 1; j += 1; }
            (Some(x), Some(y)) if x.tile_id < y.tile_id => { only_a.push(*x); i += 1; }
            (Some(_), Some(y)) => { only_b.push(*y); j += 1; }
            (Some(x), None) => { only_a.push(*x); i += 1; }
            (None, Some(y)) => { only_b.push(*y); j += 1; }
            (None, None) => unreachable!(),
        }
    }

    let common = both.len();
    let mut diffs: Vec<TileDiff> = both.par_iter()
        .map(|(x, y)| -> Result<Option<TileDiff>> {
            let (bx, by) = (ra.tile_bytes(x), rb.tile_bytes(y));
            if bx == by { return Ok(None); }
            let (ca, cb) = (canon(bx)?, canon(by)?);
            if ca == cb { return Ok(None); }
            let (pa, pb) = (points(&ca), points(&cb));
            if let Some(tol) = tolerance {
                let dev = deviation(&ca, &cb);
                if dev.is_some_and(|d| d <= tol) {
                    return Ok(Some(TileDiff { tile_id: x.tile_id, kind: Kind::WithinTolerance, points_a: pa, points_b: pb, max_dev: dev }));
                }
                let kind = containment(&ca, &cb);
                return Ok(Some(TileDiff { tile_id: x.tile_id, kind, points_a: pa, points_b: pb, max_dev: dev }));
            }
            Ok(Some(TileDiff { tile_id: x.tile_id, kind: containment(&ca, &cb), points_a: pa, points_b: pb, max_dev: None }))
        })
        .filter_map(|r| r.transpose())
        .collect::<Result<_>>()?;

    // With a tolerance, a tile only one side has may be a sliver of a line
    // that moved across the tile's edge.
    let (mut keep_a, mut keep_b) = (Vec::new(), Vec::new());
    for (side, reader, list, keep) in [(0, &ra, &only_a, &mut keep_a), (1, &rb, &only_b, &mut keep_b)] {
        for e in list {
            if let Some(tol) = tolerance {
                let c = canon(reader.tile_bytes(e))?;
                let d = edge_distance(&c);
                if d <= tol {
                    let n = points(&c);
                    let (points_a, points_b) = if side == 0 { (n, 0) } else { (0, n) };
                    diffs.push(TileDiff { tile_id: e.tile_id, kind: Kind::EdgeSliver, points_a, points_b, max_dev: Some(d) });
                    continue;
                }
            }
            keep.push(e.tile_id);
        }
    }
    diffs.sort_by_key(|d| d.tile_id);
    Ok(Report { tiles_a: ea.len(), tiles_b: eb.len(), common, only_a: keep_a, only_b: keep_b, diffs })
}

fn canon(raw: &[u8]) -> Result<Canon> {
    let mut m = Canon::new();
    for f in decode_tile(&gunzip(raw)?, "elevation")? {
        m.entry((f.elevation, f.main, f.show)).or_default().extend(f.lines);
    }
    for lines in m.values_mut() { lines.sort(); }
    Ok(m)
}

fn points(c: &Canon) -> usize { c.values().flatten().map(|l| l.len()).sum() }

/// Multiset inclusion of lines, group by group: is every line of `small` in `big`?
fn contains(big: &Canon, small: &Canon) -> bool {
    small.iter().all(|(k, ls)| {
        let Some(bl) = big.get(k) else { return false };
        let mut bi = bl.iter().peekable();
        ls.iter().all(|l| {
            while bi.peek().is_some_and(|b| *b < l) { bi.next(); }
            bi.next_if(|b| *b == l).is_some()
        })
    })
}

fn containment(a: &Canon, b: &Canon) -> Kind {
    if contains(a, b) { Kind::AExtra } else if contains(b, a) { Kind::BExtra } else { Kind::Changed }
}

/// Largest distance (tile pixels) from any vertex on one side to the other
/// side's lines of the same group, both ways; capped at DEV_CAP. None when
/// a group exists on one side only.
fn deviation(a: &Canon, b: &Canon) -> Option<f64> {
    let mut worst = 0.0f64;
    for key in a.keys().chain(b.keys()) {
        let (la, lb) = (a.get(key)?, b.get(key)?);
        worst = worst.max(directed(la, lb)).max(directed(lb, la));
    }
    Some(worst)
}

/// Largest distance from a vertex of `from` inside the tile to the
/// segments of `to`.
fn directed(from: &[Vec<(i32, i32)>], to: &[Vec<(i32, i32)>]) -> f64 {
    // Segments of `to` in a grid of DEV_CAP-sized cells.
    let cell = DEV_CAP;
    let key = |x: f64, y: f64| ((x / cell).floor() as i64, (y / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<((f64, f64), (f64, f64))>> = HashMap::new();
    for l in to {
        if l.len() == 1 {
            let p = (l[0].0 as f64, l[0].1 as f64);
            grid.entry(key(p.0, p.1)).or_default().push((p, p));
        }
        for w in l.windows(2) {
            let (p, q) = ((w[0].0 as f64, w[0].1 as f64), (w[1].0 as f64, w[1].1 as f64));
            let (k0, k1) = (key(p.0.min(q.0), p.1.min(q.1)), key(p.0.max(q.0), p.1.max(q.1)));
            for gx in k0.0..=k1.0 {
                for gy in k0.1..=k1.1 {
                    grid.entry((gx, gy)).or_default().push((p, q));
                }
            }
        }
    }
    let mut worst = 0.0f64;
    let extent = EXTENT as i32;
    for l in from {
        for &(x, y) in l {
            if x < 0 || y < 0 || x > extent || y > extent { continue; }
            let (px, py) = (x as f64, y as f64);
            let (kx, ky) = key(px, py);
            let mut best = DEV_CAP;
            for gx in kx - 1..=kx + 1 {
                for gy in ky - 1..=ky + 1 {
                    for &(p, q) in grid.get(&(gx, gy)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        best = best.min(segment_distance((px, py), p, q));
                    }
                }
            }
            worst = worst.max(best);
        }
    }
    worst
}

fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// How far a tile's lines reach into it: the largest distance from a vertex
/// inside the tile to the nearest tile edge (vertices outside count 0).
fn edge_distance(c: &Canon) -> f64 {
    let e = EXTENT as f64;
    c.values().flatten().flatten()
        .map(|&(x, y)| {
            let (x, y) = (x as f64, y as f64);
            if x < 0.0 || y < 0.0 || x > e || y > e { 0.0 } else { x.min(y).min(e - x).min(e - y) }
        })
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon_of(groups: &[((i32, bool, bool), Vec<Vec<(i32, i32)>>)]) -> Canon {
        let mut m = Canon::new();
        for (k, ls) in groups {
            m.insert(*k, ls.clone());
        }
        for ls in m.values_mut() { ls.sort(); }
        m
    }

    #[test]
    fn deviation_measures_the_largest_shift() {
        let a = canon_of(&[((100, false, false), vec![vec![(0, 0), (100, 0), (200, 0)]])]);
        // b: same line, middle vertex dropped and the end moved up by 1 px
        let b = canon_of(&[((100, false, false), vec![vec![(0, 0), (200, 1)]])]);
        let d = deviation(&a, &b).unwrap();
        assert!(d > 0.4 && d <= 1.0, "{d}");
        assert_eq!(deviation(&a, &a), Some(0.0));
        // a group on one side only
        let c = canon_of(&[((110, false, false), vec![vec![(0, 0), (1, 1)]])]);
        assert_eq!(deviation(&a, &c), None);
        // far apart: capped
        let far = canon_of(&[((100, false, false), vec![vec![(0, 3000), (200, 3000)]])]);
        assert_eq!(deviation(&a, &far), Some(DEV_CAP));
        // vertices past the tile's edge are not drawn, so they don't count
        let over_a = canon_of(&[((100, false, false), vec![vec![(10, 10), (4000, 10), (4300, -200)]])]);
        let over_b = canon_of(&[((100, false, false), vec![vec![(10, 10), (4000, 10), (4200, 400)]])]);
        assert_eq!(deviation(&over_a, &over_b), Some(0.0));
    }

    #[test]
    fn edge_distance_is_how_far_lines_reach_in() {
        let sliver = canon_of(&[((100, false, false), vec![vec![(-5, 300), (1, 302)]])]);
        assert_eq!(edge_distance(&sliver), 1.0);
        let deep = canon_of(&[((100, false, false), vec![vec![(-5, 300), (400, 302)]])]);
        assert_eq!(edge_distance(&deep), 302.0);
    }
}
