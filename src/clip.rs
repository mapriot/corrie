//! Cohen-Sutherland line clipping. Operates on `f64` coordinates.

#[derive(Debug, Clone, Copy)]
pub struct Box2 {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

const INSIDE: u8 = 0;
const LEFT:   u8 = 1;
const RIGHT:  u8 = 2;
const BOTTOM: u8 = 4;
const TOP:    u8 = 8;

#[inline]
fn outcode(x: f64, y: f64, b: &Box2) -> u8 {
    let mut c = INSIDE;
    if x < b.min_x { c |= LEFT; }
    else if x > b.max_x { c |= RIGHT; }
    if y < b.min_y { c |= BOTTOM; }
    else if y > b.max_y { c |= TOP; }
    c
}

/// Clip one segment to `b`. Returns the clipped endpoints, or None if the
/// segment is entirely outside.
pub fn clip_segment_to_box(
    a: (f64, f64),
    b: (f64, f64),
    box_: &Box2,
) -> Option<((f64, f64), (f64, f64))> {
    let (mut x0, mut y0) = a;
    let (mut x1, mut y1) = b;
    let mut c0 = outcode(x0, y0, box_);
    let mut c1 = outcode(x1, y1, box_);
    loop {
        if c0 | c1 == INSIDE { return Some(((x0, y0), (x1, y1))); }
        if c0 & c1 != INSIDE { return None; }
        let cout = if c0 != INSIDE { c0 } else { c1 };
        let (x, y);
        if cout & TOP != 0 {
            x = x0 + (x1 - x0) * (box_.max_y - y0) / (y1 - y0);
            y = box_.max_y;
        } else if cout & BOTTOM != 0 {
            x = x0 + (x1 - x0) * (box_.min_y - y0) / (y1 - y0);
            y = box_.min_y;
        } else if cout & RIGHT != 0 {
            y = y0 + (y1 - y0) * (box_.max_x - x0) / (x1 - x0);
            x = box_.max_x;
        } else { // LEFT
            y = y0 + (y1 - y0) * (box_.min_x - x0) / (x1 - x0);
            x = box_.min_x;
        }
        if cout == c0 {
            x0 = x; y0 = y; c0 = outcode(x0, y0, box_);
        } else {
            x1 = x; y1 = y; c1 = outcode(x1, y1, box_);
        }
    }
}

/// Split a polyline into chains based on whether each consecutive segment
/// intersects `clip_box`. Each chain is a maximal sequence of consecutive
/// segments that have any intersection with clip_box. Chain vertices are the
/// ORIGINAL source vertices (no clipping applied), so chains may extend
/// slightly past the clip box at their endpoints — providing natural rendering
/// overshoot. Chains with < 2 vertices are dropped.
pub fn build_clip_chains(coords: &[[f64; 2]], clip_box: &Box2) -> Vec<Vec<[f64; 2]>> {
    if coords.len() < 2 { return Vec::new(); }
    // Precompute outcodes once per vertex — avoids redundant outcode
    // evaluation that the windows(2)-based version did at every segment boundary.
    let mut outs: Vec<u8> = Vec::with_capacity(coords.len());
    for v in coords {
        outs.push(outcode(v[0], v[1], clip_box));
    }
    let mut chains: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    for i in 0..coords.len() - 1 {
        let c0 = outs[i];
        let c1 = outs[i + 1];
        let intersects = if c0 & c1 != 0 {
            false  // trivial reject: both endpoints share an outside edge
        } else if c0 | c1 == 0 {
            true   // trivial accept: both endpoints inside
        } else {
            // Borderline — need actual segment test (we only care yes/no).
            clip_segment_to_box(
                (coords[i][0], coords[i][1]),
                (coords[i + 1][0], coords[i + 1][1]),
                clip_box,
            ).is_some()
        };
        if intersects {
            if current.is_empty() {
                current.push(coords[i]);
            }
            current.push(coords[i + 1]);
        } else if current.len() >= 2 {
            chains.push(std::mem::take(&mut current));
            current.clear();
        } else {
            current.clear();
        }
    }
    if current.len() >= 2 { chains.push(current); }
    chains
}

/// Clip a polyline to `b`, returning zero or more sub-polylines.
/// Maintains line continuity: consecutive segments that share an inside endpoint
/// keep that vertex in one sub-polyline.
pub fn clip_polyline_to_box(coords: &[[f64; 2]], box_: &Box2) -> Vec<Vec<[f64; 2]>> {
    if coords.len() < 2 { return Vec::new(); }
    let mut subs: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();

    let push_pt = |v: &mut Vec<[f64; 2]>, p: [f64; 2]| {
        if let Some(&last) = v.last() {
            if last == p { return; }
        }
        v.push(p);
    };

    for w in coords.windows(2) {
        let (a, b) = ((w[0][0], w[0][1]), (w[1][0], w[1][1]));
        match clip_segment_to_box(a, b, box_) {
            None => {
                if !current.is_empty() {
                    if current.len() >= 2 { subs.push(std::mem::take(&mut current)); }
                    current.clear();
                }
            }
            Some((ca, cb)) => {
                let ca_equals_a = (ca.0 - a.0).abs() < 1e-12 && (ca.1 - a.1).abs() < 1e-12;
                if !ca_equals_a {
                    // Segment was clipped at its start — previous sub ends here.
                    if current.len() >= 2 { subs.push(std::mem::take(&mut current)); }
                    current.clear();
                    push_pt(&mut current, [ca.0, ca.1]);
                } else if current.is_empty() {
                    push_pt(&mut current, [ca.0, ca.1]);
                }
                push_pt(&mut current, [cb.0, cb.1]);
                let cb_equals_b = (cb.0 - b.0).abs() < 1e-12 && (cb.1 - b.1).abs() < 1e-12;
                if !cb_equals_b {
                    // Segment was clipped at its end — close out this sub.
                    if current.len() >= 2 { subs.push(std::mem::take(&mut current)); }
                    current.clear();
                }
            }
        }
    }
    if current.len() >= 2 { subs.push(current); }
    subs
}
