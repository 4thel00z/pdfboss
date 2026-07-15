//! Type 1 font program decryption and segmentation (Type 1 Font Format spec
//! ch. 2 §PFB and ch. 7 `eexec`/charstring encryption; ISO 32000 §9.9
//! `FontFile`).
//!
//! A `FontFile` stream holds a Type1 program in one of two container shapes:
//! **PFB** (a sequence of `0x80`-tagged segments) or **raw** (plain
//! concatenation, split at the ASCII `eexec` token, optionally hex-encoded).
//! Either way, the program's tail is an `eexec`-encrypted "private" portion;
//! the same stream cipher, keyed differently, also protects each charstring
//! inside it once decrypted (a later task decrypts those with
//! `CHARSTRING_KEY`).
//!
//! This module decrypts and segments a program into its clear-text header
//! and decrypted private portion, then parses those bytes into a
//! [`Type1Font`]: `/FontMatrix`, `/Encoding`, `/Subrs`, and `/CharStrings`
//! (spec ch. 6 "Font Dictionary" and ch. 8 "Private Dictionary"; ISO 32000
//! §9.6.6.2). Interpreting each charstring into an outline is a later
//! task's job.

use std::collections::HashMap;

use crate::truetype::Seg;

// --- Type1 stream cipher constants (spec ch. 7) -----------------------------

/// The fixed key for decrypting a `FontFile`'s `eexec` portion.
const EEXEC_KEY: u16 = 55665;
/// The fixed key for decrypting an individual charstring, once extracted
/// from the decrypted `eexec` portion (consumed by a later task).
const CHARSTRING_KEY: u16 = 4330;
/// Cipher multiplier, shared by both keys' recurrences.
const C1: u16 = 52845;
/// Cipher addend, shared by both keys' recurrences.
const C2: u16 = 22719;
/// Number of scrambled lead bytes an `eexec` region always starts with.
/// (Charstrings use their own `lenIV`, conventionally also 4, read from the
/// decrypted `/Private` dict by a later task.)
const EEXEC_SKIP: usize = 4;

/// Decrypts a Type1-encrypted byte string with the stream cipher of spec
/// ch. 7: starting from `R = key`, each ciphertext byte `C` yields plaintext
/// byte `P = C ^ high_byte(R)`, after which `R` advances to
/// `(R + C) * C1 + C2` (all arithmetic wrapping `u16`, and driven by the
/// *ciphertext* byte, not the plaintext one). The cipher's first `skip`
/// emitted bytes are scrambled padding with no meaning and are dropped from
/// the result.
///
/// Returns `None` if `cipher` is shorter than `skip` -- too short to ever
/// have come from a valid `encrypt`, so there is nothing meaningful to
/// return.
fn decrypt(cipher: &[u8], key: u16, skip: usize) -> Option<Vec<u8>> {
    if cipher.len() < skip {
        return None;
    }
    let mut r = key;
    let mut plain = Vec::with_capacity(cipher.len());
    for &c in cipher {
        let p = c ^ (r >> 8) as u8;
        r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
        plain.push(p);
    }
    Some(plain.split_off(skip))
}

/// PostScript whitespace (spec ch. 2; ISO 32000 Table 1): NUL, tab, LF, FF,
/// CR, space.
fn is_ps_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// Whether `region`'s first 4 non-whitespace bytes are all ASCII hex digits
/// -- the test spec ch. 7 uses to recognize a hex-encoded (rather than raw
/// binary) `eexec` region. Fewer than 4 non-whitespace bytes available in
/// `region` cannot satisfy the check, so that case is treated as (trivially
/// malformed) binary.
fn looks_like_hex(region: &[u8]) -> bool {
    let mut seen = 0usize;
    for &b in region {
        if is_ps_whitespace(b) {
            continue;
        }
        if !b.is_ascii_hexdigit() {
            return false;
        }
        seen += 1;
        if seen == 4 {
            return true;
        }
    }
    false
}

