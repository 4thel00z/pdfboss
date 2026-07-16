//! Minimal TrueType (`glyf`) font parser: reads the tables needed to turn a
//! glyph index into an outline (`head`, `maxp`, `loca`, `glyf`) plus `cmap`
//! for character-to-glyph mapping. Quadratic outlines are emitted as path
//! [`Seg`]ments in font units.
//!
//! CFF-flavoured OpenType (`OTTO`, no `glyf`) is not handled. Every accessor is
//! bounds-checked so a malformed embedded font yields empty output rather than
//! a panic.

use pdfboss_core::FastMap;

/// One outline command in font units. The on-curve start of each `Quad` is the
/// current point (the end of the previous command).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Quadratic Bezier: control point then end point.
    Quad(f32, f32, f32, f32),
    /// Cubic Bezier: two control points then the end point.
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A parsed TrueType font with the tables required for outline extraction.
pub(crate) struct TrueType {
    data: Vec<u8>,
    units_per_em: u16,
    /// Absolute byte offsets into `data` for each glyph, `num_glyphs + 1` long.
    loca: Vec<usize>,
    cmap: Option<Cmap>,
    post: Option<Post>,
    /// Per-glyph advance widths in font units (from `hmtx`), or empty.
    advances: Vec<u16>,
}

impl TrueType {
    /// Parses a font program, returning `None` if it is not `glyf`-based or is
    /// too malformed to yield the required tables.
    pub(crate) fn parse(data: Vec<u8>) -> Option<TrueType> {
        let sfnt = be32(&data, 0)?;
        // Accept TrueType (0x00010000) and the 'true'/'ttcf' tags; reject OTTO
        // (CFF outlines, which have no glyf table).
        if sfnt == 0x4F54_544F {
            return None;
        }
        let num_tables = be16(&data, 4)? as usize;
        let mut head = None;
        let mut maxp = None;
        let mut loca_tab = None;
        let mut glyf = None;
        let mut cmap_off = None;
        let mut hhea_off = None;
        let mut hmtx_off = None;
        let mut post_tab = None;
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            let tag = data.get(rec..rec + 4)?;
            let off = be32(&data, rec + 8)? as usize;
            let len = be32(&data, rec + 12)? as usize;
            match tag {
                b"head" => head = Some((off, len)),
                b"maxp" => maxp = Some((off, len)),
                b"loca" => loca_tab = Some((off, len)),
                b"glyf" => glyf = Some((off, len)),
                b"cmap" => cmap_off = Some(off),
                b"hhea" => hhea_off = Some(off),
                b"hmtx" => hmtx_off = Some(off),
                b"post" => post_tab = Some((off, len)),
                _ => {}
            }
        }
        let (head_off, _) = head?;
        let (maxp_off, _) = maxp?;
        let (loca_off, loca_len) = loca_tab?;
        let (glyf_off, glyf_len) = glyf?;

        let units_per_em = be16(&data, head_off + 18)?.max(1);
        let index_to_loc = bei16(&data, head_off + 50)?; // 0 short, 1 long
        let num_glyphs = be16(&data, maxp_off + 4)?;

        // loca holds num_glyphs + 1 offsets into glyf (short entries are ×2).
        let count = num_glyphs as usize + 1;
        let mut loca = Vec::with_capacity(count);
        if index_to_loc == 0 {
            for i in 0..count {
                let v = be16(&data, loca_off + i * 2).filter(|_| i * 2 + 2 <= loca_len)? as usize;
                loca.push(glyf_off + v * 2);
            }
        } else {
            for i in 0..count {
                let v = be32(&data, loca_off + i * 4).filter(|_| i * 4 + 4 <= loca_len)? as usize;
                loca.push(glyf_off + v);
            }
        }
        // Clamp offsets into the glyf table.
        let glyf_end = glyf_off + glyf_len;
        for o in &mut loca {
            *o = (*o).min(glyf_end).min(data.len());
        }

        let cmap = cmap_off.and_then(|o| Cmap::parse(&data, o));
        let post = post_tab.and_then(|(o, l)| Post::parse(&data, o, l));

