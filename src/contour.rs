use std::collections::{HashMap, VecDeque};

/// Edge identifier for segment chaining. Two adjacent cells share the same
/// edge, so segments that meet at a shared edge get the same EdgeId.
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
enum EdgeId {
    /// Horizontal edge at grid row `r`, between columns `c` and `c+1`.
    H(usize, usize),
    /// Vertical edge at grid column `c`, between rows `r` and `r+1`.
    V(usize, usize),
}

struct Segment {
    a: EdgeId,
    b: EdgeId,
    pa: (f64, f64),
    pb: (f64, f64),
}

/// Run marching squares on `grid` (row-major, `width` columns, `height` rows)
/// for a single `threshold`. Returns a list of polylines in grid coordinates
/// (column, row) — i.e. (x, y) where x increases right and y increases down.
///
/// Cells where any corner is nodata are skipped.
pub fn extract_contours(
    grid: &[f32],
    width: usize,
    height: usize,
    nodata: f32,
    threshold: f64,
) -> Vec<Vec<(f64, f64)>> {
    let is_nd = |v: f32| (v - nodata).abs() < 0.5 || v.is_nan();

    let mut segments: Vec<Segment> = Vec::new();

    for i in 0..height - 1 {
        for j in 0..width - 1 {
            let tl = grid[i * width + j];
            let tr = grid[i * width + (j + 1)];
            let bl = grid[(i + 1) * width + j];
            let br = grid[(i + 1) * width + (j + 1)];

            if is_nd(tl) || is_nd(tr) || is_nd(bl) || is_nd(br) {
                continue;
            }

            let tl_d = tl as f64;
            let tr_d = tr as f64;
            let bl_d = bl as f64;
            let br_d = br as f64;

            let case = ((tl_d >= threshold) as u8) << 3
                | ((tr_d >= threshold) as u8) << 2
                | ((br_d >= threshold) as u8) << 1
                | ((bl_d >= threshold) as u8);

            if case == 0 || case == 15 {
                continue;
            }

            // Interpolation helpers — clamp t to [0,1] for safety
            let top = || {
                let t = ((threshold - tl_d) / (tr_d - tl_d)).clamp(0.0, 1.0);
                (EdgeId::H(i, j), (j as f64 + t, i as f64))
            };
            let bottom = || {
                let t = ((threshold - bl_d) / (br_d - bl_d)).clamp(0.0, 1.0);
                (EdgeId::H(i + 1, j), (j as f64 + t, (i + 1) as f64))
            };
            let left = || {
                let t = ((threshold - tl_d) / (bl_d - tl_d)).clamp(0.0, 1.0);
                (EdgeId::V(i, j), (j as f64, i as f64 + t))
            };
            let right = || {
                let t = ((threshold - tr_d) / (br_d - tr_d)).clamp(0.0, 1.0);
                (EdgeId::V(i, j + 1), ((j + 1) as f64, i as f64 + t))
            };

            match case {
                1 | 14 => push_seg(&mut segments, left(), bottom()),
                2 | 13 => push_seg(&mut segments, bottom(), right()),
                3 | 12 => push_seg(&mut segments, left(), right()),
                4 | 11 => push_seg(&mut segments, top(), right()),
                6 | 9 => push_seg(&mut segments, top(), bottom()),
                7 | 8 => push_seg(&mut segments, top(), left()),
                // Saddle cases — disambiguate with centre value
                5 => {
                    let center = (tl_d + tr_d + bl_d + br_d) / 4.0;
                    if center >= threshold {
                        push_seg(&mut segments, top(), left());
                        push_seg(&mut segments, right(), bottom());
                    } else {
                        push_seg(&mut segments, top(), right());
                        push_seg(&mut segments, left(), bottom());
                    }
                }
                10 => {
                    let center = (tl_d + tr_d + bl_d + br_d) / 4.0;
                    if center >= threshold {
                        push_seg(&mut segments, top(), right());
                        push_seg(&mut segments, left(), bottom());
                    } else {
                        push_seg(&mut segments, top(), left());
                        push_seg(&mut segments, right(), bottom());
                    }
                }
                _ => {}
            }
        }
    }

    if segments.is_empty() {
        return vec![];
    }

    chain_segments(segments)
}

#[inline]
fn push_seg(
    segs: &mut Vec<Segment>,
    a: (EdgeId, (f64, f64)),
    b: (EdgeId, (f64, f64)),
) {
    segs.push(Segment {
        a: a.0,
        b: b.0,
        pa: a.1,
        pb: b.1,
    });
}

/// Chain individual segments into polylines by following shared edges.
fn chain_segments(segments: Vec<Segment>) -> Vec<Vec<(f64, f64)>> {
    // Map EdgeId → list of (segment_index, entered_at_b_end)
    let mut adj: HashMap<EdgeId, Vec<(usize, bool)>> = HashMap::new();
    for (idx, seg) in segments.iter().enumerate() {
        adj.entry(seg.a).or_default().push((idx, false));
        adj.entry(seg.b).or_default().push((idx, true));
    }

    let mut visited = vec![false; segments.len()];
    let mut polylines = Vec::new();

    for start_idx in 0..segments.len() {
        if visited[start_idx] {
            continue;
        }
        visited[start_idx] = true;

        let seg = &segments[start_idx];
        let mut deque: VecDeque<(f64, f64)> = VecDeque::new();
        deque.push_back(seg.pa);
        deque.push_back(seg.pb);

        // Extend forward (from the b-end of the starting segment)
        let mut edge = seg.b;
        loop {
            let next = adj
                .get(&edge)
                .and_then(|v| v.iter().find(|&&(idx, _)| !visited[idx]).copied());
            match next {
                Some((idx, is_b)) => {
                    visited[idx] = true;
                    let s = &segments[idx];
                    if is_b {
                        deque.push_back(s.pa);
                        edge = s.a;
                    } else {
                        deque.push_back(s.pb);
                        edge = s.b;
                    }
                }
                None => break,
            }
        }

        // Extend backward (from the a-end of the starting segment)
        edge = seg.a;
        loop {
            let next = adj
                .get(&edge)
                .and_then(|v| v.iter().find(|&&(idx, _)| !visited[idx]).copied());
            match next {
                Some((idx, is_b)) => {
                    visited[idx] = true;
                    let s = &segments[idx];
                    if is_b {
                        deque.push_front(s.pa);
                        edge = s.a;
                    } else {
                        deque.push_front(s.pb);
                        edge = s.b;
                    }
                }
                None => break,
            }
        }

        polylines.push(deque.into_iter().collect());
    }

    polylines
}