/// Hex-decodes `region`, ignoring whitespace and stopping at the first byte
/// that is neither whitespace nor an ASCII hex digit (a hex-encoded `eexec`
/// region is conventionally wrapped across multiple lines). A trailing lone
/// nibble -- an odd count of hex digits before the stop -- is dropped rather
/// than guessed at.
fn hex_decode_lenient(region: &[u8]) -> Vec<u8> {
    let mut nibbles = Vec::with_capacity(region.len());
    for &b in region {
        if is_ps_whitespace(b) {
            continue;
        }
        match (b as char).to_digit(16) {
            Some(n) => nibbles.push(n as u8),
            None => break,
        }
    }
    nibbles
        .chunks_exact(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect()
}

/// Splits a raw (non-PFB) Type1 program at the ASCII `eexec` token:
/// `clear_text` is everything up to and including `eexec` and the single
/// whitespace separator immediately following it (a `\r\n` pair counts as
/// one separator, per spec ch. 7); the remainder is the (possibly
/// hex-encoded) `eexec` region, which is hex-decoded first when
/// [`looks_like_hex`] says so, then decrypted. `None` if the `eexec` token is
/// not present anywhere in `program`, or if decryption fails.
fn segment_raw(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    const TOKEN: &[u8] = b"eexec";
    let token_at = program.windows(TOKEN.len()).position(|w| w == TOKEN)?;
    let after_token = token_at + TOKEN.len();
    // Consume exactly ONE whitespace separator here, not a run: the eexec
    // ciphertext is arbitrary binary, so a real font's first ciphertext byte
    // may itself equal a whitespace value, and eating more than the one
    // separator the spec defines would misalign the whole decrypt window.
    let region_start = if program.get(after_token) == Some(&b'\r')
        && program.get(after_token + 1) == Some(&b'\n')
    {
        after_token + 2 // CRLF is a single line terminator
    } else if program
        .get(after_token)
        .is_some_and(|&b| is_ps_whitespace(b))
    {
        after_token + 1
    } else {
        after_token
    };
    let clear_text = program.get(..region_start)?.to_vec();
    let region = program.get(region_start..)?;

    let raw = if looks_like_hex(region) {
        hex_decode_lenient(region)
    } else {
        region.to_vec()
    };
    let priv_dec = decrypt(&raw, EEXEC_KEY, EEXEC_SKIP)?;
    Some((clear_text, priv_dec))
}

/// Walks a PFB program's `0x80 <type> <len:u32 LE>`-tagged segments (spec
/// ch. 2 §PFB), concatenating ASCII (type 1) segments into `clear_text` and
/// binary (type 2) segments into the raw `eexec` ciphertext, then decrypting
/// that ciphertext. Stops at a type-3 (EOF) segment or the first malformed
/// segment header/length -- every length is bounds-checked against what
/// remains of `program` before it is used to slice. A program with no
/// type-2 segment (nothing to decrypt) yields `None`.
fn segment_pfb(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut clear_text = Vec::new();
    let mut cipher = Vec::new();
    let mut pos = 0usize;
    while program.get(pos) == Some(&0x80) {
        let seg_type = *program.get(pos.checked_add(1)?)?;
        let len_start = pos.checked_add(2)?;
        let len_end = len_start.checked_add(4)?;
        let len = u32::from_le_bytes(program.get(len_start..len_end)?.try_into().ok()?) as usize;
        let data_start = len_end;
        let data_end = data_start.checked_add(len)?;
        let data = program.get(data_start..data_end)?;
        match seg_type {
            1 => clear_text.extend_from_slice(data),
            2 => cipher.extend_from_slice(data),
            _ => break, // type 3 (EOF), or an unrecognized/malformed type
        }
        pos = data_end;
    }
    if cipher.is_empty() {
        return None;
    }
    let priv_dec = decrypt(&cipher, EEXEC_KEY, EEXEC_SKIP)?;
    Some((clear_text, priv_dec))
}

/// Splits a `FontFile` program into its clear-text header and
/// eexec-decrypted private portion (spec ch. 2 §PFB, ch. 7; ISO 32000 §9.9
/// `FontFile`): a leading `0x80` byte marks a PFB container ([`segment_pfb`]);
/// anything else is a raw, plainly-concatenated program ([`segment_raw`]).
///
/// `None` if no `eexec` region could be found at all (a PFB program with no
/// type-2 segment, or a raw program with no `eexec` token), or if the region
/// found failed to decrypt.
fn segment(program: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if program.first() == Some(&0x80) {
        segment_pfb(program)
    } else {
        segment_raw(program)
    }
}

// --- Type1Font: parsed program (spec ch. 6, 8) ------------------------------

/// Default `/lenIV` (spec ch. 8) when the decrypted private portion doesn't
/// declare one: the number of scrambled lead bytes each charstring/subr's
/// own (`CHARSTRING_KEY`-keyed) encryption drops, distinct from the fixed
/// `EEXEC_SKIP` the outer `eexec` layer always uses.
const DEFAULT_LEN_IV: usize = 4;

/// Cap on the number of `/CharStrings` entries accepted: also the largest
/// count a `u16` gid (`Type1Font::name_to_gid`, `gid_for_name`) can address,
/// so it doubles as the defensive bound a hostile declared `/CharStrings
/// <count>` calls for (this parser never allocates proportionally to that
/// declared count in the first place -- see `parse_charstrings` -- but the
/// accepted-entry cap still bounds the work a pathological input can cause).
const MAX_GLYPHS: usize = 65_536;

/// Cap on an individual `/Subrs` index (`dup <index> ...`): real fonts use
/// small, roughly contiguous indices, so an index at or beyond this is
/// almost certainly hostile input (e.g. `dup 4000000000 ...`) rather than
/// something worth growing `subrs` to match.
const MAX_SUBR_INDEX: usize = 65_536;

/// A parsed Type 1 font: the pieces needed to map a glyph name to a
/// charstring and (for a later task) interpret that charstring into an
/// outline.
pub(crate) struct Type1Font {
    /// Decrypted charstring bytes per glyph, indexed by gid (gid 0 ==
    /// ".notdef" when the font defines it; otherwise CharStrings appearance
    /// order).
    charstrings: Vec<Vec<u8>>,
    /// gid -> glyph name, parallel to `charstrings`.
    names: Vec<String>,
    /// name -> gid.
    name_to_gid: HashMap<String, u16>,
    /// Decrypted local subroutines, indexed by subr number (gaps -> empty).
    subrs: Vec<Vec<u8>>,
    /// The font's built-in `/Encoding`: code -> glyph name (256 slots).
    builtin_encoding: Box<[Option<String>; 256]>,
    units_per_em: f32,
}

impl Type1Font {
    /// Parses a decrypted-and-segmented Type1 program: [`segment`] splits it
    /// into a clear-text header and eexec-decrypted private portion, then
    /// `/FontMatrix` and `/Encoding` are read from the header and `/lenIV`,
    /// `/Subrs`, and `/CharStrings` from the private portion.
    ///
    /// `/Encoding`'s `StandardEncoding` token form is deliberately NOT
    /// expanded into a code -> name table here: this font's caller (a later
    /// task) resolves the PDF `/Encoding` entry first and only falls back to
    /// this font's built-in encoding when the PDF gives nothing, so the
    /// built-in `StandardEncoding` case is already covered from the PDF
    /// side. Only an explicit `dup <code> /<name> put` encoding array
    /// populates `builtin_encoding` here; the bare `StandardEncoding` token
    /// leaves every slot `None`.
    ///
    /// Returns `None` if `segment` fails, or if the program yields zero
    /// charstrings (nothing paintable).
    pub(crate) fn parse(program: Vec<u8>) -> Option<Type1Font> {
        let (clear, private) = segment(&program)?;

        let units_per_em = units_per_em_from_clear(&clear);
        let builtin_encoding = parse_encoding(&clear);

        let len_iv = parse_len_iv(&private);
        let subrs = parse_subrs(&private, len_iv);
        let (charstrings, names, name_to_gid) = parse_charstrings(&private, len_iv);

        if charstrings.is_empty() {
            return None;
        }

        Some(Type1Font {
            charstrings,
            names,
            name_to_gid,
            subrs,
            builtin_encoding,
            units_per_em,
        })
    }

    /// Number of glyphs (the CharStrings entries found).
    pub(crate) fn num_glyphs(&self) -> usize {
        self.charstrings.len()
    }

    /// Maps a glyph name to a glyph index.
    pub(crate) fn gid_for_name(&self, name: &str) -> Option<u16> {
        self.name_to_gid.get(name).copied()
    }

    /// Maps a glyph index to its name -- the inverse of `gid_for_name`,
    /// needed by `glyph.rs`'s base-encoding tier to build a `unicode -> gid`
    /// map by walking every glyph name through the Adobe Glyph List (mirrors
    /// `CffFont::name_for_gid`, used the same way in `load_cff_simple`).
    /// `None` for an out-of-range gid.
    pub(crate) fn name_for_gid(&self, gid: u16) -> Option<&str> {
        self.names.get(gid as usize).map(String::as_str)
    }

    /// The font's built-in `/Encoding` name for `code` (see `parse`'s doc
    /// comment for why the `StandardEncoding` form leaves this `None`
    /// throughout).
    pub(crate) fn builtin_name(&self, code: u8) -> Option<&str> {
        self.builtin_encoding[code as usize].as_deref()
    }

    /// Font design units per em, from `/FontMatrix` (default 1000; see
    /// `units_per_em_from_clear`).
    pub(crate) fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Interprets glyph `gid`'s (already-decrypted) Type1 charstring (Type 1
    /// Font Format ch. 6.4) into outline segments in font design units.
    /// Bounds-checked and step/recursion-bounded throughout (see
    /// [`Type1Interpreter`]); a missing gid or malformed charstring yields
    /// whatever partial outline was decoded so far (empty if nothing could be
    /// decoded), never a panic.
    pub(crate) fn glyph_path(&self, gid: u16) -> Vec<Seg> {
        let Some(code) = self.charstrings.get(gid as usize) else {
            return Vec::new();
        };
        Type1Interpreter::new(self).run(code)
    }
}

// --- Type1 charstring interpreter (spec ch. 6.4) ----------------------------
//
// Turns one glyph's decrypted charstring into `Seg` outline segments. Every
// curve is cubic (`Seg::Cubic`); the "current point" starts at the origin and
// is first positioned by `hsbw`/`sbw`. Each `moveto` closes the previous
// subpath (mirroring `cff.rs`'s Type2 interpreter and the fill pipeline, which
// treats `Seg::Close` subpaths as implicitly closed -- no explicit closing
// edge is needed).

/// Type1 operand stack depth. The spec caps a charstring's operand stack at
/// 24; 48 gives headroom while still bounding a hostile push storm.
const MAX_STACK: usize = 48;

/// Bounds nested `callsubr`/`seac` recursion so a self-referential or cyclic
/// subroutine (or a self-referential `seac`) can't recurse forever or overflow
/// the real call stack (each level is one Rust stack frame).
const MAX_SUBR_DEPTH: u32 = 10;

/// Total operator-execution budget for one glyph, guarding against adversarial
/// charstrings that loop without ever deepening recursion (e.g. a long chain
/// of subr calls rather than a self-recursive one).
const MAX_STEPS: u32 = 50_000;

// One-byte operators (Type 1 Font Format ch. 6.4).
const T1_HSTEM: u8 = 1;
const T1_VSTEM: u8 = 3;
const T1_VMOVETO: u8 = 4;
const T1_RLINETO: u8 = 5;
const T1_HLINETO: u8 = 6;
const T1_VLINETO: u8 = 7;
const T1_RRCURVETO: u8 = 8;
const T1_CLOSEPATH: u8 = 9;
const T1_CALLSUBR: u8 = 10;
const T1_RETURN: u8 = 11;
const T1_ESCAPE: u8 = 12;
const T1_HSBW: u8 = 13;
const T1_ENDCHAR: u8 = 14;
const T1_RMOVETO: u8 = 21;
const T1_HMOVETO: u8 = 22;
const T1_VHCURVETO: u8 = 30;
const T1_HVCURVETO: u8 = 31;

// Escape (`12 x`) operators.
const T1E_DOTSECTION: u8 = 0;
const T1E_VSTEM3: u8 = 1;
const T1E_HSTEM3: u8 = 2;
const T1E_SEAC: u8 = 6;
const T1E_SBW: u8 = 7;
const T1E_DIV: u8 = 12;
const T1E_CALLOTHERSUBR: u8 = 16;
const T1E_POP: u8 = 17;
const T1E_SETCURRENTPOINT: u8 = 33;

/// What an executed operator (or a bounds/budget failure) tells the calling
/// decode loop to do next.
enum OpResult {
    /// Keep decoding this charstring.
    Continue,
    /// The `return` operator, or running off the end of a charstring/subr:
    /// unwind one `callsubr` level.
    Return,
    /// Stop interpreting the glyph entirely: `endchar`/`seac`, an exhausted
    /// step budget, subr recursion too deep, or a reserved/malformed opcode.
    Stop,
}

/// Interprets a single glyph's Type1 charstring, borrowing the font for its
/// (already-decrypted) local `Subrs` and -- for `seac` -- its other glyphs'
/// charstrings. Every operand read and subr-index lookup is bounds-checked;
/// malformed input makes the interpreter stop and `run` return whatever
/// outline had been decoded so far.
struct Type1Interpreter<'a> {
    font: &'a Type1Font,
    /// Type1 operand stack.
    stack: Vec<f32>,
    /// The PostScript-interpreter stack `callothersubr` pushes results onto
    /// and `pop` retrieves from.
    ps_stack: Vec<f32>,
    /// Absolute points collected during a flex (`OtherSubr 1..0`); index 0 is
    /// the flex reference point (ignored), 1..=6 are the two cubics' control
    /// and end points.
    flex_pts: Vec<(f32, f32)>,
    /// Whether a flex sequence is in progress (moveto captures a point instead
    /// of emitting `Seg::Move`).
    in_flex: bool,
    /// Current point, in the component's own (untranslated) coordinate space.
    x: f32,
    y: f32,
    /// Translation applied to every emitted coordinate: `(0, 0)` for a normal
    /// glyph or a `seac` base, `(adx - asb, ady)` for a `seac` accent.
    ox: f32,
    oy: f32,
    /// Whether a subpath is open (a `moveto` not yet followed by another
    /// `moveto`, a `closepath`, or the end of the glyph).
    open: bool,
    depth: u32,
    steps: u32,
    segs: Vec<Seg>,
}

