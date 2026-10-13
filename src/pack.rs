//! Contour packs (`.cpk`): one source's lines in a compact file that
//! `corrie tile` reads in place.
//!
//! Vertices are stored on a 2^32 grid over Web Mercator's `(lon, latp)`
//! square (both axes span 360°), so a step is 8.4e-8° — under 1 cm on the
//! ground — and a decoded vertex lies within half a step of the original.
//! Each line is cut into chunks of [`CHUNK_SEGMENTS`] segments (consecutive
//! chunks share one vertex). A chunk is stored as its first vertex followed
//! by zigzag varint deltas, so it decodes on its own. The chunk table is in
//! Hilbert order of the chunks' centres and doubles as the leaf level of a
//! packed R-tree (16 children per node) stored after it, so a pack needs no
//! index build: open it and query.
//!
//! Layout (little-endian, every section 8-byte aligned):
//!
//! ```text
//! Header (120 B) | data (varints) | lines: [LineRec] | chunks: [ChunkRec] | nodes: [[u32; 4]] | meta (JSON)
//! ```

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use bytemuck::{Pod, Zeroable};
use memmap2::Mmap;

pub const MAGIC: [u8; 8] = *b"CORRIEPK";
pub const VERSION: u32 = 1;
/// Segments per chunk; a chunk holds up to CHUNK_SEGMENTS + 1 vertices.
pub const CHUNK_SEGMENTS: usize = 128;
/// Children per packed R-tree node.
const NODE_SIZE: usize = 16;
const GRID: f64 = 4_294_967_296.0; // 2^32 steps over 360°

/// A bounding box in grid units: `[x_min, y_min, x_max, y_max]`, inclusive.
pub type GridBox = [u32; 4];

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct Header {
    pub magic: [u8; 8],
    pub version: u32,
    pub chunk_segments: u32,
    pub n_lines: u64,
    pub n_chunks: u64,
    pub n_vertices: u64,
    pub data_off: u64,
    pub data_len: u64,
    pub lines_off: u64,
    pub chunks_off: u64,
    pub nodes_off: u64,
    pub n_nodes: u64,
    pub meta_off: u64,
    pub meta_len: u64,
    pub bbox: GridBox,
}
const HEADER_SIZE: usize = std::mem::size_of::<Header>();
const _: () = assert!(HEADER_SIZE == 120);

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct LineRec {
    pub bbox: GridBox,
    pub n_vertices: u32,
    pub elevation: i16,
    pub _pad: u16,
}
const _: () = assert!(std::mem::size_of::<LineRec>() == 24);

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct ChunkRec {
    pub bbox: GridBox,
    /// Byte offset of the chunk's encoding within the data section.
    pub data_off: u64,
    pub line: u32,
    /// Index of the chunk's first vertex within its line.
    pub first_vertex: u32,
}
const _: () = assert!(std::mem::size_of::<ChunkRec>() == 32);

// ---------------- coordinates ----------------

/// Grid cell of a longitude or Mercator latitude (`latp`), in degrees.
#[inline]
pub fn to_grid(v: f64) -> u32 {
    ((v + 180.0) / 360.0 * GRID).floor().clamp(0.0, GRID - 1.0) as u32
}

/// Centre of a grid cell, in degrees.
#[inline]
pub fn from_grid(g: u32) -> f64 {
    (g as f64 + 0.5) / GRID * 360.0 - 180.0
}

/// Smallest grid box holding every cell that intersects `[lo, hi]` (degrees),
/// widened by one cell so edge cases never drop a chunk.
#[inline]
pub fn grid_range(lo: f64, hi: f64) -> (u32, u32) {
    (to_grid(lo).saturating_sub(1), to_grid(hi).saturating_add(1))
}

#[inline]
pub fn boxes_intersect(a: &GridBox, b: &GridBox) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

// ---------------- varints ----------------

#[inline]
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn get_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 { return v; }
        shift += 7;
    }
}

#[inline]
fn zigzag(d: i64) -> u64 { ((d << 1) ^ (d >> 63)) as u64 }
#[inline]
fn unzigzag(u: u64) -> i64 { ((u >> 1) as i64) ^ -((u & 1) as i64) }

// ---------------- Hilbert order ----------------

