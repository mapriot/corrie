use std::collections::HashMap;
use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;
use prost::Message;

// ── MVT protobuf messages (vector_tile.proto, proto2) ───────────────────────

#[derive(Clone, PartialEq, Message)]
pub struct Tile {
    #[prost(message, repeated, tag = "3")]
    pub layers: Vec<Layer>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Layer {
    /// Must be 2
    #[prost(uint32, tag = "15")]
    pub version: u32,
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, repeated, tag = "2")]
    pub features: Vec<Feature>,
    #[prost(string, repeated, tag = "3")]
    pub keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    pub values: Vec<Value>,
    #[prost(uint32, optional, tag = "5")]
    pub extent: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Feature {
    #[prost(uint64, optional, tag = "1")]
    pub id: Option<u64>,
    #[prost(uint32, repeated, tag = "2")]
    pub tags: Vec<u32>,
    #[prost(enumeration = "GeomType", optional, tag = "3")]
    pub r#type: Option<i32>,
    #[prost(uint32, repeated, tag = "4")]
    pub geometry: Vec<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Value {
    #[prost(string, optional, tag = "1")]
    pub string_value: Option<String>,
    #[prost(float, optional, tag = "2")]
    pub float_value: Option<f32>,
    #[prost(double, optional, tag = "3")]
    pub double_value: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    pub int_value: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    pub uint_value: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    pub sint_value: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    pub bool_value: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum GeomType {
    Unknown = 0,
    Point = 1,
    Linestring = 2,
    Polygon = 3,
}

// ── Public encoding API ─────────────────────────────────────────────────────

pub const EXTENT: u32 = 4096;

/// A contour ready for MVT encoding. May hold multiple sub-linestrings
/// that share the same attributes (encoded as one MultiLineString feature).
pub struct ContourFeature {
    /// One or more sub-linestrings in MVT coordinates (0..EXTENT, y-down).
    /// Each sub-linestring must have at least 2 points.
    pub lines: Vec<Vec<(i32, i32)>>,
    /// Elevation in whole metres.
    pub elevation: i32,
    /// True if this is a "main" contour (e.g. every 50 m).
    pub main: bool,
    /// True if this contour should carry the "show" hint.
    pub show: bool,
}

/// Encode a set of contour features into a gzip-compressed MVT tile.
pub fn encode_tile(
    features: &[ContourFeature],
    layer_name: &str,
    elevation_attr: &str,
) -> Vec<u8> {
    // Build shared key/value tables
    let mut key_idx: HashMap<&str, u32> = HashMap::new();
    let mut keys: Vec<String> = Vec::new();
    let mut val_idx: HashMap<i64, u32> = HashMap::new();
    let mut values: Vec<Value> = Vec::new();

    let ensure_key = |k: &str, keys: &mut Vec<String>, map: &mut HashMap<&str, u32>| -> u32 {
        if let Some(&i) = map.get(k) {
            return i;
        }
        let i = keys.len() as u32;
        keys.push(k.to_owned());
        // SAFETY: we only look up keys that are string literals living for 'static or
        // the duration of this function, so we extend the borrow manually.
        map.insert(unsafe { &*(k as *const str) }, i);
        i
    };

    let ensure_int_val = |v: i64, vals: &mut Vec<Value>, map: &mut HashMap<i64, u32>| -> u32 {
        if let Some(&i) = map.get(&v) {
            return i;
        }
        let i = vals.len() as u32;
        vals.push(Value {
            int_value: Some(v),
            ..Default::default()
        });
        map.insert(v, i);
        i
    };

    let elev_key = ensure_key(elevation_attr, &mut keys, &mut key_idx);

    // Pre-register "main" and "show" keys only if any feature uses them.
    let has_main = features.iter().any(|f| f.main);
    let has_show = features.iter().any(|f| f.show);
    let main_key = if has_main {
        Some(ensure_key("main", &mut keys, &mut key_idx))
    } else {
        None
    };
    let show_key = if has_show {
        Some(ensure_key("show", &mut keys, &mut key_idx))
    } else {
        None
    };

    let mut mvt_features: Vec<Feature> = Vec::with_capacity(features.len());

    for (id, cf) in features.iter().enumerate() {
        if cf.lines.is_empty() {
            continue;
        }

        let mut tags = Vec::new();
        // elevation
        let vi = ensure_int_val(cf.elevation as i64, &mut values, &mut val_idx);
        tags.push(elev_key);
        tags.push(vi);
        // main
        if cf.main {
            if let Some(mk) = main_key {
                let vi = ensure_int_val(1, &mut values, &mut val_idx);
                tags.push(mk);
                tags.push(vi);
            }
        }
        // show
        if cf.show {
            if let Some(sk) = show_key {
                let vi = ensure_int_val(1, &mut values, &mut val_idx);
                tags.push(sk);
                tags.push(vi);
            }
        }

        let geometry = encode_multilinestring(&cf.lines);

        mvt_features.push(Feature {
            id: Some((id + 1) as u64),
            tags,
            r#type: Some(GeomType::Linestring as i32),
            geometry,
        });
    }

    let tile = Tile {
        layers: vec![Layer {
            version: 2,
            name: layer_name.to_owned(),
            features: mvt_features,
            keys,
            values,
            extent: Some(EXTENT),
        }],
    };

    // Serialize to protobuf (raw, no compression — container handles that)
    let mut buf = Vec::new();
    tile.encode(&mut buf).expect("MVT encode");
    buf
}

/// Gzip-compress a buffer. Used by MBTiles (PMTiles handles compression itself).
pub fn gzip_compress(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data).expect("gzip write");
    enc.finish().expect("gzip finish")
}

/// Encode one or more LineStrings into MVT geometry commands. Multiple
/// linestrings share one feature (MultiLineString) by repeating MoveTo+LineTo;
/// the cursor state carries over between sub-lines.
fn encode_multilinestring(lines: &[Vec<(i32, i32)>]) -> Vec<u32> {
    let total_pts: usize = lines.iter().map(|l| l.len()).sum();
    let mut cmds = Vec::with_capacity(lines.len() * 3 + total_pts * 2);
    let (mut cx, mut cy) = (0i32, 0i32);

    for points in lines {
        if points.len() < 2 { continue; }

        // MoveTo(1)
        cmds.push(command(1, 1));
        let dx = points[0].0 - cx;
        let dy = points[0].1 - cy;
        cmds.push(zigzag(dx));
        cmds.push(zigzag(dy));
        cx = points[0].0;
        cy = points[0].1;

        // LineTo(n-1)
        let n = points.len() - 1;
        cmds.push(command(2, n as u32));
        for &(x, y) in &points[1..] {
            let dx = x - cx;
            let dy = y - cy;
            cmds.push(zigzag(dx));
            cmds.push(zigzag(dy));
            cx = x;
            cy = y;
        }
    }

    cmds
}

#[inline]
fn command(id: u32, count: u32) -> u32 {
    (id & 0x7) | (count << 3)
}

#[inline]
fn zigzag(n: i32) -> u32 {
    ((n << 1) ^ (n >> 31)) as u32
}