impl<'a> Type1Interpreter<'a> {
    fn new(font: &'a Type1Font) -> Type1Interpreter<'a> {
        Type1Interpreter {
            font,
            stack: Vec::new(),
            ps_stack: Vec::new(),
            flex_pts: Vec::new(),
            in_flex: false,
            x: 0.0,
            y: 0.0,
            ox: 0.0,
            oy: 0.0,
            open: false,
            depth: 0,
            steps: 0,
            segs: Vec::new(),
        }
    }

    /// Runs the top-level charstring and returns the decoded outline, closing
    /// any subpath still open when interpretation stops (whether via
    /// `endchar` or a malformed charstring that never reaches one).
    fn run(mut self, code: &[u8]) -> Vec<Seg> {
        self.exec(code);
        self.close_current();
        self.segs
    }

    /// Local subroutine `i`'s bytes, tied to the font's lifetime (not the
    /// `&self` borrow) so the caller can pass it straight to `exec`.
    fn subr(&self, i: usize) -> Option<&'a [u8]> {
        let font: &'a Type1Font = self.font;
        font.subrs.get(i).map(Vec::as_slice)
    }

    /// Glyph `gid`'s charstring bytes, tied to the font's lifetime (see
    /// [`Self::subr`]).
    fn charstring(&self, gid: u16) -> Option<&'a [u8]> {
        let font: &'a Type1Font = self.font;
        font.charstrings.get(gid as usize).map(Vec::as_slice)
    }

    fn push_operand(&mut self, v: f32) {
        if self.stack.len() < MAX_STACK {
            self.stack.push(v);
        }
    }

    /// Operand `i`, or `0.0` if the stack came up short (a malformed
    /// charstring never panics here).
    fn arg(&self, i: usize) -> f32 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn close_current(&mut self) {
        if self.open {
            self.segs.push(Seg::Close);
            self.open = false;
        }
    }

    /// Moves the current point by `(dx, dy)`. While a flex is in progress the
    /// new point is appended to `flex_pts` rather than starting a subpath.
    fn moveto(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        if self.in_flex {
            self.flex_pts.push((self.x, self.y));
        } else {
            self.close_current();
            self.segs
                .push(Seg::Move(self.x + self.ox, self.y + self.oy));
            self.open = true;
        }
    }

    fn lineto(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.segs
            .push(Seg::Line(self.x + self.ox, self.y + self.oy));
    }

    /// Appends one cubic Bézier from three deltas relative to the current
    /// point: first control point, second control point, end point.
    fn curveto(&mut self, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
        let c1x = self.x + dx1;
        let c1y = self.y + dy1;
        let c2x = c1x + dx2;
        let c2y = c1y + dy2;
        self.x = c2x + dx3;
        self.y = c2y + dy3;
        self.segs.push(Seg::Cubic(
            c1x + self.ox,
            c1y + self.oy,
            c2x + self.ox,
            c2y + self.oy,
            self.x + self.ox,
            self.y + self.oy,
        ));
    }

    /// Decodes operators and operands from `code` until `return`/`endchar`/
    /// `seac`, the step budget is exhausted, or the bytes run out. Type1
    /// number encoding (ch. 6.2) differs from Type2: byte 255 introduces a
    /// 32-bit signed **integer**, not a 16.16 fixed value.
    fn exec(&mut self, code: &[u8]) -> OpResult {
        let mut i = 0usize;
        while i < code.len() {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return OpResult::Stop;
            }
            let b0 = code[i];
            match b0 {
                32..=246 => {
                    self.push_operand(b0 as f32 - 139.0);
                    i += 1;
                }
                247..=250 => {
                    let Some(&b1) = code.get(i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand((b0 as f32 - 247.0) * 256.0 + b1 as f32 + 108.0);
                    i += 2;
                }
                251..=254 => {
                    let Some(&b1) = code.get(i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand(-(b0 as f32 - 251.0) * 256.0 - b1 as f32 - 108.0);
                    i += 2;
                }
                255 => {
                    let Some(bytes) = code.get(i + 1..i + 5) else {
                        return OpResult::Stop;
                    };
                    let v = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    self.push_operand(v as f32);
                    i += 5;
                }
                T1_ESCAPE => {
                    let Some(&b1) = code.get(i + 1) else {
                        return OpResult::Stop;
                    };
                    i += 2;
                    match self.exec_escape(b1) {
                        OpResult::Continue => {}
                        other => return other,
                    }
                }
                _ => {
                    i += 1;
                    match self.exec_operator(b0) {
                        OpResult::Continue => {}
                        other => return other,
                    }
                }
            }
        }
        OpResult::Return
    }

    /// Handles the one-byte operators. Per Type1 semantics the operand stack
    /// is cleared after each path/hint operator (so operand counts stay
    /// correct across the glyph), but not after `callsubr`/`return`.
    fn exec_operator(&mut self, op: u8) -> OpResult {
        match op {
            T1_HSBW => {
                // sbx wx: set current point x = sbx (y = 0); wx (advance) is
                // recorded by the spec but unused for painting.
                self.x = self.arg(0);
                self.y = 0.0;
                self.stack.clear();
                OpResult::Continue
            }
            T1_RLINETO => {
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.stack.clear();
                if self.open {
                    self.lineto(dx, dy);
                }
                OpResult::Continue
            }
            T1_HLINETO => {
                let dx = self.arg(0);
                self.stack.clear();
                if self.open {
                    self.lineto(dx, 0.0);
                }
                OpResult::Continue
            }
            T1_VLINETO => {
                let dy = self.arg(0);
                self.stack.clear();
                if self.open {
                    self.lineto(0.0, dy);
                }
                OpResult::Continue
            }
            T1_RRCURVETO => {
                let (dx1, dy1, dx2, dy2, dx3, dy3) = (
                    self.arg(0),
                    self.arg(1),
                    self.arg(2),
                    self.arg(3),
                    self.arg(4),
                    self.arg(5),
                );
                self.stack.clear();
                if self.open {
                    self.curveto(dx1, dy1, dx2, dy2, dx3, dy3);
                }
                OpResult::Continue
            }
            T1_VHCURVETO => {
                // dy1 dx2 dy2 dx3: vertical start tangent, horizontal end.
                let (dy1, dx2, dy2, dx3) = (self.arg(0), self.arg(1), self.arg(2), self.arg(3));
                self.stack.clear();
                if self.open {
                    self.curveto(0.0, dy1, dx2, dy2, dx3, 0.0);
                }
                OpResult::Continue
            }
            T1_HVCURVETO => {
                // dx1 dx2 dy2 dy3: horizontal start tangent, vertical end.
                let (dx1, dx2, dy2, dy3) = (self.arg(0), self.arg(1), self.arg(2), self.arg(3));
                self.stack.clear();
                if self.open {
                    self.curveto(dx1, 0.0, dx2, dy2, 0.0, dy3);
                }
                OpResult::Continue
            }
            T1_RMOVETO => {
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.stack.clear();
                self.moveto(dx, dy);
                OpResult::Continue
            }
            T1_HMOVETO => {
                let dx = self.arg(0);
                self.stack.clear();
                self.moveto(dx, 0.0);
                OpResult::Continue
            }
            T1_VMOVETO => {
                let dy = self.arg(0);
                self.stack.clear();
                self.moveto(0.0, dy);
                OpResult::Continue
            }
            T1_CLOSEPATH => {
                // Closes the current subpath without moving the current point.
                self.close_current();
                self.stack.clear();
                OpResult::Continue
            }
            T1_HSTEM | T1_VSTEM => {
                // Hints do not affect the filled outline: clear and continue.
                self.stack.clear();
                OpResult::Continue
            }
            T1_CALLSUBR => self.callsubr(),
            T1_RETURN => OpResult::Return,
            T1_ENDCHAR => {
                self.close_current();
                self.stack.clear();
                OpResult::Stop
            }
            _ => OpResult::Stop, // reserved opcode: malformed charstring
        }
    }

    /// Handles the escape (`12 x`) operators.
    fn exec_escape(&mut self, op: u8) -> OpResult {
        match op {
            T1E_DOTSECTION | T1E_VSTEM3 | T1E_HSTEM3 => {
                // Hint operators: no effect on the filled outline.
                self.stack.clear();
                OpResult::Continue
            }
            T1E_SBW => {
                // sbx sby wx wy: set current point to (sbx, sby).
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.stack.clear();
                OpResult::Continue
            }
            T1E_SEAC => self.seac(),
            T1E_DIV => {
                // a b div -> a / b (pops exactly two, pushes the quotient;
                // does not clear the rest of the stack).
                let b = self.stack.pop().unwrap_or(1.0);
                let a = self.stack.pop().unwrap_or(0.0);
                self.push_operand(if b != 0.0 { a / b } else { 0.0 });
                OpResult::Continue
            }
            T1E_CALLOTHERSUBR => {
                self.callothersubr();
                OpResult::Continue
            }
            T1E_POP => {
                let v = self.ps_stack.pop().unwrap_or(0.0);
                self.push_operand(v);
                OpResult::Continue
            }
            T1E_SETCURRENTPOINT => {
                // x y: set the current point directly (no segment emitted).
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.stack.clear();
                OpResult::Continue
            }
            _ => {
                // Unknown escape: discard operands and continue (leniency).
                self.stack.clear();
                OpResult::Continue
            }
        }
    }

    /// `callsubr`: execute local subroutine `subr#` (no bias, unlike Type2),
    /// bounding recursion depth. A missing operand or out-of-range/absent
    /// subr is a no-op rather than an abort.
    fn callsubr(&mut self) -> OpResult {
        let Some(idx) = self.stack.pop() else {
            return OpResult::Continue;
        };
        let Ok(idx) = usize::try_from(idx as i32) else {
            return OpResult::Continue;
        };
        let Some(bytes) = self.subr(idx) else {
            return OpResult::Continue;
        };
        if self.depth >= MAX_SUBR_DEPTH {
            return OpResult::Stop;
        }
        self.depth += 1;
        let result = self.exec(bytes);
        self.depth -= 1;
        match result {
            OpResult::Stop => OpResult::Stop,
            _ => OpResult::Continue,
        }
    }

    /// The OtherSubr protocol (spec ch. 8). `callothersubr` consumes
    /// `othersubr#` (top), then `n` (arg count), then the `n` args from the
    /// operand stack. The three standard flex OtherSubrs and hint replacement
    /// (OtherSubr 3) are recognized; any other `othersubr#` is a generic
    /// passthrough that moves its `n` args onto the PS stack so subsequent
    /// `pop`s balance.
    ///
    /// `n` is attacker-controlled (a hostile charstring can push an arbitrary
    /// 32-bit value via the 255 number encoding, then an unknown
    /// `othersubr#`). It is capped at `self.stack.len()` -- there can never be
    /// more real args to pass through than operands actually on the stack --
    /// so the passthrough loop below can iterate at most `MAX_STACK` times,
    /// not up to `i32::MAX`. [`Self::push_ps`] additionally caps the PS stack
    /// itself, so no OtherSubr path (known or unknown) can grow it unbounded.
    fn callothersubr(&mut self) {
        let othersubr = self.stack.pop().unwrap_or(0.0) as i32;
        let n = (self.stack.pop().unwrap_or(0.0).max(0.0) as usize).min(self.stack.len());
        match othersubr {
            1 => {
                // Start flex: begin capturing points (there are no args).
                self.in_flex = true;
                self.flex_pts.clear();
            }
            2 => {
                // Collect flex point: a no-op here -- the point was already
                // captured by the preceding `rmoveto` while `in_flex`.
            }
            0 => {
                // End flex: args (bottom-to-top) are flex_depth, end_x, end_y.
                let end_y = self.stack.pop().unwrap_or(0.0);
                let end_x = self.stack.pop().unwrap_or(0.0);
                let _flex_depth = self.stack.pop().unwrap_or(0.0);
                self.end_flex();
                // Push end_y then end_x so the following `pop pop
                // setcurrentpoint` retrieves x first, then y.
                self.push_ps(end_y);
                self.push_ps(end_x);
            }
            _ => {
                // OtherSubr 3 (hint replacement, 1 arg subr#) and any unknown
                // OtherSubr: push the n args onto the PS stack in reverse so
                // the following `pop`s retrieve them in their original order.
                // `n` is already capped at `self.stack.len()` above, so this
                // loop runs at most `MAX_STACK` times.
                for _ in 0..n {
                    let v = self.stack.pop().unwrap_or(0.0);
                    self.push_ps(v);
                }
            }
        }
    }

    /// Pushes onto the PS-interpreter stack, capping it at [`MAX_STACK`] so
    /// that no `OtherSubr` path -- known or unknown -- can grow it without
    /// bound (defense in depth alongside the `n` cap in
    /// [`Self::callothersubr`]).
    fn push_ps(&mut self, v: f32) {
        if self.ps_stack.len() < MAX_STACK {
            self.ps_stack.push(v);
        }
    }

    /// Ends a flex sequence: emits the two cubics described by the 7 collected
    /// points (index 0 is the reference point, ignored) and leaves the current
    /// point at the flex's final point. Degrades to emitting nothing if fewer
    /// than 7 points were collected, or (mirroring the `lineto`/`curveto`
    /// leniency) if no subpath is open -- a flex with no preceding `moveto`.
    fn end_flex(&mut self) {
        self.in_flex = false;
        if self.open && self.flex_pts.len() >= 7 {
            let p = self.flex_pts.clone();
            self.segs.push(Seg::Cubic(
                p[1].0 + self.ox,
                p[1].1 + self.oy,
                p[2].0 + self.ox,
                p[2].1 + self.oy,
                p[3].0 + self.ox,
                p[3].1 + self.oy,
            ));
            self.segs.push(Seg::Cubic(
                p[4].0 + self.ox,
                p[4].1 + self.oy,
                p[5].0 + self.ox,
                p[5].1 + self.oy,
                p[6].0 + self.ox,
                p[6].1 + self.oy,
            ));
            self.x = p[6].0;
            self.y = p[6].1;
        }
        self.flex_pts.clear();
    }

    /// `seac` (spec ch. 6.4, Appendix C): `asb adx ady bchar achar` composes a
    /// StandardEncoding accented character from two other glyphs in the font.
    /// The base glyph (`bchar`) is drawn at the origin; the accent glyph
    /// (`achar`) is drawn with every coordinate translated by
    /// `(adx - asb, ady)`. Counts as one recursion level (bounded by
    /// [`MAX_SUBR_DEPTH`], so a self-referential `seac` terminates); an
    /// unresolvable component degrades to being skipped. Always terminal.
    fn seac(&mut self) -> OpResult {
        let asb = self.arg(0);
        let adx = self.arg(1);
        let ady = self.arg(2);
        let bchar = self.arg(3);
        let achar = self.arg(4);
        self.stack.clear();
        if self.depth >= MAX_SUBR_DEPTH {
            return OpResult::Stop;
        }
        self.depth += 1;
        self.run_component(bchar, 0.0, 0.0);
        self.run_component(achar, adx - asb, ady);
        self.depth -= 1;
        OpResult::Stop
    }

    /// Interprets the glyph named by StandardEncoding `code` (from a `seac`)
    /// with all output translated by `(ox, oy)`, appending its segments to the
    /// running outline. Resets the per-glyph interpreter state first (so the
    /// component starts clean) but shares the step budget, recursion depth,
    /// and segment buffer. An unknown code or missing glyph is skipped.
    fn run_component(&mut self, code: f32, ox: f32, oy: f32) {
        let Ok(code) = u8::try_from(code as i32) else {
            return;
        };
        let Some(name) = standard_encoding_name(code) else {
            return;
        };
        let Some(gid) = self.font.gid_for_name(name) else {
            return;
        };
        let Some(bytes) = self.charstring(gid) else {
            return;
        };
        // Close anything left open by a previous component, then reset the
        // per-glyph state for this one.
        self.close_current();
        self.stack.clear();
        self.ps_stack.clear();
        self.flex_pts.clear();
        self.in_flex = false;
        self.x = 0.0;
        self.y = 0.0;
        self.ox = ox;
        self.oy = oy;
        self.exec(bytes);
        // The component's own `endchar` closes its subpath; close defensively
        // in case a malformed component never reached one.
        self.close_current();
    }
}