        // hmtx: `numberOfHMetrics` (from hhea) long-metric records of
        // (advanceWidth u16, lsb i16); glyphs past that reuse the last advance.
        let mut advances = Vec::new();
        if let (Some(hhea), Some(hmtx)) = (hhea_off, hmtx_off) {
            if let Some(nhm) = be16(&data, hhea + 34) {
                let nhm = (nhm as usize).min(num_glyphs as usize);
                let mut last = 0u16;
                for i in 0..nhm {
                    last = be16(&data, hmtx + i * 4).unwrap_or(last);
                    advances.push(last);
                }
                for _ in nhm..num_glyphs as usize {
                    advances.push(last); // monospaced tail shares the last width
                }
            }
        }

        Some(TrueType {
            data,
            units_per_em,
            loca,
            cmap,
            post,
            advances,
        })
    }

    /// Advance width of glyph `gid` in font units (0 if metrics are absent).
    pub(crate) fn advance(&self, gid: u16) -> u16 {
        self.advances.get(gid as usize).copied().unwrap_or(0)
    }

    /// Font design units per em (the outline coordinate space).
    pub(crate) fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Maps a Unicode scalar to a glyph index via the selected `cmap` subtable.
    pub(crate) fn gid_for_unicode(&self, cp: u32) -> Option<u16> {
        self.cmap.as_ref().and_then(|c| c.lookup(&self.data, cp))
    }

    /// Whether a usable `cmap` subtable was found.
    pub(crate) fn has_cmap(&self) -> bool {
        self.cmap.is_some()
    }

    /// Maps a glyph name to a glyph index via the `post` table's custom names.
    pub(crate) fn gid_for_name(&self, name: &str) -> Option<u16> {
        self.post.as_ref().and_then(|p| p.gid_for_name(name))
    }

    /// The outline of glyph `gid` as path segments in font units. Empty glyphs
    /// (e.g. the space) and out-of-range indices yield an empty vector.
    pub(crate) fn glyph_path(&self, gid: u16) -> Vec<Seg> {
        let mut out = Vec::new();
        self.append_glyph(gid, &mut out, 0);
        out
    }

    fn append_glyph(&self, gid: u16, out: &mut Vec<Seg>, depth: u32) {
        if depth > 8 || gid as usize + 1 >= self.loca.len() {
            return;
        }
        let start = self.loca[gid as usize];
        let end = self.loca[gid as usize + 1];
        if end <= start {
            return; // empty glyph
        }
        let Some(g) = self.data.get(start..end) else {
            return;
        };
        let Some(num_contours) = bei16(g, 0) else {
            return;
        };
        if num_contours >= 0 {
            simple_glyph(g, num_contours as usize, out);
        } else {
            self.composite_glyph(g, out, depth);
        }
    }

    fn composite_glyph(&self, g: &[u8], out: &mut Vec<Seg>, depth: u32) {
        const ARGS_ARE_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY: u16 = 0x0002;
        const HAVE_SCALE: u16 = 0x0008;
        const MORE_COMPONENTS: u16 = 0x0020;
        const HAVE_XY_SCALE: u16 = 0x0040;
        const HAVE_2X2: u16 = 0x0080;

        let mut p = 10; // after the 10-byte glyph header
        loop {
            let Some(flags) = be16(g, p) else { return };
            let Some(component) = be16(g, p + 2) else {
                return;
            };
            p += 4;
            let (dx, dy) = if flags & ARGS_ARE_WORDS != 0 {
                let (Some(a), Some(b)) = (bei16(g, p), bei16(g, p + 2)) else {
                    return;
                };
                p += 4;
                (a as f32, b as f32)
            } else {
                let (Some(a), Some(b)) =
                    (g.get(p).map(|&x| x as i8), g.get(p + 1).map(|&x| x as i8))
                else {
                    return;
                };
                p += 2;
                (a as f32, b as f32)
            };
            let (mut sa, mut sb, mut sc, mut sd) = (1.0f32, 0.0, 0.0, 1.0);
            if flags & HAVE_SCALE != 0 {
                let s = f2dot14(g, p);
                sa = s;
                sd = s;
                p += 2;
            } else if flags & HAVE_XY_SCALE != 0 {
                sa = f2dot14(g, p);
                sd = f2dot14(g, p + 2);
                p += 4;
            } else if flags & HAVE_2X2 != 0 {
                sa = f2dot14(g, p);
                sb = f2dot14(g, p + 2);
                sc = f2dot14(g, p + 4);
                sd = f2dot14(g, p + 6);
                p += 8;
            }
            // Component offset is only a translation when ARGS_ARE_XY is set;
            // point-matching (the alternative) is rare and skipped.
            let (tx, ty) = if flags & ARGS_ARE_XY != 0 {
                (dx, dy)
            } else {
                (0.0, 0.0)
            };
            let mut sub = Vec::new();
            self.append_glyph(component, &mut sub, depth + 1);
            for seg in sub {
                out.push(transform_seg(seg, sa, sb, sc, sd, tx, ty));
            }
            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
    }
}