/// Hilbert index of (x, y) on a 2^16 × 2^16 grid.
fn hilbert16(mut x: u32, mut y: u32) -> u64 {
    let mut d: u64 = 0;
    let mut s: u32 = 1 << 15;
    while s > 0 {
        let rx = (x & s > 0) as u32;
        let ry = (y & s > 0) as u32;
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x) & 0xffff;
                y = s.wrapping_sub(1).wrapping_sub(y) & 0xffff;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    d
}

fn hilbert_key(b: &GridBox) -> u64 {
    let cx = ((b[0] as u64 + b[2] as u64) / 2) >> 16;
    let cy = ((b[1] as u64 + b[3] as u64) / 2) >> 16;
    hilbert16(cx as u32, cy as u32)
}

fn union(a: &mut GridBox, b: &GridBox) {
    a[0] = a[0].min(b[0]);
    a[1] = a[1].min(b[1]);
    a[2] = a[2].max(b[2]);
    a[3] = a[3].max(b[3]);
}

const EMPTY_BOX: GridBox = [u32::MAX, u32::MAX, 0, 0];

/// Level sizes of the packed tree above `n` leaves: level 1 first, root last.
fn level_sizes(n: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut m = n;
    while m > 1 {
        m = m.div_ceil(NODE_SIZE);
        sizes.push(m);
    }
    sizes
}

// ---------------- writer ----------------

pub struct PackStats {
    pub lines: u64,
    pub vertices: u64,
    pub bytes: u64,
    /// Source features left out as degree-seam lines (`--drop-seams`).
    pub seam_features: u64,
}

/// How sources are packed.
#[derive(Debug, Clone, Copy, Default)]
pub struct PackOptions {
    /// Leave out features whose every vertex lies within this many degrees of
    /// a whole-degree meridian or parallel (see
    /// [`crate::shapefile_read::on_degree_seams`]).
    pub drop_seams: Option<f64>,
}

/// Streams lines into a pack. Data is written as lines arrive; the line and
/// chunk tables are kept in memory (56 B per line + 32 B per chunk) and
/// written, sorted and indexed, by [`PackWriter::finish`].
pub struct PackWriter {
    file: BufWriter<File>,
    lines: Vec<LineRec>,
    chunks: Vec<ChunkRec>,
    data_len: u64,
    n_vertices: u64,
    scratch: Vec<u8>,
    grid: Vec<[u32; 2]>,
}

