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
    let mut scratch = ClipScratch::default();
    clip_chain_spans_into(coords, clip_box, &mut scratch);
    scratch.spans.into_iter()
        .map(|(s, e)| coords[s..e].to_vec())
        .collect()
}

/// Per-worker reusable scratch buffers for clip. `spans` holds (start, end)
/// half-open ranges into the source coords slice.
#[derive(Default)]
pub struct ClipScratch {
    pub spans: Vec<(usize, usize)>,
}

/// Single-pass version that emits chains as half-open index spans into the
/// original `coords` slice instead of cloning vertices into Vec<Vec<[f64;2]>>.
/// `scratch.spans` is cleared (length=0, capacity retained) at entry; on
/// return, `scratch.spans` holds the produced chains.
///
/// Semantics preserved from `build_clip_chains`:
/// - Chain vertices are the ORIGINAL source vertices — `coords[start..end]`
///   slices the raw polyline with no clipping applied at the boundary
///   (natural rendering overshoot).
/// - A chain is the maximal run of consecutive segments that have any
///   intersection with clip_box.
/// - Chains with < 2 vertices are dropped (never emitted in `spans`).
pub fn clip_chain_spans_into(
    coords: &[[f64; 2]],
    clip_box: &Box2,
    scratch: &mut ClipScratch,
) {
    scratch.spans.clear();
    if coords.len() < 2 { return; }

    // Fused outcode + chain-finding: hold only the previous vertex's outcode
    // in a register, eliminating the per-vertex Vec<u8> push (bounds-check +
    // length store) and the per-segment indexed outs[i] reads (bounds-check +
    // load) that dominated the prior implementation's CPU time on dense data.
    let mut c_prev = outcode(coords[0][0], coords[0][1], clip_box);
    let mut chain_start: Option<usize> = None;

    for i in 1..coords.len() {
        let c_curr = outcode(coords[i][0], coords[i][1], clip_box);
        let intersects = if c_prev & c_curr != 0 {
            false
        } else if c_prev | c_curr == 0 {
            true
        } else {
            clip_segment_to_box(
                (coords[i - 1][0], coords[i - 1][1]),
                (coords[i][0], coords[i][1]),
                clip_box,
            ).is_some()
        };
        if intersects {
            if chain_start.is_none() { chain_start = Some(i - 1); }
        } else if let Some(s) = chain_start.take() {
            // last intersecting segment was (i-2, i-1); chain = coords[s..i]
            if i - s >= 2 {
                scratch.spans.push((s, i));
            }
        }
        c_prev = c_curr;
    }
    if let Some(s) = chain_start.take() {
        let end = coords.len();
        if end - s >= 2 {
            scratch.spans.push((s, end));
        }
    }
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
