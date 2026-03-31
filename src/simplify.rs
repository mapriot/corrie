/// Douglas-Peucker line simplification. `epsilon` is the maximum allowed
/// perpendicular distance from the simplified line to any omitted point.
pub fn simplify(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let (first, last) = (points[0], points[points.len() - 1]);

    let mut max_dist: f64 = 0.0;
    let mut max_idx = 0;
    for i in 1..points.len() - 1 {
        let d = perp_distance(points[i], first, last);
        if d > max_dist {
            max_dist = d;
            max_idx = i;
        }
    }

    if max_dist > epsilon {
        let mut left = simplify(&points[..=max_idx], epsilon);
        let right = simplify(&points[max_idx..], epsilon);
        left.pop(); // remove duplicate split point
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

/// Perpendicular distance from point `p` to the line through `a` and `b`.
fn perp_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-30 {
        // a and b coincide — return distance to a
        let ex = p.0 - a.0;
        let ey = p.1 - a.1;
        return (ex * ex + ey * ey).sqrt();
    }
    ((p.0 - a.0) * dy - (p.1 - a.1) * dx).abs() / len_sq.sqrt()
}