/// The subset of Adobe StandardEncoding (spec Appendix C) needed to resolve
/// `seac` components: the Latin letters and the common accent glyphs. An
/// unrecognized code returns `None` and its component is skipped (a complete
/// StandardEncoding table is a deferred refinement).
fn standard_encoding_name(code: u8) -> Option<&'static str> {
    let name = match code {
        32 => "space",
        48 => "zero",
        49 => "one",
        50 => "two",
        51 => "three",
        52 => "four",
        53 => "five",
        54 => "six",
        55 => "seven",
        56 => "eight",
        57 => "nine",
        65 => "A",
        66 => "B",
        67 => "C",
        68 => "D",
        69 => "E",
        70 => "F",
        71 => "G",
        72 => "H",
        73 => "I",
        74 => "J",
        75 => "K",
        76 => "L",
        77 => "M",
        78 => "N",
        79 => "O",
        80 => "P",
        81 => "Q",
        82 => "R",
        83 => "S",
        84 => "T",
        85 => "U",
        86 => "V",
        87 => "W",
        88 => "X",
        89 => "Y",
        90 => "Z",
        97 => "a",
        98 => "b",
        99 => "c",
        100 => "d",
        101 => "e",
        102 => "f",
        103 => "g",
        104 => "h",
        105 => "i",
        106 => "j",
        107 => "k",
        108 => "l",
        109 => "m",
        110 => "n",
        111 => "o",
        112 => "p",
        113 => "q",
        114 => "r",
        115 => "s",
        116 => "t",
        117 => "u",
        118 => "v",
        119 => "w",
        120 => "x",
        121 => "y",
        122 => "z",
        // Accent glyphs (StandardEncoding high codes).
        193 => "grave",
        194 => "acute",
        195 => "circumflex",
        196 => "tilde",
        197 => "macron",
        198 => "breve",
        199 => "dotaccent",
        200 => "dieresis",
        202 => "ring",
        203 => "cedilla",
        205 => "hungarumlaut",
        206 => "ogonek",
        207 => "caron",
        _ => return None,
    };
    Some(name)
}