/// Applies `[a b c d]` linear part and `(tx, ty)` translation to a segment.
fn transform_seg(seg: Seg, a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32) -> Seg {
    let f = |x: f32, y: f32| (a * x + c * y + tx, b * x + d * y + ty);
    match seg {
        Seg::Move(x, y) => {
            let (x, y) = f(x, y);
            Seg::Move(x, y)
        }
        Seg::Line(x, y) => {
            let (x, y) = f(x, y);
            Seg::Line(x, y)
        }
        Seg::Quad(cx, cy, x, y) => {
            let (cx, cy) = f(cx, cy);
            let (x, y) = f(x, y);
            Seg::Quad(cx, cy, x, y)
        }
        Seg::Cubic(x1, y1, x2, y2, x, y) => {
            let (x1, y1) = f(x1, y1);
            let (x2, y2) = f(x2, y2);
            let (x, y) = f(x, y);
            Seg::Cubic(x1, y1, x2, y2, x, y)
        }
        Seg::Close => Seg::Close,
    }
}

/// Decodes a simple (non-composite) glyph's contours into segments.
fn simple_glyph(g: &[u8], num_contours: usize, out: &mut Vec<Seg>) {
    let mut p = 10;
    let mut end_pts = Vec::with_capacity(num_contours);
    for _ in 0..num_contours {
        let Some(e) = be16(g, p) else { return };
        end_pts.push(e as usize);
        p += 2;
    }
    let num_points = match end_pts.last() {
        Some(&last) => last + 1,
        None => return,
    };
    // Skip the instructions.
    let Some(instr_len) = be16(g, p) else { return };
    p += 2 + instr_len as usize;

    // Flags (run-length encoded via the REPEAT bit).
    const ON_CURVE: u8 = 0x01;
    const X_SHORT: u8 = 0x02;
    const Y_SHORT: u8 = 0x04;
    const REPEAT: u8 = 0x08;
    const X_SAME: u8 = 0x10; // or "x is positive" when X_SHORT
    const Y_SAME: u8 = 0x20;
    let mut flags = Vec::with_capacity(num_points);
    while flags.len() < num_points {
        let Some(&f) = g.get(p) else { return };
        p += 1;
        flags.push(f);
        if f & REPEAT != 0 {
            let Some(&count) = g.get(p) else { return };
            p += 1;
            for _ in 0..count {
                if flags.len() >= num_points {
                    break;
                }
                flags.push(f);
            }
        }
    }

    // X coordinates (delta-encoded).
    let mut xs = Vec::with_capacity(num_points);
    let mut x = 0i32;
    for &f in &flags {
        if f & X_SHORT != 0 {
            let Some(&d) = g.get(p) else { return };
            p += 1;
            x += if f & X_SAME != 0 {
                d as i32
            } else {
                -(d as i32)
            };
        } else if f & X_SAME == 0 {
            let Some(d) = bei16(g, p) else { return };
            p += 2;
            x += d as i32;
        }
        xs.push(x);
    }
    // Y coordinates (delta-encoded).
    let mut ys = Vec::with_capacity(num_points);
    let mut y = 0i32;
    for &f in &flags {
        if f & Y_SHORT != 0 {
            let Some(&d) = g.get(p) else { return };
            p += 1;
            y += if f & Y_SAME != 0 {
                d as i32
            } else {
                -(d as i32)
            };
        } else if f & Y_SAME == 0 {
            let Some(d) = bei16(g, p) else { return };
            p += 2;
            y += d as i32;
        }
        ys.push(y);
    }

    // Emit each contour.
    let mut begin = 0usize;
    for &last in &end_pts {
        if last >= num_points {
            break;
        }
        let pts: Vec<Pt> = (begin..=last)
            .map(|i| Pt {
                x: xs[i] as f32,
                y: ys[i] as f32,
                on: flags[i] & ON_CURVE != 0,
            })
            .collect();
        contour_segs(&pts, out);
        begin = last + 1;
    }
}

#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    y: f32,
    on: bool,
}