impl PackWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let f = File::create(path).with_context(|| format!("create {:?}", path))?;
        let mut file = BufWriter::with_capacity(1 << 20, f);
        file.write_all(&[0u8; HEADER_SIZE])?;
        Ok(Self { file, lines: Vec::new(), chunks: Vec::new(), data_len: 0, n_vertices: 0, scratch: Vec::new(), grid: Vec::new() })
    }

    /// Add a polyline given as `(lon, latp)` degrees. Consecutive vertices in
    /// the same grid cell are merged; a line left with < 2 vertices is skipped.
    pub fn add_line(&mut self, coords: &[[f64; 2]], elevation: i16) -> Result<()> {
        self.grid.clear();
        for c in coords {
            let g = [to_grid(c[0]), to_grid(c[1])];
            if self.grid.last() != Some(&g) { self.grid.push(g); }
        }
        self.push_grid_line(elevation)
    }

    /// Add a line given as grid cells, e.g. decoded from another pack (so
    /// packing a pack again is lossless).
    pub fn add_cells(&mut self, cells: &[[u32; 2]], elevation: i16) -> Result<()> {
        self.grid.clear();
        for g in cells {
            if self.grid.last() != Some(g) { self.grid.push(*g); }
        }
        self.push_grid_line(elevation)
    }

    fn push_grid_line(&mut self, elevation: i16) -> Result<()> {
        let n = self.grid.len();
        if n < 2 { return Ok(()); }
        let line = u32::try_from(self.lines.len()).context("more than 4 billion lines in one pack")?;
        let mut line_box = EMPTY_BOX;
        let mut first = 0usize;
        while first < n - 1 {
            let last = (first + CHUNK_SEGMENTS).min(n - 1);
            let pts = &self.grid[first..=last];
            let mut b = EMPTY_BOX;
            self.scratch.clear();
            put_varint(&mut self.scratch, pts[0][0] as u64);
            put_varint(&mut self.scratch, pts[0][1] as u64);
            for w in pts.windows(2) {
                put_varint(&mut self.scratch, zigzag(w[1][0] as i64 - w[0][0] as i64));
                put_varint(&mut self.scratch, zigzag(w[1][1] as i64 - w[0][1] as i64));
            }
            for p in pts { union(&mut b, &[p[0], p[1], p[0], p[1]]); }
            union(&mut line_box, &b);
            self.chunks.push(ChunkRec { bbox: b, data_off: self.data_len, line, first_vertex: first as u32 });
            self.file.write_all(&self.scratch)?;
            self.data_len += self.scratch.len() as u64;
            first = last;
        }
        self.lines.push(LineRec { bbox: line_box, n_vertices: n as u32, elevation, _pad: 0 });
        self.n_vertices += n as u64;
        Ok(())
    }

    pub fn finish(mut self, meta: &serde_json::Value) -> Result<PackStats> {
        let mut pos = HEADER_SIZE as u64 + self.data_len;
        let pad = |f: &mut BufWriter<File>, pos: &mut u64| -> Result<()> {
            let p = (8 - (*pos % 8)) % 8;
            f.write_all(&[0u8; 8][..p as usize])?;
            *pos += p;
            Ok(())
        };
        pad(&mut self.file, &mut pos)?;
        let lines_off = pos;
        self.file.write_all(bytemuck::cast_slice(&self.lines))?;
        pos += (self.lines.len() * std::mem::size_of::<LineRec>()) as u64;
        pad(&mut self.file, &mut pos)?;

        // Chunks in Hilbert order are the tree's leaves; nodes above them.
        self.chunks.sort_by_cached_key(|c| hilbert_key(&c.bbox));
        let chunks_off = pos;
        self.file.write_all(bytemuck::cast_slice(&self.chunks))?;
        pos += (self.chunks.len() * std::mem::size_of::<ChunkRec>()) as u64;
        let mut nodes: Vec<GridBox> = Vec::new();
        let mut below: Vec<GridBox> = self.chunks.iter().map(|c| c.bbox).collect();
        for _ in level_sizes(self.chunks.len()) {
            let level: Vec<GridBox> = below.chunks(NODE_SIZE).map(|kids| {
                let mut b = EMPTY_BOX;
                for k in kids { union(&mut b, k); }
                b
            }).collect();
            nodes.extend_from_slice(&level);
            below = level;
        }
        let nodes_off = pos;
        self.file.write_all(bytemuck::cast_slice(&nodes))?;
        pos += (nodes.len() * std::mem::size_of::<GridBox>()) as u64;

        let meta_bytes = serde_json::to_vec(meta)?;
        let meta_off = pos;
        self.file.write_all(&meta_bytes)?;
        pos += meta_bytes.len() as u64;

        let mut bbox = EMPTY_BOX;
        for l in &self.lines { union(&mut bbox, &l.bbox); }
        let header = Header {
            magic: MAGIC,
            version: VERSION,
            chunk_segments: CHUNK_SEGMENTS as u32,
            n_lines: self.lines.len() as u64,
            n_chunks: self.chunks.len() as u64,
            n_vertices: self.n_vertices,
            data_off: HEADER_SIZE as u64,
            data_len: self.data_len,
            lines_off,
            chunks_off,
            nodes_off,
            n_nodes: nodes.len() as u64,
            meta_off,
            meta_len: meta_bytes.len() as u64,
            bbox,
        };
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(bytemuck::bytes_of(&header))?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        Ok(PackStats { lines: header.n_lines, vertices: self.n_vertices, bytes: pos, seam_features: 0 })
    }
}

/// Pack the polylines of a GDAL-readable vector file (SHP, FlatGeobuf,
/// GeoPackage, …) into `out`. `meta` gets the source's path and size/mtime
/// and the options.
/// Pack the polylines of a GDAL-readable vector file (SHP, FlatGeobuf,
/// GeoPackage, …) into `out`.
pub fn pack_vector_file(source: &Path, elevation_field: &str, out: &Path, opts: &PackOptions) -> Result<PackStats> {
    pack_sources(&[(source.to_path_buf(), elevation_field.to_string())], out, opts)
}

