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