fn mid(a: Pt, b: Pt) -> Pt {
    Pt {
        x: (a.x + b.x) * 0.5,
        y: (a.y + b.y) * 0.5,
        on: true,
    }
}

/// Turns one contour's points into `Move`/`Line`/`Quad`/`Close` segments,
/// inserting the implied on-curve midpoints between consecutive off-curve
/// points that TrueType leaves out.
fn contour_segs(pts: &[Pt], out: &mut Vec<Seg>) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    // Build a sequence that starts and ends on the same on-curve point.
    let seq: Vec<Pt> = match (0..n).find(|&i| pts[i].on) {
        Some(s) => (0..=n).map(|k| pts[(s + k) % n]).collect(),
        None => {
            // All control points: synthesize an on-curve start.
            let start = mid(pts[0], pts[n - 1]);
            let mut v = Vec::with_capacity(n + 2);
            v.push(start);
            v.extend_from_slice(pts);
            v.push(start);
            v
        }
    };

    out.push(Seg::Move(seq[0].x, seq[0].y));
    let mut i = 1;
    while i < seq.len() {
        let p = seq[i];
        if p.on {
            out.push(Seg::Line(p.x, p.y));
            i += 1;
        } else {
            // `seq` ends on-curve, so an off-curve point always has a successor.
            let next = seq[i + 1];
            if next.on {
                out.push(Seg::Quad(p.x, p.y, next.x, next.y));
                i += 2;
            } else {
                let m = mid(p, next);
                out.push(Seg::Quad(p.x, p.y, m.x, m.y));
                i += 1;
            }
        }
    }
    out.push(Seg::Close);
}

// --- cmap -----------------------------------------------------------------

/// A selected `cmap` subtable, stored as its absolute byte offset and format.
struct Cmap {
    offset: usize,
    format: u16,
}

impl Cmap {
    /// Chooses the best supported subtable: a Unicode/Windows table if present,
    /// otherwise the first symbol or Mac table.
    fn parse(data: &[u8], base: usize) -> Option<Cmap> {
        let num = be16(data, base + 2)? as usize;
        let mut best: Option<(i32, usize)> = None; // (score, subtable offset)
        for i in 0..num {
            let rec = base + 4 + i * 8;
            let platform = be16(data, rec)?;
            let encoding = be16(data, rec + 2)?;
            let sub = base + be32(data, rec + 4)? as usize;
            let score = match (platform, encoding) {
                (3, 10) => 5, // Windows UCS-4
                (0, _) => 4,  // Unicode
                (3, 1) => 4,  // Windows BMP
                (3, 0) => 2,  // Windows symbol
                (1, 0) => 1,  // Mac Roman
                _ => 0,
            };
            if score > 0 && best.map(|(s, _)| score > s).unwrap_or(true) {
                best = Some((score, sub));
            }
        }
        let offset = best?.1;
        let format = be16(data, offset)?;
        matches!(format, 0 | 4 | 6 | 12).then_some(Cmap { offset, format })
    }

    fn lookup(&self, data: &[u8], cp: u32) -> Option<u16> {
        match self.format {
            0 => self.lookup_0(data, cp),
            4 => self.lookup_4(data, cp),
            6 => self.lookup_6(data, cp),
            12 => self.lookup_12(data, cp),
            _ => None,
        }
    }

    fn lookup_0(&self, data: &[u8], cp: u32) -> Option<u16> {
        if cp > 255 {
            return None;
        }
        // Byte encoding table: 256 single-byte glyph indices at offset+6.
        data.get(self.offset + 6 + cp as usize).map(|&g| g as u16)
    }

    fn lookup_4(&self, data: &[u8], cp: u32) -> Option<u16> {
        if cp > 0xFFFF {
            return None;
        }
        let cp = cp as u16;
        let o = self.offset;
        let segx2 = be16(data, o + 6)? as usize;
        let segs = segx2 / 2;
        let ends = o + 14;
        let starts = ends + segx2 + 2; // +2 skips reservedPad
        let deltas = starts + segx2;
        let ranges = deltas + segx2;
        for s in 0..segs {
            let end = be16(data, ends + s * 2)?;
            if cp > end {
                continue;
            }
            let start = be16(data, starts + s * 2)?;
            if cp < start {
                return Some(0);
            }
            let delta = be16(data, deltas + s * 2)?;
            let range_off = be16(data, ranges + s * 2)?;
            if range_off == 0 {
                return Some(cp.wrapping_add(delta));
            }
            // Indirect glyph-id lookup through the glyphIdArray.
            let idx = ranges + s * 2 + range_off as usize + (cp - start) as usize * 2;
            let g = be16(data, idx)?;
            return Some(if g == 0 { 0 } else { g.wrapping_add(delta) });
        }
        Some(0)
    }