/// Pack sources into one pack, in the order given: vector files (with the
/// column holding the elevation) and packs, whose lines are copied as they
/// are. `--drop-seams` applies to vector files only. The pack's metadata
/// records each source's path, size and modification time, and the options.
pub fn pack_sources(sources: &[(PathBuf, String)], out: &Path, opts: &PackOptions) -> Result<PackStats> {
    let mut w = PackWriter::create(out)?;
    let mut seam_features = 0;
    let mut recorded = Vec::new();
    for (source, field) in sources {
        if crate::build::is_pack(source) {
            let p = Pack::open(source)?;
            p.for_each_line(|elevation, cells| w.add_cells(cells, elevation))
                .with_context(|| format!("read {:?}", source))?;
        } else {
            seam_features += crate::shapefile_read::read_polylines_streaming(source, field, opts.drop_seams, |f| {
                w.add_line(&f.coords, f.elevation)
            }).with_context(|| format!("read {:?}", source))?;
        }
        let m = std::fs::metadata(source).with_context(|| format!("stat {:?}", source))?;
        recorded.push(serde_json::json!({
            "source": source.to_string_lossy(),
            "source_size": m.len(),
            "source_mtime_ns": mtime_ns(&m).to_string(),
            "elevation_field": field,
        }));
    }
    // one source: its fields at the top level, as packs have always had them
    let mut meta = match recorded.len() {
        1 => recorded.pop().unwrap(),
        _ => serde_json::json!({ "sources": recorded }),
    };
    meta["drop_seams"] = serde_json::json!(opts.drop_seams);
    meta["corrie"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    Ok(PackStats { seam_features, ..w.finish(&meta)? })
}

/// Does the pack at `pack` hold `sources` as they are now (same paths in the
/// same order, sizes, modification times, elevation fields and options)?
pub fn is_current(pack: &Path, sources: &[(PathBuf, String)], opts: &PackOptions) -> bool {
    let Ok(p) = Pack::open(pack) else { return false };
    let Ok(meta) = p.meta() else { return false };
    if meta["drop_seams"].as_f64() != opts.drop_seams { return false; }
    let recorded: Vec<&serde_json::Value> = match meta["sources"].as_array() {
        Some(list) => list.iter().collect(),
        None => vec![&meta],
    };
    recorded.len() == sources.len() && recorded.iter().zip(sources).all(|(r, (source, field))| {
        let Ok(m) = std::fs::metadata(source) else { return false };
        r["source"].as_str() == Some(&*source.to_string_lossy())
            && r["source_size"].as_u64() == Some(m.len())
            && r["source_mtime_ns"].as_str() == Some(&*mtime_ns(&m).to_string())
            && r["elevation_field"].as_str() == Some(field)
    })
}

/// One pack to make: its sources (path, elevation column) and its path.
#[derive(Debug, Clone)]
pub struct PackJob {
    pub sources: Vec<(PathBuf, String)>,
    pub out: PathBuf,
}

/// Make several packs in parallel. Each is written as `<output>.partial` and
/// renamed when complete. Returns the number of source features left out as
/// degree-seam lines.
pub fn pack_files(jobs: &[PackJob], opts: &PackOptions) -> Result<u64> {
    use indicatif::{ProgressBar, ProgressStyle};
    use rayon::prelude::*;
    let pb = ProgressBar::new(jobs.len() as u64);
    pb.set_style(ProgressStyle::with_template(
        "[{elapsed_precise}] {bar:45.green/blue} {pos:>4}/{len} packs  eta {eta}").unwrap());
    let seams = jobs.par_iter().map(|job| -> Result<u64> {
        let partial = crate::cleanup::PartialOutput::new(&job.out)?;
        let stats = pack_sources(&job.sources, partial.path(), opts)?;
        partial.commit()?;
        pb.inc(1);
        Ok(stats.seam_features)
    }).try_reduce(|| 0, |a, b| Ok(a + b))?;
    pb.finish_and_clear();
    Ok(seams)
}

pub fn mtime_ns(m: &std::fs::Metadata) -> u128 {
    m.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ---------------- reader ----------------

/// A pack opened read-only (mmap).
pub struct Pack {
    path: PathBuf,
    mmap: Mmap,
    header: Header,
    /// (start, count) of each tree level in `nodes()`, level 1 first.
    levels: Vec<(usize, usize)>,
}

impl Pack {
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path).with_context(|| format!("open {:?}", path))?;
        let mmap = unsafe { Mmap::map(&f)? };
        if mmap.len() < HEADER_SIZE || mmap[..8] != MAGIC {
            bail!("{:?} is not a corrie pack", path);
        }
        let header: Header = bytemuck::pod_read_unaligned(&mmap[..HEADER_SIZE]);
        if header.version != VERSION {
            bail!("{:?}: pack version {} is not supported (this corrie reads {})", path, header.version, VERSION);
        }
        if header.chunk_segments as usize != CHUNK_SEGMENTS {
            bail!("{:?}: unexpected chunk size {}", path, header.chunk_segments);
        }
        let end = header.meta_off + header.meta_len;
        if end > mmap.len() as u64 {
            bail!("{:?} is truncated ({} of {} bytes)", path, mmap.len(), end);
        }
        let mut levels = Vec::new();
        let mut start = 0;
        for size in level_sizes(header.n_chunks as usize) {
            levels.push((start, size));
            start += size;
        }
        if start as u64 != header.n_nodes {
            bail!("{:?}: index has {} nodes, expected {}", path, header.n_nodes, start);
        }
        Ok(Self { path: path.to_path_buf(), mmap, header, levels })
    }

    pub fn path(&self) -> &Path { &self.path }
    pub fn header(&self) -> &Header { &self.header }
    pub fn bbox(&self) -> GridBox { self.header.bbox }
    pub fn is_empty(&self) -> bool { self.header.n_lines == 0 }

    pub fn lines(&self) -> &[LineRec] {
        let o = self.header.lines_off as usize;
        bytemuck::cast_slice(&self.mmap[o..o + self.header.n_lines as usize * std::mem::size_of::<LineRec>()])
    }

    pub fn chunks(&self) -> &[ChunkRec] {
        let o = self.header.chunks_off as usize;
        bytemuck::cast_slice(&self.mmap[o..o + self.header.n_chunks as usize * std::mem::size_of::<ChunkRec>()])
    }

    fn nodes(&self) -> &[GridBox] {
        let o = self.header.nodes_off as usize;
        bytemuck::cast_slice(&self.mmap[o..o + self.header.n_nodes as usize * std::mem::size_of::<GridBox>()])
    }

    pub fn meta(&self) -> Result<serde_json::Value> {
        let o = self.header.meta_off as usize;
        Ok(serde_json::from_slice(&self.mmap[o..o + self.header.meta_len as usize])?)
    }

    /// Call `f(chunk index)` for every chunk whose box intersects `q`.
    /// Stops early when `f` returns false; returns false if it stopped.
    pub fn query(&self, q: &GridBox, mut f: impl FnMut(u32) -> bool) -> bool {
        let chunks = self.chunks();
        if chunks.is_empty() || !boxes_intersect(&self.header.bbox, q) { return true; }
        let nodes = self.nodes();
        // stack of (level, index); level 0 = chunks
        let mut stack: Vec<(usize, usize)> = Vec::with_capacity(64);
        match self.levels.last() {
            None => stack.push((0, 0)), // one chunk, no nodes
            Some(&(start, count)) => {
                for i in 0..count {
                    if boxes_intersect(&nodes[start + i], q) { stack.push((self.levels.len(), i)); }
                }
            }
        }
        while let Some((level, i)) = stack.pop() {
            if level == 0 {
                if boxes_intersect(&chunks[i].bbox, q) && !f(i as u32) { return false; }
                continue;
            }
            let below = if level == 1 { chunks.len() } else { self.levels[level - 2].1 };
            let (lo, hi) = (i * NODE_SIZE, ((i + 1) * NODE_SIZE).min(below));
            for k in lo..hi {
                let b = if level == 1 { &chunks[k].bbox } else { &nodes[self.levels[level - 2].0 + k] };
                if boxes_intersect(b, q) { stack.push((level - 1, k)); }
            }
        }
        true
    }

    /// Is any chunk inside `q`?
    pub fn any(&self, q: &GridBox) -> bool {
        !self.query(q, |_| false)
    }

    /// Append the vertices of chunk `ci` as `(lon, latp)` degrees to `out`,
    /// skipping its first vertex if `skip_first` (the previous chunk of the
    /// same line already ended there).
    pub fn decode_chunk(&self, ci: u32, skip_first: bool, out: &mut Vec<[f64; 2]>) {
        self.decode_chunk_with(ci, skip_first, |x, y| out.push([from_grid(x), from_grid(y)]));
    }

    /// Chunk `ci`'s vertices as grid cells.
    pub fn decode_chunk_cells(&self, ci: u32, skip_first: bool, out: &mut Vec<[u32; 2]>) {
        self.decode_chunk_with(ci, skip_first, |x, y| out.push([x, y]));
    }

    fn decode_chunk_with(&self, ci: u32, skip_first: bool, mut f: impl FnMut(u32, u32)) {
        let c = &self.chunks()[ci as usize];
        let n_line = self.lines()[c.line as usize].n_vertices as usize;
        let n = (c.first_vertex as usize + CHUNK_SEGMENTS).min(n_line - 1) - c.first_vertex as usize + 1;
        let data = &self.mmap[self.header.data_off as usize..];
        let mut pos = c.data_off as usize;
        let mut x = get_varint(data, &mut pos) as i64;
        let mut y = get_varint(data, &mut pos) as i64;
        if !skip_first { f(x as u32, y as u32); }
        for _ in 1..n {
            x += unzigzag(get_varint(data, &mut pos));
            y += unzigzag(get_varint(data, &mut pos));
            f(x as u32, y as u32);
        }
    }

    /// Every line, in the order it was packed: its elevation and its grid
    /// cells.
    pub fn for_each_line(&self, mut f: impl FnMut(i16, &[[u32; 2]]) -> Result<()>) -> Result<()> {
        let mut order: Vec<(u32, u32, u32)> = self.chunks().iter().enumerate()
            .map(|(i, c)| (c.line, c.first_vertex, i as u32))
            .collect();
        order.sort_unstable();
        let lines = self.lines();
        let mut cells = Vec::new();
        let mut k = 0;
        while k < order.len() {
            let line = order[k].0;
            cells.clear();
            let mut first = true;
            while k < order.len() && order[k].0 == line {
                self.decode_chunk_cells(order[k].2, !first, &mut cells);
                first = false;
                k += 1;
            }
            f(lines[line as usize].elevation, &cells)?;
        }
        Ok(())
    }

    /// All vertices of line `li`, in order (for unpacking and tests).
    pub fn decode_line(&self, li: u32, out: &mut Vec<[f64; 2]>) {
        let mut ids: Vec<(u32, u32)> = self.chunks().iter().enumerate()
            .filter(|(_, c)| c.line == li)
            .map(|(i, c)| (c.first_vertex, i as u32))
            .collect();
        ids.sort_unstable();
        for (k, &(_, ci)) in ids.iter().enumerate() {
            self.decode_chunk(ci, k > 0, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs packed again into one keep every line, cell for cell, in order;
    /// vector sources can be mixed in.
    #[test]
    fn packs_merge_without_loss() {
        let dir = tempfile::tempdir().unwrap();
        let geojson = |name: &str, lines: &[(i32, &str)]| {
            let f: Vec<String> = lines.iter().map(|(e, c)| format!(
                r#"{{"type":"Feature","properties":{{"elevation":{e}}},"geometry":{{"type":"LineString","coordinates":{c}}}}}"#)).collect();
            let p = dir.path().join(name);
            std::fs::write(&p, format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, f.join(","))).unwrap();
            p
        };
        let a = geojson("a.geojson", &[(100, "[[14.1,50.1],[14.2,50.15],[14.3,50.1]]"), (110, "[[14.0,49.9],[14.05,49.95]]")]);
        let b = geojson("b.geojson", &[(120, "[[-78.5,-1.5],[-78.4,-1.4],[-78.3,-1.45],[-78.2,-1.4]]")]);
        let c = geojson("c.geojson", &[(130, "[[55.4,-21.1],[55.5,-21.0]]")]);
        let field = |p: &Path| (p.to_path_buf(), "elevation".to_string());
        let (pa, pb) = (dir.path().join("a.cpk"), dir.path().join("b.cpk"));
        pack_vector_file(&a, "elevation", &pa, &PackOptions::default()).unwrap();
        pack_vector_file(&b, "elevation", &pb, &PackOptions::default()).unwrap();
        // a long line spans several chunks
        let long: String = (0..1000).map(|i| format!("[{},{}]", 10.0 + i as f64 * 0.001, 45.0 + (i % 7) as f64 * 0.0003)).collect::<Vec<_>>().join(",");
        let d = geojson("d.geojson", &[(140, &format!("[{long}]"))]);
        let pd = dir.path().join("d.cpk");
        pack_vector_file(&d, "elevation", &pd, &PackOptions::default()).unwrap();

        let all = dir.path().join("all.cpk");
        let sources = [field(&pa), field(&pb), field(&c), field(&pd)];
        let stats = pack_sources(&sources, &all, &PackOptions::default()).unwrap();
        assert_eq!(stats.lines, 5);

        let lines_of = |p: &Path| {
            let mut v = Vec::new();
            Pack::open(p).unwrap().for_each_line(|e, cells| { v.push((e, cells.to_vec())); Ok(()) }).unwrap();
            v
        };
        let pc = dir.path().join("c.cpk");
        pack_vector_file(&c, "elevation", &pc, &PackOptions::default()).unwrap();
        let expected: Vec<_> = [&pa, &pb, &pc, &pd].iter().flat_map(|p| lines_of(p)).collect();
        assert_eq!(lines_of(&all), expected);
        assert!(lines_of(&pd)[0].1.len() > 2 * CHUNK_SEGMENTS);

        // current while every source is unchanged, in the same order
        assert!(is_current(&all, &sources, &PackOptions::default()));
        let reordered = [field(&pb), field(&pa), field(&c), field(&pd)];
        assert!(!is_current(&all, &reordered, &PackOptions::default()));
        std::fs::write(&c, std::fs::read(&c).unwrap().iter().chain(b" ").copied().collect::<Vec<u8>>()).unwrap();
        assert!(!is_current(&all, &sources, &PackOptions::default()));
    }

    /// --drop-seams decides per feature, over all its parts: a MultiLineString
    /// with one part off the seams is kept whole.
    #[test]
    fn drop_seams_leaves_out_whole_seam_features() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("lines.geojson");
        let f = |elev: i32, geom: &str| format!(r#"{{"type":"Feature","properties":{{"elevation":{elev}}},"geometry":{geom}}}"#);
        let features = [
            f(100, r#"{"type":"LineString","coordinates":[[-79.0004,-1.2],[-78.9998,-1.6]]}"#),
            f(110, r#"{"type":"MultiLineString","coordinates":[[[-79.0004,-1.2],[-78.9998,-1.6]],[[-78.5,-2.0003],[-78.4,-1.9998]]]}"#),
            f(120, r#"{"type":"MultiLineString","coordinates":[[[-79.0004,-1.2],[-78.9998,-1.6]],[[-78.5,-1.5],[-78.4,-1.4]]]}"#),
            f(130, r#"{"type":"LineString","coordinates":[[-78.6,-1.3],[-78.5,-1.2]]}"#),
        ];
        std::fs::write(&src, format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","))).unwrap();

        let plain = dir.path().join("plain.cpk");
        let all = pack_vector_file(&src, "elevation", &plain, &PackOptions::default()).unwrap();
        assert_eq!((all.lines, all.seam_features), (6, 0));

        let opts = PackOptions { drop_seams: Some(0.001) };
        let out = dir.path().join("seams.cpk");
        let kept = pack_vector_file(&src, "elevation", &out, &opts).unwrap();
        assert_eq!((kept.lines, kept.seam_features), (3, 2));
        let pack = Pack::open(&out).unwrap();
        let elevations: Vec<i16> = pack.lines().iter().map(|l| l.elevation).collect();
        assert_eq!(elevations, vec![120, 120, 130]);

        // the option is part of what makes a pack current
        let one = [(src.clone(), "elevation".to_string())];
        assert!(is_current(&out, &one, &opts));
        assert!(!is_current(&out, &one, &PackOptions::default()));
        assert!(is_current(&plain, &one, &PackOptions::default()));
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12; self.0 ^= self.0 << 25; self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545F4914F6CDD1D)
        }
        fn unit(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
    }

    fn random_lines(seed: u64, n: usize) -> Vec<(Vec<[f64; 2]>, i16)> {
        let mut r = Rng(seed);
        (0..n).map(|i| {
            let len = 2 + (r.next() % 700) as usize;
            let (mut x, mut y) = (14.0 + r.unit(), 55.0 + r.unit());
            let pts = (0..len).map(|_| { x += (r.unit() - 0.5) * 1e-3; y += (r.unit() - 0.5) * 1e-3; [x, y] }).collect();
            (pts, (i % 300) as i16 * 10)
        }).collect()
    }

    fn write(lines: &[(Vec<[f64; 2]>, i16)]) -> (tempfile::TempDir, Pack) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.cpk");
        let mut w = PackWriter::create(&p).unwrap();
        for (l, e) in lines { w.add_line(l, *e).unwrap(); }
        w.finish(&serde_json::json!({"test": true})).unwrap();
        let pack = Pack::open(&p).unwrap();
        (dir, pack)
    }

    #[test]
    fn grid_round_trip_is_within_half_a_step() {
        let step = 360.0 / GRID;
        for &v in &[-180.0, -179.999, -12.3456789012, 0.0, 14.123456789, 179.9999999] {
            assert!((from_grid(to_grid(v)) - v).abs() <= step / 2.0 + 1e-12, "{v}");
        }
        assert_eq!(to_grid(180.0), u32::MAX);
        assert_eq!(to_grid(-200.0), 0);
    }

    #[test]
    fn lines_decode_to_their_grid_cells() {
        let lines = random_lines(1, 300);
        let (_d, pack) = write(&lines);
        assert_eq!(pack.lines().len(), 300);
        let mut out = Vec::new();
        for (li, (src, elev)) in lines.iter().enumerate() {
            out.clear();
            pack.decode_line(li as u32, &mut out);
            let mut want: Vec<[u32; 2]> = Vec::new();
            for c in src {
                let g = [to_grid(c[0]), to_grid(c[1])];
                if want.last() != Some(&g) { want.push(g); }
            }
            let got: Vec<[u32; 2]> = out.iter().map(|c| [to_grid(c[0]), to_grid(c[1])]).collect();
            assert_eq!(got, want, "line {li}");
            assert_eq!(pack.lines()[li].elevation, *elev);
            assert_eq!(pack.lines()[li].n_vertices as usize, want.len());
        }
        assert_eq!(pack.meta().unwrap()["test"], true);
    }

    #[test]
    fn query_finds_exactly_the_intersecting_chunks() {
        let lines = random_lines(2, 2000);
        let (_d, pack) = write(&lines);
        let mut r = Rng(3);
        for _ in 0..200 {
            let (x, y) = (to_grid(14.0 + r.unit()), to_grid(55.0 + r.unit()));
            let w = (r.unit() * 1e-2 / 360.0 * GRID) as u32;
            let q = [x, y, x.saturating_add(w), y.saturating_add(w)];
            let mut got = Vec::new();
            pack.query(&q, |ci| { got.push(ci); true });
            got.sort_unstable();
            let want: Vec<u32> = pack.chunks().iter().enumerate()
                .filter(|(_, c)| boxes_intersect(&c.bbox, &q)).map(|(i, _)| i as u32).collect();
            assert_eq!(got, want);
            assert_eq!(pack.any(&q), !want.is_empty());
        }
    }

    #[test]
    fn tiny_and_empty_packs() {
        let (_d, empty) = write(&[]);
        assert!(empty.is_empty());
        assert!(!empty.any(&[0, 0, u32::MAX, u32::MAX]));
        let (_d, one) = write(&[(vec![[14.0, 55.0], [14.001, 55.001]], 100)]);
        assert_eq!(one.chunks().len(), 1);
        assert!(one.any(&[0, 0, u32::MAX, u32::MAX]));
        // a line collapsing into one grid cell is dropped
        let (_d, dropped) = write(&[(vec![[14.0, 55.0], [14.0, 55.0]], 100)]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn rejects_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.cpk");
        std::fs::write(&p, b"PMTiles not a pack at all, padded to be long enough for a header ............................................................").unwrap();
        let err = Pack::open(&p).err().expect("must not open");
        assert!(err.to_string().contains("not a corrie pack"), "{err}");
    }
}