// --- private-text/clear-text tokenizing (bounds-checked throughout) --------

/// Finds the first occurrence of `needle` in `haystack`, or `None`. (A
/// `needle` longer than `haystack` simply yields no windows, not a panic.)
fn find_token(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Reads the next whitespace-delimited token at or after `i` (leading
/// whitespace is skipped first). Returns the token and the index
/// immediately following it -- which, per the grammar this module parses, is
/// where the single mandatory separator byte before a binary blob lives.
/// `None` once no token remains. Every read goes through `.get()`, so a
/// wildly out-of-range `i` just yields `None` rather than panicking.
fn next_token(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let mut p = i;
    while bytes.get(p).is_some_and(|&b| is_ps_whitespace(b)) {
        p += 1;
    }
    let start = p;
    while bytes.get(p).is_some_and(|&b| !is_ps_whitespace(b)) {
        p += 1;
    }
    if p == start {
        return None;
    }
    bytes.get(start..p).map(|tok| (tok, p))
}

/// Parses a token as a non-negative decimal integer.
fn parse_uint_token(tok: &[u8]) -> Option<usize> {
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// Parses a token as an `f64`, tolerating a leading `[` (the matrix's first
/// value is conventionally fused with its opening bracket, e.g. `[0.001`).
fn parse_matrix_number(tok: &[u8]) -> Option<f64> {
    let tok = tok.strip_prefix(b"[").unwrap_or(tok);
    std::str::from_utf8(tok).ok()?.parse().ok()
}

/// Attempts to read one `<len> RD <len bytes>` (or `-|`) binary object (spec
/// ch. 6) starting the token scan at `i`: the next token must be a decimal
/// length, the one after it the binary-read marker (`RD` or `-|`), and then
/// exactly the single separator byte the spec requires before the binary
/// data itself begins. Returns the still-`CHARSTRING_KEY`-encrypted blob
/// (truncated, not panicking, if `len` runs past the end of `bytes`) and the
/// index immediately following it. `None` if the pattern doesn't match at
/// `i` at all (no decimal length there, or no recognized marker after it) --
/// the caller is responsible for advancing the scan itself in that case.
fn read_rd_blob(bytes: &[u8], i: usize) -> Option<(&[u8], usize)> {
    let (len_tok, after_len) = next_token(bytes, i)?;
    let len = parse_uint_token(len_tok)?;
    let (marker, after_marker) = next_token(bytes, after_len)?;
    if marker != b"RD" && marker != b"-|" {
        return None;
    }
    if !bytes
        .get(after_marker)
        .is_some_and(|&b| is_ps_whitespace(b))
    {
        return None; // exactly one separator byte must follow the marker
    }
    let blob_start = after_marker.checked_add(1)?;
    let blob_end = blob_start.saturating_add(len).min(bytes.len());
    let blob = bytes.get(blob_start..blob_end)?;
    Some((blob, blob_end))
}

/// Reads `/lenIV <int> def` from the decrypted private portion (spec ch. 8),
/// defaulting to [`DEFAULT_LEN_IV`] if absent or unparsable.
fn parse_len_iv(private: &[u8]) -> usize {
    let Some(pos) = find_token(private, b"/lenIV") else {
        return DEFAULT_LEN_IV;
    };
    let after = pos.saturating_add(b"/lenIV".len());
    next_token(private, after)
        .and_then(|(tok, _)| parse_uint_token(tok))
        .unwrap_or(DEFAULT_LEN_IV)
}

/// Parses the decrypted private portion's `/Subrs <count> array` block (spec
/// ch. 8): repeated `dup <index> <len> RD <len bytes> NP` entries (the
/// terminator -- `NP`/`|`/`noaccess put` -- is never itself inspected; the
/// pattern is keyed off `<len> RD` alone, per this module's leniency
/// convention). Each blob is decrypted with `decrypt(_, CHARSTRING_KEY,
/// len_iv)`. Indexed into the result by `<index>` (gaps become empty
/// `Vec`s); an index `>= MAX_SUBR_INDEX`, or a blob that fails to decrypt, is
/// skipped rather than acted on. The scan stops at `/CharStrings` (Subrs
/// entries never appear past it) or the end of `private`, whichever comes
/// first -- this also keeps a spurious `dup` inside `/CharStrings` (there
/// shouldn't be one, but this parser is deliberately lenient) from being
/// mistaken for a Subrs entry.
fn parse_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let mut subrs: Vec<Vec<u8>> = Vec::new();
    let Some(subrs_pos) = find_token(private, b"/Subrs") else {
        return subrs;
    };
    let tail = private.get(subrs_pos..).unwrap_or(&[]);
    let scan_end = find_token(tail, b"/CharStrings")
        .map(|off| subrs_pos.saturating_add(off))
        .unwrap_or(private.len());

    let mut i = subrs_pos;
    while i < scan_end {
        let Some((tok, after_tok)) = next_token(private, i) else {
            break;
        };
        if tok != b"dup" {
            i = after_tok;
            continue;
        }
        let Some((idx_tok, after_idx)) = next_token(private, after_tok) else {
            i = after_tok;
            continue;
        };
        let Some(index) = parse_uint_token(idx_tok) else {
            i = after_idx;
            continue;
        };
        let Some((blob, end)) = read_rd_blob(private, after_idx) else {
            i = after_idx;
            continue;
        };
        if index < MAX_SUBR_INDEX {
            if index >= subrs.len() {
                subrs.resize(index + 1, Vec::new());
            }
            if let Some(decoded) = decrypt(blob, CHARSTRING_KEY, len_iv) {
                subrs[index] = decoded;
            }
        }
        i = end;
    }
    subrs
}

/// Parses the decrypted private portion's `/CharStrings <count> dict dup
/// begin` block (spec ch. 8): repeated `/<name> <len> RD <len bytes> ND`
/// entries (terminator -- `ND`/`|-`/`noaccess def` -- not itself inspected,
/// same leniency convention as [`parse_subrs`]). gid is assignment order: a
/// literal `.notdef` entry keeps its natural order rather than being forced
/// to gid 0 (the loader treats gid 0 as "not found"; a real `.notdef`
/// charstring landing elsewhere is harmless). Each blob is decrypted with
/// `decrypt(_, CHARSTRING_KEY, len_iv)`; a blob that fails to decrypt is
/// skipped. Accepts at most [`MAX_GLYPHS`] entries.
fn parse_charstrings(
    private: &[u8],
    len_iv: usize,
) -> (Vec<Vec<u8>>, Vec<String>, HashMap<String, u16>) {
    let mut charstrings: Vec<Vec<u8>> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut name_to_gid: HashMap<String, u16> = HashMap::new();

    let Some(cs_pos) = find_token(private, b"/CharStrings") else {
        return (charstrings, names, name_to_gid);
    };

    let mut i = cs_pos;
    while i < private.len() {
        if charstrings.len() >= MAX_GLYPHS {
            break;
        }
        let Some((tok, after_tok)) = next_token(private, i) else {
            break;
        };
        let Some(name_bytes) = tok.strip_prefix(b"/") else {
            i = after_tok;
            continue;
        };
        let Some((blob, end)) = read_rd_blob(private, after_tok) else {
            i = after_tok;
            continue;
        };
        let Ok(name) = std::str::from_utf8(name_bytes) else {
            i = end;
            continue;
        };
        let Some(decoded) = decrypt(blob, CHARSTRING_KEY, len_iv) else {
            i = end;
            continue;
        };
        let gid = charstrings.len() as u16; // charstrings.len() < MAX_GLYPHS <= u16::MAX + 1
        charstrings.push(decoded);
        names.push(name.to_string());
        name_to_gid.insert(name.to_string(), gid);
        i = end;
    }
    (charstrings, names, name_to_gid)
}

/// Parses the clear-text header's `/Encoding` declaration (spec ch. 6): a
/// custom encoding array's `dup <code> /<name> put` entries populate
/// `builtin_encoding[code]`. The other legal form -- the bare token
/// `StandardEncoding` -- has no such entries to find, so it (and any font
/// with no `/Encoding` at all) simply yields every slot `None`; see
/// `Type1Font::parse`'s doc comment for why that is an acceptable v1
/// simplification.
fn parse_encoding(clear: &[u8]) -> Box<[Option<String>; 256]> {
    let mut table: Box<[Option<String>; 256]> = Box::new(std::array::from_fn(|_| None));
    let Some(enc_pos) = find_token(clear, b"/Encoding") else {
        return table;
    };

    let mut i = enc_pos;
    while i < clear.len() {
        let Some((tok, after_tok)) = next_token(clear, i) else {
            break;
        };
        if tok != b"dup" {
            i = after_tok;
            continue;
        }
        let Some((code_tok, after_code)) = next_token(clear, after_tok) else {
            i = after_tok;
            continue;
        };
        let Some(code) = parse_uint_token(code_tok) else {
            i = after_code;
            continue;
        };
        let Some((name_tok, after_name)) = next_token(clear, after_code) else {
            i = after_code;
            continue;
        };
        let Some(name_bytes) = name_tok.strip_prefix(b"/") else {
            i = after_code;
            continue;
        };
        let Some((put_tok, after_put)) = next_token(clear, after_name) else {
            i = after_name;
            continue;
        };
        if put_tok != b"put" {
            i = after_name;
            continue;
        }
        if let Ok(name) = std::str::from_utf8(name_bytes) {
            if let Some(slot) = table.get_mut(code) {
                *slot = Some(name.to_string());
            }
        }
        i = after_put;
    }
    table
}

/// Computes units-per-em from the clear-text header's `/FontMatrix [a b c d
/// e f]` (spec ch. 6): `(1.0 / a).abs()`, or 1000.0 if `/FontMatrix` is
/// absent, unparsable, or `a` is zero. Mirrors the convention
/// `cff.rs::units_per_em_from_top_dict` uses for the CFF Top DICT's
/// `FontMatrix`.
fn units_per_em_from_clear(clear: &[u8]) -> f32 {
    let Some(pos) = find_token(clear, b"/FontMatrix") else {
        return 1000.0;
    };
    let after = pos.saturating_add(b"/FontMatrix".len());
    let a = next_token(clear, after).and_then(|(tok, _)| parse_matrix_number(tok));
    match a {
        Some(a) if a != 0.0 => (1.0_f64 / a).abs() as f32,
        _ => 1000.0,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::truetype::Seg;

    /// The shared cipher's inverse: same recurrence, but `C` is the byte this
    /// function itself just emitted (rather than one read from ciphertext).
    /// Prepends `skip` zero-valued filler bytes before encrypting, mirroring
    /// what `decrypt` then drops.
    fn encrypt(plain: &[u8], key: u16, skip: usize) -> Vec<u8> {
        encrypt_with_lead(plain, key, &vec![0u8; skip])
    }

    /// Like [`encrypt`], but the caller supplies the lead filler bytes
    /// explicitly instead of always zero-filling them. Lets a test pin the
    /// resulting first *ciphertext* byte to a specific value (by choosing
    /// the corresponding lead plaintext byte), since the cipher's first
    /// emitted byte depends only on `key` and `lead[0]`.
    fn encrypt_with_lead(plain: &[u8], key: u16, lead: &[u8]) -> Vec<u8> {
        let mut r = key;
        let mut out = Vec::new();
        let mut buf = lead.to_vec();
        buf.extend_from_slice(plain);
        for &p in &buf {
            let c = p ^ (r >> 8) as u8;
            r = (c as u16).wrapping_add(r).wrapping_mul(C1).wrapping_add(C2);
            out.push(c);
        }
        out
    }

    #[test]
    fn decrypt_round_trips_eexec() {
        let plain = b"/Private 10 dict dup begin";
        let cipher = encrypt(plain, EEXEC_KEY, EEXEC_SKIP);
        assert_eq!(
            decrypt(&cipher, EEXEC_KEY, EEXEC_SKIP).as_deref(),
            Some(&plain[..])
        );
    }

    #[test]
    fn decrypt_drops_skip_bytes_and_rejects_short_input() {
        // A ciphertext of exactly `skip` bytes decrypts to empty, not None.
        let cipher = encrypt(b"", CHARSTRING_KEY, 4);
        assert_eq!(
            decrypt(&cipher, CHARSTRING_KEY, 4).as_deref(),
            Some(&b""[..])
        );
        // Fewer than `skip` bytes -> None (can't be a valid encrypted object).
        assert_eq!(decrypt(&[1, 2, 3], CHARSTRING_KEY, 4), None);
    }

    #[test]
    fn decrypt_with_wrong_key_differs() {
        let plain = b"hello type1";
        let cipher = encrypt(plain, EEXEC_KEY, EEXEC_SKIP);
        assert_ne!(
            decrypt(&cipher, 12345, EEXEC_SKIP).as_deref(),
            Some(&plain[..])
        );
    }

    // --- segmentation fixture helpers --------------------------------------

    /// Builds a raw (non-PFB) Type1 program: ASCII `clear_ascii`, the
    /// `eexec` token, then `eexec_plain` encrypted with the eexec key/skip.
    fn raw_program(clear_ascii: &str, eexec_plain: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(clear_ascii.as_bytes());
        p.extend_from_slice(b"eexec\n");
        p.extend_from_slice(&encrypt(eexec_plain, EEXEC_KEY, EEXEC_SKIP));
        p
    }

    /// Builds one PFB segment: `0x80 <seg_type> <len:u32 LE> <data>`.
    fn pfb_segment(seg_type: u8, data: &[u8]) -> Vec<u8> {
        let mut s = vec![0x80, seg_type];
        s.extend_from_slice(&(data.len() as u32).to_le_bytes());
        s.extend_from_slice(data);
        s
    }

    // --- segmentation: raw, hex, PFB ----------------------------------------

    #[test]
    fn segment_raw_splits_clear_and_decrypts_eexec() {
        let prog = raw_program("%!FontType1\n/FontName /X def\n", b"/lenIV 4 def");
        let (clear, priv_dec) = segment(&prog).expect("segment");
        assert!(clear.starts_with(b"%!FontType1"));
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_raw_single_separator_does_not_eat_a_whitespace_valued_ciphertext_byte() {
        // Force the eexec region's first *ciphertext* byte (C0) to be 10
        // (0x0A, a newline value) to prove the fix consumes exactly one
        // separator after `eexec`, not a run: C0 = leadByte ^ high_byte(EEXEC_KEY)
        // = leadByte ^ 217 (EEXEC_KEY = 55665 = 0xD971, high byte 0xD9 = 217).
        // Solving leadByte ^ 217 == 10 gives leadByte == 211 (211 ^ 217 == 10).
        // `lead` supplies all EEXEC_SKIP (=4) filler bytes; only byte 0 feeds
        // C0, so the rest stay 0.
        let lead = [211u8, 0, 0, 0];
        let plain = b"/lenIV 4 def";
        let cipher = encrypt_with_lead(plain, EEXEC_KEY, &lead);
        assert_eq!(
            cipher[0], 10,
            "test setup: first ciphertext byte must itself be a whitespace (newline) value"
        );

        let mut program = Vec::new();
        program.extend_from_slice(b"%!FontType1\neexec\n");
        program.extend_from_slice(&cipher);

        // The old (buggy) run-skipping logic would treat this leading 0x0A
        // ciphertext byte as more separator whitespace and consume it too,
        // shifting the whole decrypt window by one byte and corrupting the
        // result. The fixed logic stops after the single literal `\n` that
        // follows `eexec` in the program text, leaving `cipher` untouched.
        let (_clear, priv_dec) = segment(&program).expect("segment");
        assert_eq!(&priv_dec, plain);
    }

    #[test]
    fn segment_hex_eexec_is_decoded_then_decrypted() {
        // Same content, but the eexec region is ASCII-hex instead of binary.
        let bin = encrypt(b"/lenIV 4 def", EEXEC_KEY, EEXEC_SKIP);
        let mut hex = String::new();
        for b in &bin {
            hex.push_str(&format!("{b:02x}"));
        }
        let mut prog = Vec::new();
        prog.extend_from_slice(b"%!\neexec\n");
        prog.extend_from_slice(hex.as_bytes());
        let (_clear, priv_dec) = segment(&prog).expect("segment");
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_pfb_concatenates_and_decrypts() {
        let clear = b"%!FontType1\n";
        let bin = encrypt(b"/lenIV 4 def", EEXEC_KEY, EEXEC_SKIP);
        let mut prog = pfb_segment(1, clear);
        prog.extend_from_slice(&pfb_segment(2, &bin));
        prog.extend_from_slice(&pfb_segment(3, b""));
        let (clear_out, priv_dec) = segment(&prog).expect("segment");
        assert!(clear_out.starts_with(b"%!FontType1"));
        assert_eq!(&priv_dec, b"/lenIV 4 def");
    }

    #[test]
    fn segment_without_eexec_returns_none() {
        assert!(segment(b"%!FontType1\nno private here\n").is_none());
    }

    // --- adversarial-input leniency: never panic, always None on garbage ---

    #[test]
    fn segment_pfb_truncated_length_field_returns_none() {
        // A type-2 (binary) segment marker (128 = 0x80, 2) followed by only
        // one byte (16) of what should be a 4-byte little-endian length --
        // the header is truncated before the length field is complete.
        let program = [128u8, 2, 16];
        assert!(segment(&program).is_none());
    }

    #[test]
    fn segment_pfb_length_exceeds_available_bytes_returns_none() {
        // A well-formed type-2 header declaring a 100-byte payload (little-
        // endian 100, 0, 0, 0), but the program ends right after the header
        // with zero bytes of actual data present.
        let program = [128u8, 2, 100, 0, 0, 0];
        assert!(segment(&program).is_none());
    }

    #[test]
    fn segment_empty_input_returns_none() {
        assert!(segment(&[]).is_none());
    }

    // --- Type1Font::parse fixture helpers -----------------------------------
    //
    // Charstring bytes are built from small decimal command encoders (spec
    // ch. 6.2's Type1 charstring number encoding) rather than literal hex/
    // binary blobs, per this codebase's clean-room fixture convention.

    /// Encodes one Type1 charstring number operand (spec ch. 6.2), decimal
    /// only.
    fn cs_num(out: &mut Vec<u8>, v: i32) {
        if (-107..=107).contains(&v) {
            out.push((v + 139) as u8);
        } else if (108..=1131).contains(&v) {
            let v = v - 108;
            out.push((v / 256 + 247) as u8);
            out.push((v % 256) as u8);
        } else if (-1131..=-108).contains(&v) {
            let v = -v - 108;
            out.push((v / 256 + 251) as u8);
            out.push((v % 256) as u8);
        } else {
            out.push(255);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }

    /// Encodes a one-byte Type1 charstring operator (1..31).
    fn cs_op(out: &mut Vec<u8>, op: u8) {
        out.push(op);
    }

    /// Encodes an escape (`12 x`) Type1 charstring operator.
    fn cs_escape(out: &mut Vec<u8>, op: u8) {
        out.push(12);
        out.push(op);
    }

    /// A minimal glyph: `hsbw(0,1000)` then `endchar`.
    fn stub_charstring() -> Vec<u8> {
        let mut c = Vec::new();
        cs_num(&mut c, 0);
        cs_num(&mut c, 1000);
        cs_op(&mut c, 13); // hsbw
        cs_op(&mut c, 14); // endchar
        c
    }

    /// Builds a raw (non-PFB) Type1 program with a clear-text header
    /// (`/FontMatrix`, `/Encoding`) and an eexec-encrypted private portion
    /// (`/lenIV`, `/Subrs`, `/CharStrings`), mirroring the grammar
    /// `Type1Font::parse` reads. `subrs` is `&[(index, plaintext
    /// charstring)]` (index need not be contiguous or sorted); `charstrings`
    /// is `&[(name, plaintext charstring)]`, emitted -- and so assigned gids
    /// -- in the given order. Each charstring/subr blob is independently
    /// encrypted with the charstring key/`len_iv` before being embedded in
    /// the (separately eexec-encrypted) private text, exactly as a real
    /// font nests the two ciphers.
    fn build_type1_program(
        font_matrix: &str,
        encoding: &[(u8, &str)],
        charstrings: &[(&str, Vec<u8>)],
        subrs: &[(u16, Vec<u8>)],
        len_iv: usize,
    ) -> Vec<u8> {
        let mut clear = String::new();
        clear.push_str("%!\n");
        clear.push_str(&format!("/FontMatrix {font_matrix} def\n"));
        clear.push_str("/Encoding 256 array\n");
        for (code, name) in encoding {
            clear.push_str(&format!("dup {code} /{name} put\n"));
        }

        let mut private = Vec::new();
        private.extend_from_slice(format!("/lenIV {len_iv} def\n").as_bytes());
        private.extend_from_slice(format!("/Subrs {} array\n", subrs.len()).as_bytes());
        for (index, plain) in subrs {
            let blob = encrypt(plain, CHARSTRING_KEY, len_iv);
            private.extend_from_slice(format!("dup {index} {} RD ", blob.len()).as_bytes());
            private.extend_from_slice(&blob);
            private.extend_from_slice(b" NP\n");
        }
        private.extend_from_slice(
            format!("/CharStrings {} dict dup begin\n", charstrings.len()).as_bytes(),
        );
        for (name, plain) in charstrings {
            let blob = encrypt(plain, CHARSTRING_KEY, len_iv);
            private.extend_from_slice(format!("/{name} {} RD ", blob.len()).as_bytes());
            private.extend_from_slice(&blob);
            private.extend_from_slice(b" ND\n");
        }
        private.extend_from_slice(b"end");

        raw_program(&clear, &private)
    }

    // --- Type1Font::parse ----------------------------------------------------

    #[test]
    fn parse_reads_charstrings_encoding_and_matrix() {
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[(65u8, "A"), (66, "B")],
            &[
                (".notdef", stub_charstring()),
                ("A", stub_charstring()),
                ("B", stub_charstring()),
            ],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        assert_eq!(f.num_glyphs(), 3);
        assert!(f.gid_for_name("A").is_some());
        assert!(f.gid_for_name("B").is_some());
        assert!(f.gid_for_name("nonesuch").is_none());
        assert_eq!(f.builtin_name(65), Some("A"));
        assert_eq!(f.builtin_name(66), Some("B"));
        assert_eq!(f.units_per_em(), 1000.0);
    }

    #[test]
    fn parse_reads_non_default_font_matrix() {
        let prog = build_type1_program(
            "[0.0005 0 0 0.0005 0 0]",
            &[],
            &[(".notdef", stub_charstring())],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        assert_eq!(f.units_per_em(), 2000.0);
    }

    #[test]
    fn parse_rejects_program_without_charstrings() {
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[], &[], 4);
        assert!(Type1Font::parse(prog).is_none()); // no glyphs -> not paintable
    }

    #[test]
    fn parse_tolerates_truncated_charstring_blob() {
        // Declare a length longer than the bytes actually present; parse must
        // not panic.
        let mut prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("A", stub_charstring())],
            &[],
            4,
        );
        prog.truncate(prog.len() - 3); // chop the tail
        let _ = Type1Font::parse(prog); // must return Some or None, never panic
    }

    // --- glyph_path: charstring interpretation ------------------------------

    /// hsbw(0,1000); rmoveto(100,0); rlineto(500,0); rlineto(0,700);
    /// rlineto(-500,0); closepath; endchar  -> the (100,0)-(600,700) box.
    fn box_charstring() -> Vec<u8> {
        let mut c = Vec::new();
        cs_num(&mut c, 0);
        cs_num(&mut c, 1000);
        cs_op(&mut c, 13); // hsbw
        cs_num(&mut c, 100);
        cs_num(&mut c, 0);
        cs_op(&mut c, 21); // rmoveto -> (100,0)
        cs_num(&mut c, 500);
        cs_num(&mut c, 0);
        cs_op(&mut c, 5); // rlineto -> (600,0)
        cs_num(&mut c, 0);
        cs_num(&mut c, 700);
        cs_op(&mut c, 5); // rlineto -> (600,700)
        cs_num(&mut c, -500);
        cs_num(&mut c, 0);
        cs_op(&mut c, 5); // rlineto -> (100,700)
        cs_op(&mut c, 9); // closepath
        cs_op(&mut c, 14); // endchar
        c
    }

    /// Builds a full Type1 `FontFile` program (raw, non-PFB) usable as a
    /// fixture from `glyph.rs`'s tests (mirrors how `cff::tests::
    /// build_box_glyph_fixture` is re-exported and consumed there): gid 0 is
    /// `.notdef`, gid 1 is `glyph_name`, tracing the (100,0)-(600,700) box in
    /// 1000-upm units (`box_charstring`) -- the same rectangle the CFF and
    /// TrueType fixtures trace, so the shared `dark_pixel_at(55,115)` check
    /// holds. `/FontMatrix [0.001 0 0 0.001 0 0]` and a built-in `/Encoding`
    /// array mapping code 128 -> `glyph_name` (so the tier-3 built-in-encoding
    /// resolver in `glyph.rs` has something to find when the PDF font
    /// dictionary carries no `/Encoding` of its own).
    pub(crate) fn build_type1_box_fixture(glyph_name: &str) -> Vec<u8> {
        build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[(128u8, glyph_name)],
            &[
                (".notdef", stub_charstring()),
                (glyph_name, box_charstring()),
            ],
            &[],
            4,
        )
    }

    #[test]
    fn glyph_path_decodes_rectangle() {
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("box", box_charstring())],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("box").expect("gid");
        assert_eq!(
            f.glyph_path(gid),
            vec![
                Seg::Move(100.0, 0.0),
                Seg::Line(600.0, 0.0),
                Seg::Line(600.0, 700.0),
                Seg::Line(100.0, 700.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_decodes_rrcurveto_as_cubic() {
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto -> (0,0)
                            // rrcurveto 100 0 100 100 0 100 -> ctrl (100,0),(200,100), end (200,200)
        for d in [100, 0, 100, 100, 0, 100] {
            cs_num(&mut cs, d);
        }
        cs_op(&mut cs, 8);
        cs_op(&mut cs, 14);
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[("c", cs)], &[], 4);
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("c").unwrap();
        assert_eq!(
            f.glyph_path(gid),
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Cubic(100.0, 0.0, 200.0, 100.0, 200.0, 200.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_follows_callsubr() {
        // Subr 0 draws the rlineto; the charstring calls it.
        let mut subr0 = Vec::new();
        cs_num(&mut subr0, 500);
        cs_num(&mut subr0, 0);
        cs_op(&mut subr0, 5); // rlineto
        cs_op(&mut subr0, 11); // return
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto (0,0)
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 10); // callsubr 0
        cs_op(&mut cs, 14);
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("s", cs)],
            &[(0u16, subr0)],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("s").unwrap();
        assert_eq!(
            f.glyph_path(gid),
            vec![Seg::Move(0.0, 0.0), Seg::Line(500.0, 0.0), Seg::Close]
        );
    }

    #[test]
    fn glyph_path_self_recursive_subr_terminates() {
        let mut subr0 = Vec::new();
        cs_num(&mut subr0, 0);
        cs_op(&mut subr0, 10); // callsubr 0 (infinite without a guard)
        cs_op(&mut subr0, 11);
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 10); // callsubr 0
        cs_op(&mut cs, 14);
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("g", cs)],
            &[(0u16, subr0)],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("g").unwrap();
        let _ = f.glyph_path(gid); // must return (bounded), not hang or overflow the stack
    }

    #[test]
    fn glyph_path_flex_emits_two_cubics() {
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto -> (0,0) start
                            // OtherSubr 1: start flex  (args: 0 1 callothersubr)
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 1);
        cs_escape(&mut cs, 16);
        // 7 flex points via rmoveto + OtherSubr 2, deltas summing along a shallow bump.
        // Points (absolute): p0(0,0 ref) p1(10,10) p2(20,10) p3(30,0) p4(40,-10) p5(50,-10) p6(60,0)
        let deltas = [
            (0, 0),
            (10, 10),
            (10, 0),
            (10, -10),
            (10, -10),
            (10, 0),
            (10, 10),
        ];
        for (dx, dy) in deltas {
            cs_num(&mut cs, dx);
            cs_num(&mut cs, dy);
            cs_op(&mut cs, 21); // rmoveto
            cs_num(&mut cs, 0);
            cs_num(&mut cs, 2);
            cs_escape(&mut cs, 16); // OtherSubr 2
        }
        // OtherSubr 0: end flex (args: flex_depth end_x end_y 3 0 callothersubr) then pop pop setcurrentpoint
        cs_num(&mut cs, 50);
        cs_num(&mut cs, 60);
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 3);
        cs_num(&mut cs, 0);
        cs_escape(&mut cs, 16);
        cs_escape(&mut cs, 17);
        cs_escape(&mut cs, 17);
        cs_escape(&mut cs, 33); // pop pop setcurrentpoint
        cs_op(&mut cs, 14); // endchar
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[("f", cs)], &[], 4);
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("f").unwrap();
        assert_eq!(
            f.glyph_path(gid),
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Cubic(10.0, 10.0, 20.0, 10.0, 30.0, 0.0),
                Seg::Cubic(40.0, -10.0, 50.0, -10.0, 60.0, 0.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_seac_composes_base_and_accent() {
        // 'A' (StandardEncoding 65) is the box; 'grave' (193) is a small box.
        // seac asb=0 adx=200 ady=300 bchar=65 achar=193 -> base at origin,
        // accent translated by (200,300).
        let mut acute = Vec::new();
        cs_num(&mut acute, 0);
        cs_num(&mut acute, 0);
        cs_op(&mut acute, 13); // hsbw
        cs_num(&mut acute, 0);
        cs_num(&mut acute, 0);
        cs_op(&mut acute, 21); // rmoveto (0,0)
        cs_num(&mut acute, 50);
        cs_num(&mut acute, 0);
        cs_op(&mut acute, 5); // rlineto (50,0)
        cs_op(&mut acute, 9);
        cs_op(&mut acute, 14); // closepath endchar
        let mut comp = Vec::new();
        cs_num(&mut comp, 0);
        cs_num(&mut comp, 0);
        cs_op(&mut comp, 13); // hsbw
        cs_num(&mut comp, 0);
        cs_num(&mut comp, 200);
        cs_num(&mut comp, 300);
        cs_num(&mut comp, 65);
        cs_num(&mut comp, 193);
        cs_escape(&mut comp, 6); // seac
        let prog = build_type1_program(
            "[0.001 0 0 0.001 0 0]",
            &[],
            &[("A", box_charstring()), ("grave", acute), ("Agrave", comp)],
            &[],
            4,
        );
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("Agrave").unwrap();
        let path = f.glyph_path(gid);
        // Base box present at origin, accent Line translated by (200,300).
        assert!(path.contains(&Seg::Move(100.0, 0.0))); // base
        assert!(path.contains(&Seg::Line(250.0, 300.0))); // accent (50+200, 0+300)
    }

    #[test]
    fn glyph_path_hostile_callothersubr_n_is_bounded() {
        // Adversarial-input guard for the unknown-`OtherSubr` passthrough.
        // `callothersubr` pops `othersubr#`, then `n`, then moves `n` args from
        // the operand stack onto the PS stack. `n` is fully attacker-supplied:
        // a hostile charstring can push a huge integer via the 255 number
        // encoding, then an unknown `othersubr#`, then `12 16`. The pre-fix
        // passthrough looped `for _ in 0..n { ps_stack.push(0.0) }` with `n`
        // up to ~2.1e9 into an UNCAPPED `ps_stack` and no MAX_STEPS check
        // inside -- a multi-GB allocation / hang / OOM on well-formed input.
        // The fix caps `n` at `self.stack.len()` and `push_ps` caps the PS
        // stack at MAX_STACK, so this must now return quickly and bounded.
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto -> open a subpath at (0,0)
                            // callothersubr with a hostile arg count: operand
                            // order is (args..., n, othersubr#). We push a huge
                            // `n` (2_000_000_000, encoded via the 255 form) and
                            // an unknown `othersubr#` (99), then `12 16`.
        cs_num(&mut cs, 2_000_000_000); // n (attacker-controlled, huge)
        cs_num(&mut cs, 99); // unknown othersubr# -> generic passthrough branch
        cs_escape(&mut cs, 16); // callothersubr
        cs_op(&mut cs, 14); // endchar
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[("h", cs)], &[], 4);
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("h").unwrap();
        let started = std::time::Instant::now();
        let _ = f.glyph_path(gid); // must return, not hang / OOM
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "hostile callothersubr n must be bounded, not loop ~2.1e9 times \
             pushing into an unbounded PS stack"
        );
    }

    #[test]
    fn glyph_path_large_number_uses_signed_i32_encoding() {
        // A coordinate delta of 40000 is > 1131, so `cs_num` emits it via the
        // 255 (32-bit signed big-endian INTEGER) number encoding. Asserting the
        // resulting Seg coordinate is exactly 40000.0 proves the interpreter
        // decodes byte 255 as a signed i32 -- NOT as Type2's 16.16 fixed value,
        // which would divide by 65536 and give ~0.61 instead.
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, 40000);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto -> (40000, 0)
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 40000);
        cs_op(&mut cs, 5); // rlineto -> (40000, 40000)
        cs_op(&mut cs, 14); // endchar
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[("n", cs)], &[], 4);
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("n").unwrap();
        assert_eq!(
            f.glyph_path(gid),
            vec![
                Seg::Move(40000.0, 0.0),
                Seg::Line(40000.0, 40000.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_large_negative_number_uses_signed_i32_encoding() {
        // The negative counterpart: -40000 via the 255 form must decode to
        // exactly -40000.0, confirming the sign is preserved by the i32 decode.
        let mut cs = Vec::new();
        cs_num(&mut cs, 0);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 13); // hsbw
        cs_num(&mut cs, -40000);
        cs_num(&mut cs, 0);
        cs_op(&mut cs, 21); // rmoveto -> (-40000, 0)
        cs_op(&mut cs, 14); // endchar
        let prog = build_type1_program("[0.001 0 0 0.001 0 0]", &[], &[("m", cs)], &[], 4);
        let f = Type1Font::parse(prog).expect("parse");
        let gid = f.gid_for_name("m").unwrap();
        assert_eq!(
            f.glyph_path(gid),
            vec![Seg::Move(-40000.0, 0.0), Seg::Close]
        );
    }
}