    fn lookup_6(&self, data: &[u8], cp: u32) -> Option<u16> {
        let o = self.offset;
        let first = be16(data, o + 6)? as u32;
        let count = be16(data, o + 8)? as u32;
        if cp < first || cp >= first + count {
            return None;
        }
        be16(data, o + 10 + (cp - first) as usize * 2)
    }

    fn lookup_12(&self, data: &[u8], cp: u32) -> Option<u16> {
        let o = self.offset;
        let groups = be32(data, o + 12)? as usize;
        for gi in 0..groups {
            let rec = o + 16 + gi * 12;
            let start = be32(data, rec)?;
            let end = be32(data, rec + 4)?;
            let start_gid = be32(data, rec + 8)?;
            if cp >= start && cp <= end {
                return Some((start_gid + (cp - start)) as u16);
            }
        }
        None
    }
}

/// The `post` table's custom glyph-name → glyph-id map (format 2.0 only).
///
/// Only glyph-name-index entries ≥ 258 (the font's own custom names) are
/// recorded; entries below 258 reference the standard Macintosh names, which
/// are recovered elsewhere through the Adobe Glyph List.
struct Post {
    names: FastMap<String, u16>,
}

impl Post {
    fn parse(data: &[u8], off: usize, len: usize) -> Option<Post> {
        if be32(data, off)? != 0x0002_0000 {
            return None; // versions 1.0/2.5/3.0 carry no custom names
        }
        let num = be16(data, off + 32)? as usize;
        let mut indices = Vec::with_capacity(num);
        for i in 0..num {
            match be16(data, off + 34 + i * 2) {
                Some(ix) => indices.push(ix),
                None => break,
            }
        }
        // Pascal strings follow the index array, bounded by the table length.
        let end = (off + len).min(data.len());
        let mut p = off + 34 + num * 2;
        let mut custom: Vec<String> = Vec::new();
        while p < end {
            let slen = *data.get(p)? as usize;
            p += 1;
            let Some(bytes) = data.get(p..p + slen).filter(|_| p + slen <= end) else {
                break;
            };
            p += slen;
            custom.push(String::from_utf8_lossy(bytes).into_owned());
        }
        let mut names = FastMap::default();
        for (gid, &ix) in indices.iter().enumerate() {
            if ix >= 258 {
                if let Some(name) = custom.get(ix as usize - 258) {
                    names.entry(name.clone()).or_insert(gid as u16);
                }
            }
        }
        (!names.is_empty()).then_some(Post { names })
    }

    fn gid_for_name(&self, name: &str) -> Option<u16> {
        self.names.get(name).copied()
    }
}

// --- big-endian readers (all bounds-checked) ------------------------------

