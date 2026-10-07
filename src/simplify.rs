/// Douglas-Peucker line simplification. `epsilon` is the maximum allowed
/// distance from the simplified line to any omitted point.
///
/// Distances are measured to the chord as a segment, not to the infinite
/// line through its ends. With the line, a spike that runs out and back
/// (both ends of the chord nearly at the same point) gets an arbitrary
/// direction and distance, so its tip was kept or dropped by sub-centimetre
/// differences in the input.
///
/// Iterative stack-based implementation. Output is APPENDED to `out` (caller
/// owns capacity; clear if you want a fresh result). `work_stack` and `kept`
/// are reusable scratch — they are cleared at entry and resized to fit the
/// input on every call. Capacity is retained for the next call.
///
/// Tie-breaking: when two vertices share the maximum perpendicular distance,
/// the lower-indexed one is selected (matches the recursive `>` not `>=`).
pub fn simplify_into(
    points: &[(f64, f64)],
    epsilon: f64,
    work_stack: &mut Vec<(usize, usize)>,
    kept: &mut Vec<bool>,
    out: &mut Vec<(f64, f64)>,
) {
    let n = points.len();
    if n <= 2 {
        out.extend_from_slice(points);
        return;
    }

    kept.clear();
    kept.resize(n, false);
    kept[0] = true;
    kept[n - 1] = true;

    work_stack.clear();
    work_stack.push((0, n - 1));

    let eps_sq = epsilon * epsilon;

    while let Some((lo, hi)) = work_stack.pop() {
        if hi - lo < 2 { continue; }
        let (a, b) = (points[lo], points[hi]);
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len_sq = dx * dx + dy * dy;

        let mut max_dist_sq: f64 = -1.0;
        let mut max_idx: usize = 0;

        if len_sq < 1e-30 {
            // a and b coincide; squared distance from each interior point to a.
            for i in (lo + 1)..hi {
                let ex = points[i].0 - a.0;
                let ey = points[i].1 - a.1;
                let d_sq = ex * ex + ey * ey;
                if d_sq > max_dist_sq {
                    max_dist_sq = d_sq;
                    max_idx = i;
                }
            }
        } else {
            let inv_len_sq = 1.0 / len_sq;
            for i in (lo + 1)..hi {
                let (ex, ey) = (points[i].0 - a.0, points[i].1 - a.1);
                // position along the chord, clamped to its ends
                let t = ((ex * dx + ey * dy) * inv_len_sq).clamp(0.0, 1.0);
                let (fx, fy) = (ex - t * dx, ey - t * dy);
                let d_sq = fx * fx + fy * fy;
                if d_sq > max_dist_sq {
                    max_dist_sq = d_sq;
                    max_idx = i;
                }
            }
        }

        if max_dist_sq > eps_sq {
            kept[max_idx] = true;
            // Push hi-side first so lo-side is processed first (matches the
            // recursive left-then-right order, important for tie behaviour).
            work_stack.push((max_idx, hi));
            work_stack.push((lo, max_idx));
        }
    }

    for i in 0..n {
        if kept[i] { out.push(points[i]); }
    }
}

/// Backwards-compatible wrapper. Allocates a fresh output Vec. Internal hot
/// paths should call `simplify_into` directly with reusable scratch buffers.
pub fn simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    let mut work_stack = Vec::new();
    let mut kept = Vec::new();
    let mut out = Vec::with_capacity(points.len());
    simplify_into(points, epsilon, &mut work_stack, &mut kept, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recursive reference impl — used only for property testing equivalence.
    fn simplify_recursive_reference(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
        if points.len() <= 2 { return points.to_vec(); }
        let (first, last) = (points[0], points[points.len() - 1]);
        let mut max_dist: f64 = 0.0;
        let mut max_idx = 0;
        for i in 1..points.len() - 1 {
            let d = perp_distance_ref(points[i], first, last);
            if d > max_dist { max_dist = d; max_idx = i; }
        }
        if max_dist > epsilon {
            let mut left = simplify_recursive_reference(&points[..=max_idx], epsilon);
            let right = simplify_recursive_reference(&points[max_idx..], epsilon);
            left.pop();
            left.extend(right);
            left
        } else {
            vec![first, last]
        }
    }

    fn perp_distance_ref(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len_sq = dx * dx + dy * dy;
        if len_sq < 1e-30 {
            let ex = p.0 - a.0;
            let ey = p.1 - a.1;
            return (ex * ex + ey * ey).sqrt();
        }
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0);
        ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
    }

    /// A spike running out past the end of the chord and back keeps its tip:
    /// its distance to the chord segment is large even though it lies on the
    /// chord's line.
    #[test]
    fn keeps_a_spike_along_the_chord() {
        let pts = [(0.0, 0.0), (10.0, 0.0), (30.0, 0.05), (10.2, 0.1), (20.0, 0.0)];
        let out = simplify(&pts, 1.0);
        assert!(out.contains(&(30.0, 0.05)), "{out:?}");
    }

    #[test]
    fn empty_input_yields_empty() {
        let mut out = Vec::new();
        simplify_into(&[], 0.1, &mut Vec::new(), &mut Vec::new(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn two_points_preserved() {
        let pts = vec![(0.0, 0.0), (1.0, 1.0)];
        assert_eq!(simplify(&pts, 0.1), pts);
    }

    #[test]
    fn collinear_three_collapsed() {
        let pts = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)];
        let out = simplify(&pts, 0.1);
        assert_eq!(out, vec![(0.0, 0.0), (2.0, 0.0)]);
    }

    #[test]
    fn vee_three_kept() {
        let pts = vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.0)];
        let out = simplify(&pts, 0.5);
        assert_eq!(out, pts);
    }

    #[test]
    fn matches_recursive_on_random_inputs() {
        let mut seed: u64 = 0xc0ffeebabe;
        fn rng(s: &mut u64) -> f64 {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            (*s & ((1u64 << 53) - 1)) as f64 / (1u64 << 53) as f64
        }
        for trial in 0..200 {
            let n = 2 + (trial % 40);
            let pts: Vec<(f64, f64)> = (0..n).map(|_| (rng(&mut seed) * 10.0, rng(&mut seed) * 10.0)).collect();
            let eps = rng(&mut seed) * 0.5;
            let iterative = simplify(&pts, eps);
            let recursive = simplify_recursive_reference(&pts, eps);
            assert_eq!(iterative, recursive,
                "trial {trial} mismatch (n={n}, eps={eps}): pts={:?}", pts);
        }
    }
}