fn be16(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn bei16(d: &[u8], o: usize) -> Option<i16> {
    be16(d, o).map(|v| v as i16)
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reads a 2.14 fixed-point number (composite-glyph scales); 0.0 if truncated.
fn f2dot14(d: &[u8], o: usize) -> f32 {
    bei16(d, o).map(|v| v as f32 / 16384.0).unwrap_or(0.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn pt(x: f32, y: f32, on: bool) -> Pt {
        Pt { x, y, on }
    }

    #[test]
    fn contour_all_on_curve_is_a_polygon() {
        let sq = [
            pt(0.0, 0.0, true),
            pt(10.0, 0.0, true),
            pt(10.0, 10.0, true),
            pt(0.0, 10.0, true),
        ];
        let mut out = Vec::new();
        contour_segs(&sq, &mut out);
        assert_eq!(
            out,
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Line(10.0, 0.0),
                Seg::Line(10.0, 10.0),
                Seg::Line(0.0, 10.0),
                Seg::Line(0.0, 0.0), // closing edge back to the start
                Seg::Close,
            ]
        );
    }

    #[test]
    fn contour_single_quadratic() {
        // on, off (control), on → one Quad.
        let c = [
            pt(0.0, 0.0, true),
            pt(5.0, 10.0, false),
            pt(10.0, 0.0, true),
        ];
        let mut out = Vec::new();
        contour_segs(&c, &mut out);
        assert_eq!(
            out,
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Quad(5.0, 10.0, 10.0, 0.0),
                Seg::Line(0.0, 0.0), // straight closing edge back to the start
                Seg::Close,
            ]
        );
    }

    #[test]
    fn contour_consecutive_off_curve_inserts_midpoint() {
        // on, off, off, on → the two controls imply an on-curve midpoint.
        let c = [
            pt(0.0, 0.0, true),
            pt(4.0, 8.0, false),
            pt(8.0, 8.0, false),
            pt(12.0, 0.0, true),
        ];
        let mut out = Vec::new();
        contour_segs(&c, &mut out);
        // First Quad ends at midpoint (6,8); second Quad ends at (12,0).
        assert_eq!(out[0], Seg::Move(0.0, 0.0));
        assert_eq!(out[1], Seg::Quad(4.0, 8.0, 6.0, 8.0));
        assert_eq!(out[2], Seg::Quad(8.0, 8.0, 12.0, 0.0));
        assert_eq!(*out.last().unwrap(), Seg::Close);
    }

    // --- Synthetic sfnt fixture -------------------------------------------

    fn be(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    /// A glyf entry for a rectangle `(xmin,ymin)-(xmax,ymax)`, all on-curve,
    /// padded to an even length.
    fn rect_glyph(xmin: i16, ymin: i16, xmax: i16, ymax: i16) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        g.extend_from_slice(&xmin.to_be_bytes()); // bbox (unused by parser)
        g.extend_from_slice(&ymin.to_be_bytes());
        g.extend_from_slice(&xmax.to_be_bytes());
        g.extend_from_slice(&ymax.to_be_bytes());
        g.extend_from_slice(&be(3)); // endPtsOfContours = [3]
        g.extend_from_slice(&be(0)); // instructionLength
        g.extend_from_slice(&[0x01, 0x01, 0x01, 0x01]); // all ON_CURVE, i16 deltas
        for d in [xmin, xmax - xmin, 0, xmin - xmax] {
            g.extend_from_slice(&d.to_be_bytes()); // x deltas
        }
        for d in [ymin, 0, ymax - ymin, 0] {
            g.extend_from_slice(&d.to_be_bytes()); // y deltas
        }
        if g.len() % 2 != 0 {
            g.push(0);
        }
        g
    }

    fn table_head() -> Vec<u8> {
        let mut t = vec![0u8; 54];
        t[18..20].copy_from_slice(&be(1000)); // unitsPerEm
        t[50..52].copy_from_slice(&be(0)); // indexToLocFormat = short
        t
    }

    fn table_maxp(num_glyphs: u16) -> Vec<u8> {
        let mut t = vec![0x00, 0x01, 0x00, 0x00]; // version 1.0
        t.extend_from_slice(&be(num_glyphs));
        t
    }

    /// Format-4 cmap mapping `'A'` (0x41) to glyph 1.
    fn table_cmap() -> Vec<u8> {
        let mut sub = Vec::new();
        sub.extend_from_slice(&be(4)); // format
        sub.extend_from_slice(&be(0)); // length placeholder
        sub.extend_from_slice(&be(0)); // language
        sub.extend_from_slice(&be(4)); // segCountX2 (2 segments)
        sub.extend_from_slice(&be(0)); // searchRange
        sub.extend_from_slice(&be(0)); // entrySelector
        sub.extend_from_slice(&be(0)); // rangeShift
        sub.extend_from_slice(&be(0x0041)); // endCode[0]
        sub.extend_from_slice(&be(0xFFFF)); // endCode[1]
        sub.extend_from_slice(&be(0)); // reservedPad
        sub.extend_from_slice(&be(0x0041)); // startCode[0]
        sub.extend_from_slice(&be(0xFFFF)); // startCode[1]
        sub.extend_from_slice(&be((1i32 - 0x41) as u16)); // idDelta[0] → gid 1
        sub.extend_from_slice(&be(1)); // idDelta[1]
        sub.extend_from_slice(&be(0)); // idRangeOffset[0]
        sub.extend_from_slice(&be(0)); // idRangeOffset[1]
        let len = sub.len() as u16;
        sub[2..4].copy_from_slice(&be(len));

        let mut t = Vec::new();
        t.extend_from_slice(&be(0)); // version
        t.extend_from_slice(&be(1)); // numTables
        t.extend_from_slice(&be(3)); // platformID = Windows
        t.extend_from_slice(&be(1)); // encodingID = BMP
        t.extend_from_slice(&12u32.to_be_bytes()); // subtable offset
        t.extend_from_slice(&sub);
        t
    }

    /// A `post` format-2.0 table naming glyph 1 "foo" (glyph 0 keeps the
    /// standard `.notdef` index 0, which this parser does not resolve).
    fn table_post() -> Vec<u8> {
        let mut t = Vec::new();
        t.extend_from_slice(&0x0002_0000u32.to_be_bytes()); // version 2.0
        t.extend_from_slice(&[0u8; 28]); // italicAngle..maxMemType1 (unused here)
        t.extend_from_slice(&be(2)); // numberOfGlyphs
        t.extend_from_slice(&be(0)); // glyph 0 → standard name index 0 (.notdef)
        t.extend_from_slice(&be(258)); // glyph 1 → custom names[0]
        t.push(3); // Pascal string length
        t.extend_from_slice(b"foo");
        t
    }

    /// Assembles a one-glyph (plus .notdef) sfnt with head/maxp/cmap/loca/glyf.
    pub(crate) fn build_font() -> Vec<u8> {
        let glyph1 = rect_glyph(100, 0, 600, 700);
        let glyf = glyph1.clone(); // gid 0 empty, gid 1 at offset 0
                                   // Short loca: [gid0=0, gid1=0, end=len/2].
        let mut loca = Vec::new();
        loca.extend_from_slice(&be(0));
        loca.extend_from_slice(&be(0));
        loca.extend_from_slice(&be((glyf.len() / 2) as u16));

        let tables: [(&[u8; 4], Vec<u8>); 6] = [
            (b"cmap", table_cmap()),
            (b"glyf", glyf),
            (b"head", table_head()),
            (b"loca", loca),
            (b"maxp", table_maxp(2)),
            (b"post", table_post()),
        ];

        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt version
        out.extend_from_slice(&be(tables.len() as u16));
        out.extend_from_slice(&be(0)); // searchRange
        out.extend_from_slice(&be(0)); // entrySelector
        out.extend_from_slice(&be(0)); // rangeShift

        // Lay data out after the directory, 4-byte aligned; fill offsets in.
        let dir = out.len();
        out.resize(dir + tables.len() * 16, 0);
        let mut cursor = out.len();
        for (i, (tag, data)) in tables.iter().enumerate() {
            while cursor % 4 != 0 {
                out.push(0);
                cursor += 1;
            }
            let rec = dir + i * 16;
            out[rec..rec + 4].copy_from_slice(*tag);
            out[rec + 8..rec + 12].copy_from_slice(&(cursor as u32).to_be_bytes());
            out[rec + 12..rec + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(data);
            cursor += data.len();
        }
        out
    }

    #[test]
    fn parses_synthetic_font() {
        let font = TrueType::parse(build_font()).expect("valid glyf font parses");
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.loca.len(), 3, "two glyphs plus the end sentinel");
        assert!(font.has_cmap());
    }

    #[test]
    fn maps_char_to_glyph_and_reads_outline() {
        let font = TrueType::parse(build_font()).unwrap();
        assert_eq!(font.gid_for_unicode(0x41), Some(1), "'A' maps to glyph 1");
        assert_eq!(font.gid_for_unicode(0x42), Some(0), "'B' has no glyph");

        assert!(font.glyph_path(0).is_empty(), "notdef is empty here");
        assert_eq!(
            font.glyph_path(1),
            vec![
                Seg::Move(100.0, 0.0),
                Seg::Line(600.0, 0.0),
                Seg::Line(600.0, 700.0),
                Seg::Line(100.0, 700.0),
                Seg::Line(100.0, 0.0), // closing edge (left side of the rect)
                Seg::Close,
            ]
        );
    }

    #[test]
    fn post_maps_custom_name_to_glyph() {
        let font = TrueType::parse(build_font()).expect("font parses");
        assert_eq!(font.gid_for_name("foo"), Some(1));
    }

    #[test]
    fn post_ignores_unknown_and_standard_names() {
        let font = TrueType::parse(build_font()).expect("font parses");
        assert_eq!(font.gid_for_name("bar"), None); // not in the table
        assert_eq!(font.gid_for_name(".notdef"), None); // standard index, not resolved
    }

    #[test]
    fn otto_and_garbage_are_rejected() {
        let mut otto = 0x4F54_544Fu32.to_be_bytes().to_vec();
        otto.extend_from_slice(&[0u8; 32]);
        assert!(
            TrueType::parse(otto).is_none(),
            "OTTO/CFF is not glyf-based"
        );
        assert!(TrueType::parse(vec![0, 1, 2]).is_none(), "truncated header");
    }

    /// Builds a one-page 200x200 doc showing CID 0x0001 (== glyph 1 under
    /// Identity, a filled rectangle from [`build_font`]) of an embedded
    /// `CIDFontType2`/Type0 font at 100pt, origin (20,50). The glyph paints to
    /// device x in [30,80], y in [80,150] (y is flipped). Shared by the paint
    /// test and the tier-equality test below so the 8-object builder isn't
    /// duplicated.
    fn embedded_glyph_doc_bytes() -> Vec<u8> {
        use pdfboss_testkit::PdfBuilder;

        let font_program = build_font(); // glyph 1 = a rectangle in 1000-upm units

        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        // Show CID 0x0001 (== glyph 1 under Identity) at 100pt, origin (20,50).
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <0001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X \
             /FontDescriptor 7 0 R /CIDToGIDMap /Identity /DW 1000 >>",
        );
        b.object(
            7,
            "<< /Type /FontDescriptor /FontName /X /Flags 4 /FontFile2 8 0 R >>",
        );
        b.stream(8, "", &font_program);
        b.build(1)
    }

    /// End-to-end: a page showing one CID from an embedded `CIDFontType2`
    /// Type0 font must paint the glyph's filled outline on the page.
    #[test]
    fn renders_embedded_glyph_onto_page() {
        use pdfboss_core::Document;

        let doc = Document::load(embedded_glyph_doc_bytes()).unwrap();
        let page = doc.page(0).unwrap();
        let pix = crate::render_page(&doc, &page, 1.0).unwrap();

        // The glyph maps to device x in [30,80], y in [80,150] (y is flipped).
        let dark = |x: u32, y: u32| {
            let o = ((y * pix.width + x) * 4) as usize;
            pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128
        };
        let white = |x: u32, y: u32| {
            let o = ((y * pix.width + x) * 4) as usize;
            pix.data[o] == 255 && pix.data[o + 1] == 255 && pix.data[o + 2] == 255
        };
        assert!(dark(55, 115), "glyph interior should be painted");
        assert!(white(10, 10), "top-left corner stays background");
        assert!(white(150, 170), "area away from the glyph stays background");
    }

    /// TODAY-ONLY invariant: `GlyphFont::load` receives the [`GlyphPainting`]
    /// tier but has no behavioral effect yet, so every tier must render an
    /// embedded-TrueType glyph identically to the default. This complements
    /// `executor::all_glyph_tiers_match_default_render_today`, which only
    /// exercises glyph-free path content; this test proves the comparison is
    /// meaningful by first asserting the glyph is actually painted, so the
    /// three tiers being equal isn't just two blank images matching.
    ///
    /// Once the first non-TrueType glyph loader (CFF, Type1, Type3, or font
    /// substitution) lands, `AllEmbedded`/`Full` will intentionally paint more
    /// than `EmbeddedTrueTypeOnly` for non-TrueType programs, and this "all
    /// tiers identical" assertion must be REPLACED (not preserved) to reflect
    /// that divergence.
    #[test]
    fn embedded_glyph_paints_identically_across_tiers() {
        use pdfboss_core::Document;

        let doc = Document::load(embedded_glyph_doc_bytes()).unwrap();
        let page = doc.page(0).unwrap();

        let base =
            crate::render_page_with_options(&doc, &page, 1.0, &crate::RenderOptions::default())
                .unwrap();

        // Confirm the glyph path is genuinely exercised before trusting the
        // "all tiers equal" comparison below.
        let dark = |x: u32, y: u32| {
            let o = ((y * base.width + x) * 4) as usize;
            base.data[o] < 128 && base.data[o + 1] < 128 && base.data[o + 2] < 128
        };
        assert!(dark(55, 115), "glyph interior should be painted");

        for tier in [
            crate::GlyphPainting::EmbeddedTrueTypeOnly,
            crate::GlyphPainting::AllEmbedded,
            crate::GlyphPainting::Full,
        ] {
            let opts = crate::RenderOptions {
                glyph_painting: tier,
                ..Default::default()
            };
            let got = crate::render_page_with_options(&doc, &page, 1.0, &opts).unwrap();
            assert_eq!(got, base, "tier {tier:?} differs from default render");
        }
    }
}
