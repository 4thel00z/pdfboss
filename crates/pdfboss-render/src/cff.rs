//! Compact Font Format (CFF) container parser: header, INDEX structures, the
//! Top and Private DICTs, the charset, the global/local subroutine indexes,
//! the width defaults, and (for CID-keyed fonts) the font-DICT array and the
//! `FDSelect` glyph-to-font-DICT map. Also interprets a glyph's Type2
//! charstring (Tech Note 5177) into [`Seg`] outline segments in font units.
//!
//! Every accessor and the interpreter itself are bounds-checked and
//! step/recursion-bounded so a malformed or adversarial embedded font yields
//! `None`/a partial outline rather than a panic or a hang.
//!
//! References: the public CFF spec (Adobe Tech Note #5176) and the Type2
//! charstring spec (Adobe Tech Note #5177).

use pdfboss_core::FastMap;

use crate::truetype::Seg;

/// A DICT is a map from operator code to its operand list. The escape
/// operator `12 x` is encoded as the key `0x0c00 | x` so both one- and
/// two-byte operators share a single numeric key space.
type Dict = FastMap<u16, Vec<f64>>;

// --- Top DICT operator keys -------------------------------------------------
const CHARSET_OP: u16 = 15;
const CHARSTRINGS_OP: u16 = 17;
const PRIVATE_OP: u16 = 18; // also the Private key inside a Font DICT
const FONT_MATRIX_OP: u16 = 0x0c00 | 7;
const ROS_OP: u16 = 0x0c00 | 30;
const FDARRAY_OP: u16 = 0x0c00 | 36;
const FDSELECT_OP: u16 = 0x0c00 | 37;

// --- Private DICT operator keys ---------------------------------------------
const PRIV_SUBRS_OP: u16 = 19;
// `defaultWidthX` (20) and `nominalWidthX` (21) are intentionally not listed
// here: `parse_private` never extracts them (see its doc comment for why),
// so the only remaining reference to those operator codes is in the test
// fixture builders below, which declare their own copies.

/// SIDs `0..=390` are reserved for the CFF standard strings regardless of how
/// many of them this module bundles; `391+` index the font's String INDEX.
const NUM_STANDARD_STRINGS: u16 = 391;

/// The complete CFF Standard Strings table (Tech Note #5176 Appendix A):
/// SID `i` (`0..=390`) names `STANDARD_STRINGS[i]`. SID 0 is `.notdef`; SIDs
/// `391+` index the font's own String INDEX instead (see `sid_for_name`).
/// Order is load-bearing -- do not resort or edit entries in place.
#[rustfmt::skip]
const STANDARD_STRINGS: &[&str] = &[
    ".notdef", "space", "exclam", "quotedbl", "numbersign", "dollar", "percent", "ampersand",
    "quoteright", "parenleft", "parenright", "asterisk", "plus", "comma", "hyphen", "period",
    "slash", "zero", "one", "two", "three", "four", "five", "six",
    "seven", "eight", "nine", "colon", "semicolon", "less", "equal", "greater",
    "question", "at", "A", "B", "C", "D", "E", "F",
    "G", "H", "I", "J", "K", "L", "M", "N",
    "O", "P", "Q", "R", "S", "T", "U", "V",
    "W", "X", "Y", "Z", "bracketleft", "backslash", "bracketright", "asciicircum",
    "underscore", "quoteleft", "a", "b", "c", "d", "e", "f",
    "g", "h", "i", "j", "k", "l", "m", "n",
    "o", "p", "q", "r", "s", "t", "u", "v",
    "w", "x", "y", "z", "braceleft", "bar", "braceright", "asciitilde",
    "exclamdown", "cent", "sterling", "fraction", "yen", "florin", "section", "currency",
    "quotesingle", "quotedblleft", "guillemotleft", "guilsinglleft", "guilsinglright", "fi", "fl", "endash",
    "dagger", "daggerdbl", "periodcentered", "paragraph", "bullet", "quotesinglbase", "quotedblbase", "quotedblright",
    "guillemotright", "ellipsis", "perthousand", "questiondown", "grave", "acute", "circumflex", "tilde",
    "macron", "breve", "dotaccent", "dieresis", "ring", "cedilla", "hungarumlaut", "ogonek",
    "caron", "emdash", "AE", "ordfeminine", "Lslash", "Oslash", "OE", "ordmasculine",
    "ae", "dotlessi", "lslash", "oslash", "oe", "germandbls", "onesuperior", "logicalnot",
    "mu", "trademark", "Eth", "onehalf", "plusminus", "Thorn", "onequarter", "divide",
    "brokenbar", "degree", "thorn", "threequarters", "twosuperior", "registered", "minus", "eth",
    "multiply", "threesuperior", "copyright", "Aacute", "Acircumflex", "Adieresis", "Agrave", "Aring",
    "Atilde", "Ccedilla", "Eacute", "Ecircumflex", "Edieresis", "Egrave", "Iacute", "Icircumflex",
    "Idieresis", "Igrave", "Ntilde", "Oacute", "Ocircumflex", "Odieresis", "Ograve", "Otilde",
    "Scaron", "Uacute", "Ucircumflex", "Udieresis", "Ugrave", "Yacute", "Ydieresis", "Zcaron",
    "aacute", "acircumflex", "adieresis", "agrave", "aring", "atilde", "ccedilla", "eacute",
    "ecircumflex", "edieresis", "egrave", "iacute", "icircumflex", "idieresis", "igrave", "ntilde",
    "oacute", "ocircumflex", "odieresis", "ograve", "otilde", "scaron", "uacute", "ucircumflex",
    "udieresis", "ugrave", "yacute", "ydieresis", "zcaron", "exclamsmall", "Hungarumlautsmall", "dollaroldstyle",
    "dollarsuperior", "ampersandsmall", "Acutesmall", "parenleftsuperior", "parenrightsuperior", "twodotenleader", "onedotenleader", "zerooldstyle",
    "oneoldstyle", "twooldstyle", "threeoldstyle", "fouroldstyle", "fiveoldstyle", "sixoldstyle", "sevenoldstyle", "eightoldstyle",
    "nineoldstyle", "commasuperior", "threequartersemdash", "periodsuperior", "questionsmall", "asuperior", "bsuperior", "centsuperior",
    "dsuperior", "esuperior", "isuperior", "lsuperior", "msuperior", "nsuperior", "osuperior", "rsuperior",
    "ssuperior", "tsuperior", "ff", "ffi", "ffl", "parenleftinferior", "parenrightinferior", "Circumflexsmall",
    "hyphensuperior", "Gravesmall", "Asmall", "Bsmall", "Csmall", "Dsmall", "Esmall", "Fsmall",
    "Gsmall", "Hsmall", "Ismall", "Jsmall", "Ksmall", "Lsmall", "Msmall", "Nsmall",
    "Osmall", "Psmall", "Qsmall", "Rsmall", "Ssmall", "Tsmall", "Usmall", "Vsmall",
    "Wsmall", "Xsmall", "Ysmall", "Zsmall", "colonmonetary", "onefitted", "rupiah", "Tildesmall",
    "exclamdownsmall", "centoldstyle", "Lslashsmall", "Scaronsmall", "Zcaronsmall", "Dieresissmall", "Brevesmall", "Caronsmall",
    "Dotaccentsmall", "Macronsmall", "figuredash", "hypheninferior", "Ogoneksmall", "Ringsmall", "Cedillasmall", "questiondownsmall",
    "oneeighth", "threeeighths", "fiveeighths", "seveneighths", "onethird", "twothirds", "zerosuperior", "foursuperior",
    "fivesuperior", "sixsuperior", "sevensuperior", "eightsuperior", "ninesuperior", "zeroinferior", "oneinferior", "twoinferior",
    "threeinferior", "fourinferior", "fiveinferior", "sixinferior", "seveninferior", "eightinferior", "nineinferior", "centinferior",
    "dollarinferior", "periodinferior", "commainferior", "Agravesmall", "Aacutesmall", "Acircumflexsmall", "Atildesmall", "Adieresissmall",
    "Aringsmall", "AEsmall", "Ccedillasmall", "Egravesmall", "Eacutesmall", "Ecircumflexsmall", "Edieresissmall", "Igravesmall",
    "Iacutesmall", "Icircumflexsmall", "Idieresissmall", "Ethsmall", "Ntildesmall", "Ogravesmall", "Oacutesmall", "Ocircumflexsmall",
    "Otildesmall", "Odieresissmall", "OEsmall", "Oslashsmall", "Ugravesmall", "Uacutesmall", "Ucircumflexsmall", "Udieresissmall",
    "Yacutesmall", "Thornsmall", "Ydieresissmall", "001.000", "001.001", "001.002", "001.003", "Black",
    "Bold", "Book", "Light", "Medium", "Regular", "Roman", "Semibold",
];

/// A parsed CFF font: the container structures needed to map glyph names or
/// CIDs to glyph indices and (for a later task) to interpret each glyph's
/// Type2 charstring.
pub(crate) struct CffFont {
    char_strings: Index,
    global_subrs: Index,
    /// The CFF String INDEX, for resolving custom glyph names (SID >= 391).
    strings: Index,
    charset: Charset,
    is_cid: bool,
    /// Non-CID: the font's single Private DICT (local subrs + widths).
    private: Option<Private>,
    /// CID-keyed: one Private DICT per font DICT, selected via `fd_select`.
    fd_array: Vec<Private>,
    fd_select: Option<FdSelect>,
    units_per_em: f32,
}

impl CffFont {
    /// Parses a bare CFF font program (no OpenType/`OTTO` wrapper). Returns
    /// `None` if the header, any INDEX, the Top DICT, or the CharStrings
    /// INDEX cannot be read.
    pub(crate) fn parse(data: Vec<u8>) -> Option<CffFont> {
        let hdr_size = *data.get(2)? as usize;
        if hdr_size > data.len() {
            return None;
        }
        let mut pos = hdr_size;

        // Header -> Name -> Top DICT -> String -> Global Subr INDEX, in order.
        let (_name_index, len) = Index::parse(&data, pos)?;
        pos += len;
        let (top_dict_index, len) = Index::parse(&data, pos)?;
        pos += len;
        let (strings, len) = Index::parse(&data, pos)?;
        pos += len;
        let (global_subrs, _) = Index::parse(&data, pos)?;

        let top = parse_dict(top_dict_index.get(0)?)?;

        let charstrings_off = first_num(&top, CHARSTRINGS_OP)? as usize;
        let (char_strings, _) = Index::parse(&data, charstrings_off)?;
        let num_glyphs = char_strings.count();

        let is_cid = top.contains_key(&ROS_OP);
        let units_per_em = units_per_em_from_top_dict(&top);

        let charset_off = first_num(&top, CHARSET_OP).unwrap_or(0.0) as usize;
        let charset = Charset::parse(&data, charset_off, num_glyphs)?;

        let (private, fd_array, fd_select) = if is_cid {
            let fd_array_off = first_num(&top, FDARRAY_OP)? as usize;
            let (fd_index, _) = Index::parse(&data, fd_array_off)?;
            let mut fds = Vec::with_capacity(fd_index.count());
            for i in 0..fd_index.count() {
                let fd_dict = parse_dict(fd_index.get(i)?)?;
                fds.push(parse_private(&data, &fd_dict)?);
            }
            let fd_select_off = first_num(&top, FDSELECT_OP)? as usize;
            let fd_select = FdSelect::parse(&data, fd_select_off, num_glyphs)?;
            (None, fds, Some(fd_select))
        } else {
            (parse_private(&data, &top), Vec::new(), None)
        };

        Some(CffFont {
            char_strings,
            global_subrs,
            strings,
            charset,
            is_cid,
            private,
            fd_array,
            fd_select,
            units_per_em,
        })
    }

    /// Number of glyphs (the CharStrings INDEX's object count).
    pub(crate) fn num_glyphs(&self) -> usize {
        self.char_strings.count()
    }

    /// Maps a glyph name to a glyph index (non-CID fonts only): name -> SID
    /// (standard strings, then the String INDEX) -> charset -> gid.
    pub(crate) fn gid_for_name(&self, name: &str) -> Option<u16> {
        if self.is_cid {
            return None; // CID-keyed fonts have no glyph names
        }
        let sid = self.sid_for_name(name)?;
        self.charset.gid_for_code(sid)
    }

    /// Maps a glyph index to its name (non-CID fonts only): charset gid ->
    /// SID, then SID -> string (the bundled standard strings for SID < 391,
    /// else the font's own String INDEX). `None` for CID-keyed fonts (no
    /// names), an out-of-range gid, or a SID this font's String INDEX
    /// doesn't cover.
    pub(crate) fn name_for_gid(&self, gid: u16) -> Option<String> {
        if self.is_cid {
            return None;
        }
        let sid = self.charset.sid_for_gid(gid)?;
        self.name_for_sid(sid)
    }

    /// Maps a CID to a glyph index (CID-keyed fonts only) via the charset,
    /// which stores gid -> CID for this font kind. `GlyphFont`'s CID loader
    /// inverts the whole charset once via `cid_to_gid` instead of looking up
    /// one CID at a time, so this single-CID lookup has no production
    /// caller yet; kept (and tested) as `gid_for_name`'s CID-keyed
    /// counterpart.
    #[allow(dead_code)]
    pub(crate) fn gid_for_cid(&self, cid: u16) -> Option<u16> {
        if !self.is_cid {
            return None;
        }
        self.charset.gid_for_code(cid)
    }

    /// Inverts the charset into a `cid -> gid` table (CID-keyed fonts only):
    /// `out[cid]` is that CID's glyph index (0/`.notdef` where no glyph
    /// claims the CID). Sized to the largest CID the charset actually uses,
    /// plus one; CIDs are `u16`, so this is bounded to 65536 entries no
    /// matter how a malformed charset is shaped. Empty for non-CID fonts.
    pub(crate) fn cid_to_gid(&self) -> Vec<u16> {
        if !self.is_cid {
            return Vec::new();
        }
        let max_cid = self.charset.codes.iter().copied().max().unwrap_or(0);
        let mut out = vec![0u16; max_cid as usize + 1];
        for (gid, &cid) in self.charset.codes.iter().enumerate() {
            // `codes.len()` (== num_glyphs) is bounded by the CharStrings
            // INDEX's u16 count, so this cast never truncates.
            if let Some(slot) = out.get_mut(cid as usize) {
                *slot = gid as u16;
            }
        }
        out
    }

    /// Font design units per em, from the Top DICT's `FontMatrix` (default
    /// `[0.001 0 0 0.001 0 0]`, i.e. 1000 units per em).
    pub(crate) fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// Interprets glyph `gid`'s Type2 charstring (Tech Note 5177) into
    /// outline segments in font design units. Bounds-checked and
    /// step/recursion-bounded throughout (see [`Type2Interpreter`]); a
    /// missing or malformed charstring yields whatever partial outline was
    /// decoded so far (empty if nothing could be decoded), never a panic.
    pub(crate) fn glyph_path(&self, gid: u16) -> Vec<Seg> {
        let Some(charstring) = self.char_strings.get(gid as usize) else {
            return Vec::new();
        };
        // Resolves the Private DICT that applies to this glyph -- for CID
        // fonts, via FDSelect(gid) -- for its local Subrs (`callsubr`). Its
        // nominalWidthX is also selected here (per the FDSelect'd Private
        // DICT) but not consumed: painting doesn't need the glyph width, only
        // that the optional leading width operand is dropped so operand
        // counts stay correct (see `Type2Interpreter::maybe_drop_width`);
        // widths for advances come from the PDF font dict, not here.
        let local_subrs = self.private_for_gid(gid).map(|p| &p.local_subrs);
        Type2Interpreter::new(&self.global_subrs, local_subrs).run(charstring)
    }

    /// The Private DICT that applies to `gid`: the font's single Private
    /// DICT for a non-CID font, or the `FDSelect`-chosen entry in `fd_array`
    /// for a CID-keyed font. `None` if there is no matching Private DICT (a
    /// bare font, or an out-of-range `FDSelect` entry).
    fn private_for_gid(&self, gid: u16) -> Option<&Private> {
        if self.is_cid {
            let fd = self.fd_select.as_ref()?.fd_for_gid(gid) as usize;
            self.fd_array.get(fd)
        } else {
            self.private.as_ref()
        }
    }

    /// Resolves a glyph name to a string id: the bundled standard-strings
    /// table first (by its real SID), then the font's own String INDEX (SID
    /// = 391 + index).
    fn sid_for_name(&self, name: &str) -> Option<u16> {
        if let Some(pos) = STANDARD_STRINGS.iter().position(|&s| s == name) {
            return Some(pos as u16);
        }
        for i in 0..self.strings.count() {
            if self.strings.get(i)? == name.as_bytes() {
                // Computed in u32 so a pathologically large String INDEX
                // can't overflow the u16 SID space; an entry whose SID
                // wouldn't fit is simply not a match (rather than panicking
                // or wrapping onto some other, wrong SID).
                let sid = u32::from(NUM_STANDARD_STRINGS) + i as u32;
                if let Ok(sid) = u16::try_from(sid) {
                    return Some(sid);
                }
            }
        }
        None
    }

    /// Resolves a SID to its glyph name: the bundled standard-strings table
    /// for `SID < 391`, otherwise the font's own String INDEX at `SID -
    /// 391`. Mirrors `sid_for_name`'s two-tier lookup in reverse.
    fn name_for_sid(&self, sid: u16) -> Option<String> {
        if sid < NUM_STANDARD_STRINGS {
            return STANDARD_STRINGS.get(sid as usize).map(|s| s.to_string());
        }
        let idx = (sid - NUM_STANDARD_STRINGS) as usize;
        self.strings
            .get(idx)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }
}

// --- Type2 charstring interpreter (Tech Note 5177) --------------------------
//
// Turns one glyph's charstring into `Seg` outline segments. Every curve is
// cubic (`Seg::Cubic`); the "current point" starts at the font origin and
// each `moveto` closes the previous subpath (mirroring the fill pipeline's
// `PathBuilder`, which treats `Seg::Close` subpaths as implicitly closed back
// to their start -- no explicit closing edge is needed).

/// Type2 operand stack depth (Tech Note 5177): pushes beyond this are simply
/// dropped rather than growing the stack without bound.
const MAX_STACK: usize = 48;

/// Bounds nested `callsubr`/`callgsubr` recursion so a self-referential or
/// cyclic subroutine can't recurse forever (or overflow the real call stack,
/// since each level is one Rust stack frame).
const MAX_SUBR_DEPTH: u32 = 10;

/// Total operator-execution budget for one glyph, guarding against
/// adversarial charstrings that loop without ever deepening recursion (e.g.
/// a long chain of subr calls rather than a self-recursive one).
const MAX_STEPS: u32 = 50_000;

// One-byte Type2 operators (Tech Note 5177 Appendix A).
const T2_HSTEM: u8 = 1;
const T2_VSTEM: u8 = 3;
const T2_VMOVETO: u8 = 4;
const T2_RLINETO: u8 = 5;
const T2_HLINETO: u8 = 6;
const T2_VLINETO: u8 = 7;
const T2_RRCURVETO: u8 = 8;
const T2_CALLSUBR: u8 = 10;
const T2_RETURN: u8 = 11;
const T2_ESCAPE: u8 = 12;
const T2_ENDCHAR: u8 = 14;
const T2_HSTEMHM: u8 = 18;
const T2_HINTMASK: u8 = 19;
const T2_CNTRMASK: u8 = 20;
const T2_RMOVETO: u8 = 21;
const T2_HMOVETO: u8 = 22;
const T2_VSTEMHM: u8 = 23;
const T2_RCURVELINE: u8 = 24;
const T2_RLINECURVE: u8 = 25;
const T2_VVCURVETO: u8 = 26;
const T2_HHCURVETO: u8 = 27;
const T2_SHORTINT: u8 = 28; // operand marker, not an operator
const T2_CALLGSUBR: u8 = 29;
const T2_VHCURVETO: u8 = 30;
const T2_HVCURVETO: u8 = 31;
const T2_FIXED: u8 = 255; // operand marker, not an operator

// Escape (`12 x`) operators.
const T2_HFLEX: u8 = 34;
const T2_FLEX: u8 = 35;
const T2_HFLEX1: u8 = 36;
const T2_FLEX1: u8 = 37;

/// Subr-index bias (Tech Note 5177): `callsubr`/`callgsubr` add this to their
/// operand before indexing the (local or global) Subrs INDEX.
fn subr_bias(n_subrs: usize) -> i32 {
    if n_subrs < 1240 {
        107
    } else if n_subrs < 33900 {
        1131
    } else {
        32768
    }
}

/// What an executed operator (or a bounds/budget failure) tells the calling
/// decode loop to do next.
enum OpResult {
    /// Keep decoding this charstring.
    Continue,
    /// The Type2 `return` operator, or simply running off the end of a
    /// charstring/subr: unwind one `callsubr`/`callgsubr` level.
    Return,
    /// Stop interpreting the glyph entirely: `endchar`, an exhausted step
    /// budget, subr recursion too deep, or a reserved/malformed opcode.
    Stop,
}

/// Interprets a single glyph's Type2 charstring, given the font's global
/// Subrs and the (already `FDSelect`-resolved, for CID fonts) local Subrs.
/// Every operand read and subr-index lookup is bounds-checked; malformed
/// input makes the interpreter stop and `run` return whatever outline had
/// been decoded so far.
struct Type2Interpreter<'a> {
    global_subrs: &'a Index,
    local_subrs: Option<&'a Index>,
    global_bias: i32,
    local_bias: i32,
    stack: Vec<f64>,
    x: f32,
    y: f32,
    /// Running count of stem hints declared so far, for `hintmask`/
    /// `cntrmask`'s `ceil(numStems/8)` mask-byte width.
    n_stems: u32,
    /// Whether the one-time optional-width operand has been looked for yet
    /// (regardless of whether one was actually present).
    width_parsed: bool,
    /// Whether a subpath is currently open (there has been a `moveto` not
    /// yet followed by another `moveto` or the end of the glyph).
    open: bool,
    depth: u32,
    steps: u32,
    segs: Vec<Seg>,
}

impl<'a> Type2Interpreter<'a> {
    fn new(global_subrs: &'a Index, local_subrs: Option<&'a Index>) -> Type2Interpreter<'a> {
        Type2Interpreter {
            global_bias: subr_bias(global_subrs.count()),
            local_bias: subr_bias(local_subrs.map(Index::count).unwrap_or(0)),
            global_subrs,
            local_subrs,
            stack: Vec::new(),
            x: 0.0,
            y: 0.0,
            n_stems: 0,
            width_parsed: false,
            open: false,
            depth: 0,
            steps: 0,
            segs: Vec::new(),
        }
    }

    /// Runs the top-level charstring and returns the decoded outline,
    /// closing any subpath still open when interpretation stops (whether via
    /// `endchar` or a malformed charstring that never reaches one).
    fn run(mut self, code: &[u8]) -> Vec<Seg> {
        self.exec(code);
        if self.open {
            self.segs.push(Seg::Close);
        }
        self.segs
    }

    fn push_operand(&mut self, v: f64) {
        if self.stack.len() < MAX_STACK {
            self.stack.push(v);
        }
    }

    /// Operand `i`, or `0.0` if the stack came up short (a malformed
    /// charstring never panics here).
    fn arg(&self, i: usize) -> f32 {
        self.stack.get(i).copied().unwrap_or(0.0) as f32
    }

    /// Drops the optional leading glyph-width operand, exactly once, the
    /// first time a stem/moveto/`hintmask`/`cntrmask`/`endchar` operator
    /// runs (Tech Note 5177's width rule). `extra` is the caller's
    /// operator-specific test for "one more operand than expected".
    fn maybe_drop_width(&mut self, extra: bool) {
        if self.width_parsed {
            return;
        }
        self.width_parsed = true;
        if extra && !self.stack.is_empty() {
            self.stack.remove(0);
        }
    }

    fn close_current(&mut self) {
        if self.open {
            self.segs.push(Seg::Close);
        }
    }

    fn moveto(&mut self, dx: f32, dy: f32) {
        self.close_current();
        self.x += dx;
        self.y += dy;
        self.segs.push(Seg::Move(self.x, self.y));
        self.open = true;
    }

    fn lineto(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.segs.push(Seg::Line(self.x, self.y));
    }

    /// Appends one cubic Bézier as three deltas from the current point:
    /// first control point, second control point, end point.
    fn curveto(&mut self, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
        let c1x = self.x + dx1;
        let c1y = self.y + dy1;
        let c2x = c1x + dx2;
        let c2y = c1y + dy2;
        self.x = c2x + dx3;
        self.y = c2y + dy3;
        self.segs
            .push(Seg::Cubic(c1x, c1y, c2x, c2y, self.x, self.y));
    }

    /// Decodes operators and operands from `code` until `return`/`endchar`,
    /// the step budget is exhausted, or the bytes run out.
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
                    self.push_operand(b0 as f64 - 139.0);
                    i += 1;
                }
                247..=250 => {
                    let Some(&b1) = code.get(i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand((b0 as f64 - 247.0) * 256.0 + b1 as f64 + 108.0);
                    i += 2;
                }
                251..=254 => {
                    let Some(&b1) = code.get(i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand(-(b0 as f64 - 251.0) * 256.0 - b1 as f64 - 108.0);
                    i += 2;
                }
                T2_SHORTINT => {
                    let Some(v) = bei16(code, i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand(v as f64);
                    i += 3;
                }
                T2_FIXED => {
                    let Some(v) = bei32(code, i + 1) else {
                        return OpResult::Stop;
                    };
                    self.push_operand(v as f64 / 65536.0);
                    i += 5;
                }
                T2_HINTMASK | T2_CNTRMASK => {
                    // Any pending stem operands implicitly declare a final
                    // vstem hint before the mask (Tech Note 5177); the
                    // width-detection rule treats this the same as an
                    // explicit stem-hint operator.
                    self.maybe_drop_width(self.stack.len() % 2 == 1);
                    self.n_stems += (self.stack.len() / 2) as u32;
                    self.stack.clear();
                    let mask_bytes = (self.n_stems as usize).div_ceil(8);
                    let Some(after_op) = i.checked_add(1) else {
                        return OpResult::Stop;
                    };
                    let Some(end) = after_op.checked_add(mask_bytes) else {
                        return OpResult::Stop;
                    };
                    if end > code.len() {
                        return OpResult::Stop;
                    }
                    i = end;
                }
                T2_ESCAPE => {
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

    /// Handles every one-byte operator except `hintmask`/`cntrmask` (which
    /// `exec` handles directly, since they consume extra bytes from `code`
    /// itself rather than from the operand stack).
    fn exec_operator(&mut self, op: u8) -> OpResult {
        match op {
            T2_HSTEM | T2_VSTEM | T2_HSTEMHM | T2_VSTEMHM => {
                self.maybe_drop_width(self.stack.len() % 2 == 1);
                self.n_stems += (self.stack.len() / 2) as u32;
                self.stack.clear();
                OpResult::Continue
            }
            T2_RMOVETO => {
                self.maybe_drop_width(self.stack.len() > 2);
                let (dx, dy) = (self.arg(0), self.arg(1));
                self.stack.clear();
                self.moveto(dx, dy);
                OpResult::Continue
            }
            T2_HMOVETO => {
                self.maybe_drop_width(self.stack.len() > 1);
                let dx = self.arg(0);
                self.stack.clear();
                self.moveto(dx, 0.0);
                OpResult::Continue
            }
            T2_VMOVETO => {
                self.maybe_drop_width(self.stack.len() > 1);
                let dy = self.arg(0);
                self.stack.clear();
                self.moveto(0.0, dy);
                OpResult::Continue
            }
            T2_RLINETO => {
                let args = std::mem::take(&mut self.stack);
                for pair in args.chunks_exact(2) {
                    self.lineto(pair[0] as f32, pair[1] as f32);
                }
                OpResult::Continue
            }
            T2_HLINETO | T2_VLINETO => {
                let args = std::mem::take(&mut self.stack);
                let mut horiz = op == T2_HLINETO;
                for &v in &args {
                    if horiz {
                        self.lineto(v as f32, 0.0);
                    } else {
                        self.lineto(0.0, v as f32);
                    }
                    horiz = !horiz;
                }
                OpResult::Continue
            }
            T2_RRCURVETO => {
                let args = std::mem::take(&mut self.stack);
                for c in args.chunks_exact(6) {
                    self.curveto(
                        c[0] as f32,
                        c[1] as f32,
                        c[2] as f32,
                        c[3] as f32,
                        c[4] as f32,
                        c[5] as f32,
                    );
                }
                OpResult::Continue
            }
            T2_VVCURVETO => {
                self.exec_vvcurveto();
                OpResult::Continue
            }
            T2_HHCURVETO => {
                self.exec_hhcurveto();
                OpResult::Continue
            }
            T2_VHCURVETO => {
                self.exec_alternating_curve(false);
                OpResult::Continue
            }
            T2_HVCURVETO => {
                self.exec_alternating_curve(true);
                OpResult::Continue
            }
            T2_RCURVELINE => {
                let args = std::mem::take(&mut self.stack);
                if args.len() >= 2 {
                    let (curves, line) = args.split_at(args.len() - 2);
                    for c in curves.chunks_exact(6) {
                        self.curveto(
                            c[0] as f32,
                            c[1] as f32,
                            c[2] as f32,
                            c[3] as f32,
                            c[4] as f32,
                            c[5] as f32,
                        );
                    }
                    self.lineto(line[0] as f32, line[1] as f32);
                }
                OpResult::Continue
            }
            T2_RLINECURVE => {
                let args = std::mem::take(&mut self.stack);
                if args.len() >= 6 {
                    let (lines, curve) = args.split_at(args.len() - 6);
                    for pair in lines.chunks_exact(2) {
                        self.lineto(pair[0] as f32, pair[1] as f32);
                    }
                    self.curveto(
                        curve[0] as f32,
                        curve[1] as f32,
                        curve[2] as f32,
                        curve[3] as f32,
                        curve[4] as f32,
                        curve[5] as f32,
                    );
                }
                OpResult::Continue
            }
            T2_CALLSUBR => {
                let Some(idx) = self.stack.pop() else {
                    return OpResult::Continue;
                };
                let biased = idx as i32 + self.local_bias;
                self.call_subr(self.local_subrs, biased)
            }
            T2_CALLGSUBR => {
                let Some(idx) = self.stack.pop() else {
                    return OpResult::Continue;
                };
                let biased = idx as i32 + self.global_bias;
                self.call_subr(Some(self.global_subrs), biased)
            }
            T2_RETURN => OpResult::Return,
            T2_ENDCHAR => {
                // A 4-operand endchar is the deprecated `seac` accent-
                // composition form (Tech Note 5177): compose two standard-
                // encoding glyphs. Not implemented -- its extra operands are
                // simply discarded, i.e. treated as a plain endchar.
                let extra = self.stack.len() == 1 || self.stack.len() == 5;
                self.maybe_drop_width(extra);
                self.stack.clear();
                OpResult::Stop
            }
            _ => OpResult::Stop, // reserved opcode: malformed charstring
        }
    }

    /// Handles the escape (`12 x`) operators: the flex family. Any other
    /// escape operator (the Type2 arithmetic/storage operators, e.g. `and`,
    /// `put`, `ifelse`) is not implemented -- vanishingly rare outside
    /// deprecated hint-replacement schemes -- so its operands are discarded
    /// and decoding continues rather than aborting the glyph.
    fn exec_escape(&mut self, op: u8) -> OpResult {
        match op {
            T2_HFLEX => self.exec_hflex(),
            T2_FLEX => self.exec_flex(),
            T2_HFLEX1 => self.exec_hflex1(),
            T2_FLEX1 => self.exec_flex1(),
            _ => self.stack.clear(),
        }
        OpResult::Continue
    }

    /// `vvcurveto`: `dx1? {dya dxb dyb dyc}+` -- vertical-tangent curves; an
    /// optional leading `dx1` only offsets the very first curve's first
    /// control point.
    fn exec_vvcurveto(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let mut i = 0;
        let mut lead_dx = 0.0f32;
        if args.len() % 4 == 1 {
            lead_dx = args[0] as f32;
            i = 1;
        }
        while i + 4 <= args.len() {
            let (dya, dxb, dyb, dyc) = (
                args[i] as f32,
                args[i + 1] as f32,
                args[i + 2] as f32,
                args[i + 3] as f32,
            );
            self.curveto(lead_dx, dya, dxb, dyb, 0.0, dyc);
            lead_dx = 0.0;
            i += 4;
        }
    }

    /// `hhcurveto`: `dy1? {dxa dxb dyb dxc}+` -- horizontal-tangent curves;
    /// an optional leading `dy1` only offsets the very first curve's first
    /// control point.
    fn exec_hhcurveto(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let mut i = 0;
        let mut lead_dy = 0.0f32;
        if args.len() % 4 == 1 {
            lead_dy = args[0] as f32;
            i = 1;
        }
        while i + 4 <= args.len() {
            let (dxa, dxb, dyb, dxc) = (
                args[i] as f32,
                args[i + 1] as f32,
                args[i + 2] as f32,
                args[i + 3] as f32,
            );
            self.curveto(dxa, lead_dy, dxb, dyb, dxc, 0.0);
            lead_dy = 0.0;
            i += 4;
        }
    }

    /// `hvcurveto`/`vhcurveto`: curves whose start tangent alternates
    /// horizontal/vertical each curve (`start_horizontal` picks which one
    /// leads). A final fifth operand on the last curve, when present,
    /// becomes that curve's otherwise-zero cross-axis endpoint delta.
    fn exec_alternating_curve(&mut self, start_horizontal: bool) {
        let args = std::mem::take(&mut self.stack);
        let n = args.len();
        let mut i = 0;
        let mut horiz = start_horizontal;
        while i + 4 <= n {
            let remaining = n - i;
            let extra = if remaining == 5 {
                Some(args[i + 4] as f32)
            } else {
                None
            };
            let (v0, v1, v2, v3) = (
                args[i] as f32,
                args[i + 1] as f32,
                args[i + 2] as f32,
                args[i + 3] as f32,
            );
            if horiz {
                self.curveto(v0, 0.0, v1, v2, extra.unwrap_or(0.0), v3);
            } else {
                self.curveto(0.0, v0, v1, v2, v3, extra.unwrap_or(0.0));
            }
            horiz = !horiz;
            i += if extra.is_some() { 5 } else { 4 };
        }
    }

    /// `hflex` (`12 34`): `dx1 dx2 dy2 dx3 dx4 dx5 dx6` -- two curves whose y
    /// returns to the starting value (the flex's whole point is a nearly-
    /// horizontal join), so only `dy2` ever moves off it and the second
    /// curve undoes that with `-dy2`.
    fn exec_hflex(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0) as f32;
        let (dx1, dx2, dy2, dx3, dx4, dx5, dx6) = (a(0), a(1), a(2), a(3), a(4), a(5), a(6));
        self.curveto(dx1, 0.0, dx2, dy2, dx3, 0.0);
        self.curveto(dx4, 0.0, dx5, -dy2, dx6, 0.0);
    }

    /// `flex` (`12 35`): `dx1 dy1 dx2 dy2 dx3 dy3 dx4 dy4 dx5 dy5 dx6 dy6 fd`
    /// -- two ordinary curves; `fd` is a flex-height selection hint with no
    /// effect on the geometry, so it is read (for stack bookkeeping) and
    /// otherwise ignored.
    fn exec_flex(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0) as f32;
        self.curveto(a(0), a(1), a(2), a(3), a(4), a(5));
        self.curveto(a(6), a(7), a(8), a(9), a(10), a(11));
    }

    /// `hflex1` (`12 36`): `dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6` -- like
    /// `flex`, but the final curve's `dy6` is solved for so the whole flex's
    /// net vertical displacement is zero (`y` returns to its starting
    /// value).
    fn exec_hflex1(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0) as f32;
        let (dx1, dy1, dx2, dy2, dx3, dx4, dx5, dy5, dx6) =
            (a(0), a(1), a(2), a(3), a(4), a(5), a(6), a(7), a(8));
        let dy6 = -(dy1 + dy2 + dy5);
        self.curveto(dx1, dy1, dx2, dy2, dx3, 0.0);
        self.curveto(dx4, 0.0, dx5, dy5, dx6, dy6);
    }

    /// `flex1` (`12 37`): `dx1 dy1 dx2 dy2 dx3 dy3 dx4 dy4 dx5 dy5 d6` --
    /// like `hflex1`, but generalized to whichever axis moved more over the
    /// first five deltas: that axis's final delta is the explicit `d6`, and
    /// the other axis is solved for so it returns to its starting value.
    fn exec_flex1(&mut self) {
        let args = std::mem::take(&mut self.stack);
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0) as f32;
        let (dx1, dy1, dx2, dy2, dx3, dy3, dx4, dy4, dx5, dy5, d6) = (
            a(0),
            a(1),
            a(2),
            a(3),
            a(4),
            a(5),
            a(6),
            a(7),
            a(8),
            a(9),
            a(10),
        );
        let dx_sum = dx1 + dx2 + dx3 + dx4 + dx5;
        let dy_sum = dy1 + dy2 + dy3 + dy4 + dy5;
        let (dx6, dy6) = if dx_sum.abs() > dy_sum.abs() {
            (d6, -dy_sum)
        } else {
            (-dx_sum, d6)
        };
        self.curveto(dx1, dy1, dx2, dy2, dx3, dy3);
        self.curveto(dx4, dy4, dx5, dy5, dx6, dy6);
    }

    /// Calls a bias-resolved subroutine index, bounding recursion depth.
    /// Leniently does nothing if `idx` is absent, the index is negative, or
    /// it is out of range for the Subrs INDEX -- a missing subr is treated
    /// as a no-op rather than aborting the glyph.
    fn call_subr(&mut self, idx: Option<&'a Index>, biased: i32) -> OpResult {
        let Some(idx) = idx else {
            return OpResult::Continue;
        };
        let Ok(biased) = usize::try_from(biased) else {
            return OpResult::Continue;
        };
        let Some(bytes) = idx.get(biased) else {
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
}

/// Computes units-per-em from the Top DICT's `FontMatrix` operand (`1 /
/// matrix[0]`), defaulting to 1000 when absent, malformed, or degenerate.
fn units_per_em_from_top_dict(top: &Dict) -> f32 {
    top.get(&FONT_MATRIX_OP)
        .filter(|m| m.len() == 6 && m[0] != 0.0)
        .map(|m| (1.0 / m[0]) as f32)
        .unwrap_or(1000.0)
}

/// Reads a Private DICT (found via the DICT's `Private` operator, `[size,
/// offset]`) and its local Subrs INDEX (offset relative to the Private
/// DICT's own start). Works for both the non-CID Top DICT and a CID font's
/// per-glyph Font DICT, since both use the same operator.
///
/// The Private DICT's `defaultWidthX`/`nominalWidthX` operands are
/// deliberately not extracted: as `CffFont::glyph_path` documents, glyph
/// widths for painting come from the PDF font dict's `/Widths` (or `/W`/
/// `/DW`), never from the CFF program, so those two operands would only ever
/// be parsed and then never read.
fn parse_private(data: &[u8], dict: &Dict) -> Option<Private> {
    let v = dict.get(&PRIVATE_OP)?;
    if v.len() != 2 {
        return None;
    }
    let size = *v.first()? as usize;
    let offset = *v.get(1)? as usize;
    let bytes = data.get(offset..offset.checked_add(size)?)?;
    let pd = parse_dict(bytes)?;

    let local_subrs = match first_num(&pd, PRIV_SUBRS_OP) {
        Some(rel_off) => {
            let subrs_off = offset.checked_add(rel_off as usize)?;
            Index::parse(data, subrs_off)
                .map(|(idx, _)| idx)
                .unwrap_or_default()
        }
        None => Index::default(),
    };
    Some(Private { local_subrs })
}

/// The first operand of `key`, if `dict` has an entry for it.
fn first_num(dict: &Dict, key: u16) -> Option<f64> {
    dict.get(&key)?.first().copied()
}

/// A font's (or, for CID-keyed fonts, one font DICT's) local resources: the
/// local Subrs INDEX used by `callsubr`.
struct Private {
    local_subrs: Index,
}

/// A CFF INDEX: a count-prefixed array of variable-length byte objects (Tech
/// Note 5176 §5).
#[derive(Default)]
struct Index {
    /// Concatenated object data, sliced out of the font at parse time.
    data: Vec<u8>,
    /// Byte offsets into `data`, `count + 1` long: object *i* spans
    /// `[offsets[i], offsets[i+1])`.
    offsets: Vec<usize>,
}

impl Index {
    /// Number of objects in the index.
    fn count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Object `i`'s bytes, or `None` if out of range.
    fn get(&self, i: usize) -> Option<&[u8]> {
        let start = *self.offsets.get(i)?;
        let end = *self.offsets.get(i + 1)?;
        self.data.get(start..end)
    }

    /// Parses an INDEX starting at byte `at`, returning the parsed index and
    /// the number of bytes it consumes (so callers can walk to whatever
    /// structure follows it). Bounds-checked; `None` on malformed input.
    fn parse(data: &[u8], at: usize) -> Option<(Index, usize)> {
        let count = be16(data, at)? as usize;
        if count == 0 {
            return Some((Index::default(), 2));
        }
        let off_size = *data.get(at.checked_add(2)?)? as usize;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets_start = at.checked_add(3)?;
        // Offsets are 1-based, relative to the byte before the object data.
        let mut offsets = Vec::with_capacity(count + 1);
        for i in 0..=count {
            let o = offsets_start.checked_add(i.checked_mul(off_size)?)?;
            let raw = read_uint(data, o, off_size)?;
            offsets.push(raw.checked_sub(1)?);
        }
        if offsets.windows(2).any(|w| w[0] > w[1]) {
            return None; // offsets must be non-decreasing
        }
        let count_plus_one = count.checked_add(1)?;
        let data_start = offsets_start.checked_add(count_plus_one.checked_mul(off_size)?)?;
        let data_len = *offsets.last()?;
        let data_end = data_start.checked_add(data_len)?;
        let obj_data = data.get(data_start..data_end)?.to_vec();
        let consumed = data_end.checked_sub(at)?;
        Some((
            Index {
                data: obj_data,
                offsets,
            },
            consumed,
        ))
    }
}

/// Maps between a glyph index and either a name SID (non-CID fonts) or a CID
/// (CID-keyed fonts) — the CFF `charset` table. Glyph 0 is always `.notdef`
/// (SID/CID 0), which the on-disk table leaves implicit.
struct Charset {
    /// `codes[gid]` is the SID/CID for that glyph; `codes[0]` is unused
    /// (`.notdef` is handled separately in `gid_for_code`).
    codes: Vec<u16>,
}

impl Charset {
    /// Parses a charset at `offset` for a font with `num_glyphs` glyphs.
    /// `offset` values 0/1/2 select the predefined ISOAdobe/Expert/
    /// ExpertSubset charsets; anything else is a byte offset to formats 0/1/2.
    fn parse(data: &[u8], offset: usize, num_glyphs: usize) -> Option<Charset> {
        let mut codes = vec![0u16; num_glyphs];
        if offset <= 2 {
            // Offset 0 (ISOAdobe) is exactly SID == GID for glyphs 1... Offsets
            // 1/2 (Expert/ExpertSubset) have their own fixed SID lists (Tech
            // Note 5176 Appendix C) not bundled here; approximated as identity
            // too until a real font exercising them shows up.
            for (gid, slot) in codes.iter_mut().enumerate().skip(1) {
                *slot = gid as u16;
            }
            return Some(Charset { codes });
        }
        let format = *data.get(offset)?;
        let mut gid = 1usize;
        let mut p = offset.checked_add(1)?;
        match format {
            0 => {
                while gid < num_glyphs {
                    codes[gid] = be16(data, p)?;
                    p = p.checked_add(2)?;
                    gid += 1;
                }
            }
            1 => {
                while gid < num_glyphs {
                    let first = be16(data, p)?;
                    let n_left = *data.get(p.checked_add(2)?)? as u16;
                    p = p.checked_add(3)?;
                    for k in 0..=n_left {
                        if gid >= num_glyphs {
                            break;
                        }
                        // `first`/`k` are raw font values; a malformed range
                        // can make their sum exceed u16::MAX, so saturate
                        // instead of overflow-panicking.
                        codes[gid] = first.saturating_add(k);
                        gid += 1;
                    }
                }
            }
            2 => {
                while gid < num_glyphs {
                    let first = be16(data, p)?;
                    let n_left = be16(data, p.checked_add(2)?)?;
                    p = p.checked_add(4)?;
                    for k in 0..=n_left {
                        if gid >= num_glyphs {
                            break;
                        }
                        // See the format-1 arm: saturate rather than panic on
                        // a malformed range.
                        codes[gid] = first.saturating_add(k);
                        gid += 1;
                    }
                }
            }
            _ => return None,
        }
        Some(Charset { codes })
    }

    /// The glyph index for a SID/CID. `.notdef` (0) is always glyph 0; other
    /// codes are found by a linear scan (charsets are small).
    fn gid_for_code(&self, code: u16) -> Option<u16> {
        if code == 0 {
            return Some(0);
        }
        self.codes.iter().position(|&c| c == code).map(|g| g as u16)
    }

    /// The SID/CID for `gid` (the inverse of `gid_for_code`), or `None` if
    /// `gid` is out of range. `.notdef` (gid 0) always resolves to code 0,
    /// which is how the on-disk charset already represents it (`codes[0]`
    /// is left at its zero-initialized default -- see `parse`).
    fn sid_for_gid(&self, gid: u16) -> Option<u16> {
        self.codes.get(gid as usize).copied()
    }
}

/// CID-keyed fonts' `FDSelect`: maps a glyph index to the index of its font
/// DICT in `FDArray` (each font DICT has its own Private DICT, hence its own
/// local Subrs and `nominalWidthX`).
struct FdSelect {
    /// `fd_for_gid[gid]` is the font-DICT index for that glyph.
    fd_for_gid: Vec<u8>,
}

impl FdSelect {
    fn parse(data: &[u8], offset: usize, num_glyphs: usize) -> Option<FdSelect> {
        let format = *data.get(offset)?;
        let mut fd_for_gid = vec![0u8; num_glyphs];
        match format {
            0 => {
                for (gid, slot) in fd_for_gid.iter_mut().enumerate() {
                    *slot = *data.get(offset.checked_add(1)?.checked_add(gid)?)?;
                }
            }
            3 => {
                let n_ranges = be16(data, offset.checked_add(1)?)? as usize;
                let ranges_start = offset.checked_add(3)?;
                let sentinel_pos = ranges_start.checked_add(n_ranges.checked_mul(3)?)?;
                let sentinel = be16(data, sentinel_pos)? as usize;
                for r in 0..n_ranges {
                    let rec = ranges_start.checked_add(r.checked_mul(3)?)?;
                    let first = be16(data, rec)? as usize;
                    let fd = *data.get(rec.checked_add(2)?)?;
                    let next = if r + 1 < n_ranges {
                        be16(data, ranges_start.checked_add((r + 1).checked_mul(3)?)?)? as usize
                    } else {
                        sentinel
                    };
                    for gid in first..next.min(num_glyphs) {
                        if let Some(slot) = fd_for_gid.get_mut(gid) {
                            *slot = fd;
                        }
                    }
                }
            }
            _ => return None,
        }
        Some(FdSelect { fd_for_gid })
    }

    /// The font-DICT index selecting `gid`'s local Subrs/widths.
    fn fd_for_gid(&self, gid: u16) -> u8 {
        self.fd_for_gid.get(gid as usize).copied().unwrap_or(0)
    }
}

/// Decodes a DICT byte stream into operator -> operands. Every read is
/// bounds-checked; malformed input (an unrecognized lead byte, a truncated
/// operand) yields `None`. The loop always advances `i` by at least one byte,
/// so it is bounded by `data.len()` with no separate step counter needed.
fn parse_dict(data: &[u8]) -> Option<Dict> {
    let mut out = FastMap::default();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let b0 = data[i];
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    let b1 = *data.get(i + 1)?;
                    i += 2;
                    0x0c00 | b1 as u16
                } else {
                    i += 1;
                    b0 as u16
                };
                out.insert(op, std::mem::take(&mut operands));
            }
            28 => {
                operands.push(bei16(data, i + 1)? as f64);
                i += 3;
            }
            29 => {
                operands.push(bei32(data, i + 1)? as f64);
                i += 5;
            }
            30 => {
                let (val, len) = parse_real(data, i + 1)?;
                operands.push(val);
                i += 1 + len;
            }
            32..=246 => {
                operands.push(b0 as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                let b1 = *data.get(i + 1)? as f64;
                operands.push((b0 as f64 - 247.0) * 256.0 + b1 + 108.0);
                i += 2;
            }
            251..=254 => {
                let b1 = *data.get(i + 1)? as f64;
                operands.push(-(b0 as f64 - 251.0) * 256.0 - b1 - 108.0);
                i += 2;
            }
            _ => return None, // 22..=27, 31, 255 are reserved in a DICT
        }
    }
    Some(out)
}

/// Decodes a DICT real-number operand (packed BCD nibbles, byte 30's payload,
/// terminated by nibble `0xf`). Returns the value and the number of bytes
/// consumed. Bounded by `data.len()`: each iteration reads one more byte.
fn parse_real(data: &[u8], start: usize) -> Option<(f64, usize)> {
    let mut s = String::new();
    let mut i = start;
    loop {
        let byte = *data.get(i)?;
        i += 1;
        for nibble in [byte >> 4, byte & 0xf] {
            match nibble {
                0..=9 => s.push((b'0' + nibble) as char),
                0xa => s.push('.'),
                0xb => s.push('E'),
                0xc => s.push_str("E-"),
                0xe => s.push('-'),
                0xf => return Some((s.parse().ok()?, i - start)),
                _ => return None, // 0xd is reserved
            }
        }
    }
}

// --- big-endian readers (all bounds-checked) --------------------------------

/// All offsets passed in here may originate from Top-DICT operands, which are
/// attacker-controlled and unbounded (a BCD real operand can parse to
/// `f64::INFINITY`, which casts to `usize::MAX`). `o + N` would overflow-panic
/// on such input under overflow checks, so every range end is built with
/// `checked_add` and a plain `.get(..)` — malformed offsets yield `None`.
fn be16(d: &[u8], o: usize) -> Option<u16> {
    let end = o.checked_add(2)?;
    d.get(o..end).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn bei16(d: &[u8], o: usize) -> Option<i16> {
    be16(d, o).map(|v| v as i16)
}

fn bei32(d: &[u8], o: usize) -> Option<i32> {
    let end = o.checked_add(4)?;
    d.get(o..end)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reads a big-endian unsigned integer of `size` bytes (1..=4).
fn read_uint(d: &[u8], o: usize, size: usize) -> Option<usize> {
    let end = o.checked_add(size)?;
    let bytes = d.get(o..end)?;
    Some(bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::truetype::Seg;

    // --- byte-level fixture helpers ----------------------------------------

    fn u16be(n: u16) -> [u8; 2] {
        n.to_be_bytes()
    }

    /// Encodes one DICT operand as a 32-bit integer (opcode 29): a fixed
    /// 5-byte form regardless of magnitude. Using this form everywhere in the
    /// fixture builders means an operand's encoded length never depends on
    /// its value, which keeps the offset arithmetic below simple.
    fn dict_int_operand(out: &mut Vec<u8>, v: i32) {
        out.push(29);
        out.extend_from_slice(&v.to_be_bytes());
    }

    /// Encodes a DICT operator: one byte for 0..=21, or the escape form
    /// `12 x` for `0x0c00 | x`.
    fn dict_operator(out: &mut Vec<u8>, op: u16) {
        if op > 0x00ff {
            out.push(12);
            out.push((op & 0xff) as u8);
        } else {
            out.push(op as u8);
        }
    }

    /// Builds a CFF INDEX from its object bytes (an empty slice is the
    /// count==0 form).
    fn build_index(objects: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u16be(objects.len() as u16));
        if objects.is_empty() {
            return out;
        }
        let total_data_len: usize = objects.iter().map(|o| o.len()).sum();
        let off_size: u8 = if total_data_len < 0xff { 1 } else { 2 };
        out.push(off_size);
        let mut offset = 1usize; // offsets are 1-based
        let mut offsets = vec![offset];
        for o in objects {
            offset += o.len();
            offsets.push(offset);
        }
        for &v in &offsets {
            let bytes = (v as u32).to_be_bytes();
            out.extend_from_slice(&bytes[4 - off_size as usize..]);
        }
        for o in objects {
            out.extend_from_slice(o);
        }
        out
    }

    /// Builds a non-CID Top DICT's bytes: charset(15), CharStrings(17),
    /// Private(18, `[size, offset]`).
    fn build_top_dict(
        charstrings_off: i32,
        charset_off: i32,
        private_size: i32,
        private_off: i32,
    ) -> Vec<u8> {
        let mut d = Vec::new();
        dict_int_operand(&mut d, charset_off);
        dict_operator(&mut d, CHARSET_OP);
        dict_int_operand(&mut d, charstrings_off);
        dict_operator(&mut d, CHARSTRINGS_OP);
        dict_int_operand(&mut d, private_size);
        dict_int_operand(&mut d, private_off);
        dict_operator(&mut d, PRIVATE_OP);
        d
    }

    // `parse_private` doesn't extract these two operands into `Private`
    // (see its doc comment), so production no longer names them; the
    // fixture builders below still embed them, to keep the synthetic Private
    // DICTs realistic and to exercise the parser skipping past them.
    const PRIV_DEFAULT_WIDTH_X_OP: u16 = 20;
    const PRIV_NOMINAL_WIDTH_X_OP: u16 = 21;

    fn build_private_dict(default_width_x: i32, nominal_width_x: i32) -> Vec<u8> {
        let mut d = Vec::new();
        dict_int_operand(&mut d, default_width_x);
        dict_operator(&mut d, PRIV_DEFAULT_WIDTH_X_OP);
        dict_int_operand(&mut d, nominal_width_x);
        dict_operator(&mut d, PRIV_NOMINAL_WIDTH_X_OP);
        d
    }

    fn charset_format0(sids: &[u16]) -> Vec<u8> {
        let mut c = vec![0u8]; // format 0
        for &sid in sids {
            c.extend_from_slice(&u16be(sid));
        }
        c
    }

    /// Assembles a minimal non-CID CFF blob: header, Name/Top-DICT/String/
    /// Global-Subr INDEXes, then charset, Private DICT, and CharStrings, in
    /// that order. gid 0 is `.notdef`; gid 1 is named "foo" (a custom String
    /// INDEX entry, SID 391); gid 2 is named "space" (standard SID 1, from
    /// the bundled `STANDARD_STRINGS` table, not the font's String INDEX).
    /// Both mappings go through a format-0 charset. Every glyph program is a
    /// single `endchar` byte since Task 1 parses the container only.
    fn build_fixture() -> Vec<u8> {
        const ENDCHAR: u8 = 14;

        let header = vec![1u8, 0, 4, 4]; // major, minor, hdrSize=4, offSize=4 (unused by readers)
        let name_index = build_index(&[b"Synthetic"]);
        let string_index = build_index(&[b"foo"]);
        let global_subr_index = build_index(&[]);
        let charstrings_index = build_index(&[&[ENDCHAR], &[ENDCHAR], &[ENDCHAR]]);
        let charset = charset_format0(&[391, 1]); // gid 1 -> SID 391 ("foo"), gid 2 -> SID 1 ("space")
        let private = build_private_dict(100, 50);

        // The Top DICT INDEX's length doesn't depend on the offset *values*
        // (every operand uses the fixed-width int32 form), so measuring it
        // with placeholders gives the exact length before the real offsets,
        // which depend on this length, are known.
        let placeholder_top_dict = build_top_dict(0, 0, private.len() as i32, 0);
        let top_dict_index_len = build_index(&[&placeholder_top_dict]).len();

        let prefix_len = header.len()
            + name_index.len()
            + top_dict_index_len
            + string_index.len()
            + global_subr_index.len();

        // Layout after the leading indexes: charset, then Private, then
        // CharStrings.
        let charset_off = prefix_len as i32;
        let private_off = charset_off + charset.len() as i32;
        let charstrings_off = private_off + private.len() as i32;

        let top_dict = build_top_dict(
            charstrings_off,
            charset_off,
            private.len() as i32,
            private_off,
        );
        let top_dict_index = build_index(&[&top_dict]);
        assert_eq!(
            top_dict_index.len(),
            top_dict_index_len,
            "int32-form operands make the Top DICT INDEX length offset-independent"
        );

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&top_dict_index);
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&private);
        out.extend_from_slice(&charstrings_index);
        out
    }

    #[test]
    fn parses_num_glyphs() {
        let font = CffFont::parse(build_fixture()).expect("fixture parses");
        assert_eq!(font.num_glyphs(), 3);
    }

    #[test]
    fn charset_maps_name_to_gid() {
        let font = CffFont::parse(build_fixture()).expect("fixture parses");
        assert_eq!(font.gid_for_name("foo"), Some(1));
        assert_eq!(font.gid_for_name(".notdef"), Some(0));
        assert_eq!(font.gid_for_name("missing"), None);
    }

    #[test]
    fn charset_maps_standard_name_to_gid() {
        // gid 2's charset entry is SID 1, which must resolve via the bundled
        // `STANDARD_STRINGS` table (not the font's own String INDEX, which
        // only carries "foo"). Locks in the full 391-entry standard-strings
        // table added for the CFF-container review fixes.
        let font = CffFont::parse(build_fixture()).expect("fixture parses");
        assert_eq!(font.gid_for_name("space"), Some(2));
    }

    #[test]
    fn standard_strings_table_is_complete_and_ordered() {
        assert_eq!(STANDARD_STRINGS.len(), 391);
        assert_eq!(STANDARD_STRINGS[0], ".notdef");
        assert_eq!(STANDARD_STRINGS[1], "space");
        assert_eq!(STANDARD_STRINGS[34], "A");
        assert_eq!(STANDARD_STRINGS[390], "Semibold");
    }

    #[test]
    fn units_per_em_defaults_to_1000() {
        let font = CffFont::parse(build_fixture()).expect("fixture parses");
        assert_eq!(font.units_per_em(), 1000.0);
    }

    // --- Type2 charstring fixtures -------------------------------------------

    /// Pushes a Type2 charstring integer operand in the compact single-byte
    /// range (`-107..=107`, encoded `139 + v`), which covers every operand
    /// these fixtures need -- so a charstring is just a short, readable list
    /// of decimal values rather than a hex blob.
    fn t2_int(out: &mut Vec<u8>, v: i32) {
        assert!(
            (-107..=107).contains(&v),
            "fixture operand {v} out of the compact single-byte range"
        );
        out.push((v + 139) as u8);
    }

    /// Pushes a one-byte Type2 operator (by its decimal opcode, Tech Note
    /// 5177 Appendix A).
    fn t2_op(out: &mut Vec<u8>, op: u8) {
        out.push(op);
    }

    /// Builds a Private DICT with `Subrs` pointing at `subrs_rel_off` bytes
    /// past the Private DICT's own start (the offset the `19` operator
    /// records), alongside the usual default/nominal width operands.
    fn build_private_dict_with_subrs(
        default_width_x: i32,
        nominal_width_x: i32,
        subrs_rel_off: i32,
    ) -> Vec<u8> {
        let mut d = Vec::new();
        dict_int_operand(&mut d, default_width_x);
        dict_operator(&mut d, PRIV_DEFAULT_WIDTH_X_OP);
        dict_int_operand(&mut d, nominal_width_x);
        dict_operator(&mut d, PRIV_NOMINAL_WIDTH_X_OP);
        dict_int_operand(&mut d, subrs_rel_off);
        dict_operator(&mut d, PRIV_SUBRS_OP);
        d
    }

    /// Builds a minimal non-CID CFF blob for exercising `glyph_path`:
    /// `charstrings[i]` becomes gid `i` (conventionally gid 0 is `.notdef`),
    /// and `local_subrs` becomes the font's local Subrs INDEX (its Type2
    /// `callsubr` bias is computed from its count, per Tech Note 5177). Uses
    /// the predefined ISOAdobe charset (Top DICT `charset` operand `0`, so
    /// gid == SID and no charset bytes are needed) since these tests only
    /// exercise `glyph_path`, not glyph-name lookup.
    fn build_glyph_fixture(charstrings: &[&[u8]], local_subrs: &[&[u8]]) -> Vec<u8> {
        let header = vec![1u8, 0, 4, 4];
        let name_index = build_index(&[b"Synthetic"]);
        let string_index = build_index(&[]);
        let global_subr_index = build_index(&[]);
        let charstrings_index = build_index(charstrings);
        let local_subr_index = build_index(local_subrs);

        // The Private DICT's length doesn't depend on the real Subrs offset
        // (every operand uses the fixed-width int32 form): measure it with a
        // placeholder offset, then rebuild with the real one (same length),
        // mirroring the Top DICT placeholder trick used by `build_fixture`.
        let private_len = build_private_dict_with_subrs(0, 0, 0).len();
        let private = build_private_dict_with_subrs(0, 0, private_len as i32);
        assert_eq!(private.len(), private_len);

        let placeholder_top = build_top_dict(0, 0, private_len as i32, 0);
        let top_dict_index_len = build_index(&[&placeholder_top]).len();

        let prefix_len = header.len()
            + name_index.len()
            + top_dict_index_len
            + string_index.len()
            + global_subr_index.len();

        // Layout after the leading indexes: Private DICT, then its local
        // Subrs INDEX (the offset the Subrs operator above points to), then
        // CharStrings. The predefined charset needs no bytes at all.
        let private_off = prefix_len as i32;
        let charstrings_off = private_off + private.len() as i32 + local_subr_index.len() as i32;

        let top_dict = build_top_dict(charstrings_off, 0, private_len as i32, private_off);
        let top_dict_index = build_index(&[&top_dict]);
        assert_eq!(top_dict_index.len(), top_dict_index_len);

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&top_dict_index);
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);
        out.extend_from_slice(&private);
        out.extend_from_slice(&local_subr_index);
        out.extend_from_slice(&charstrings_index);
        out
    }

    const ENDCHAR: u8 = 14;

    #[test]
    fn glyph_path_decodes_rectangle_via_rmoveto_rlineto_endchar() {
        const RMOVETO: u8 = 21;
        const RLINETO: u8 = 5;

        let mut cs = Vec::new();
        t2_int(&mut cs, 10);
        t2_int(&mut cs, 10);
        t2_op(&mut cs, RMOVETO); // move to (10, 10)
        t2_int(&mut cs, 80);
        t2_int(&mut cs, 0);
        t2_int(&mut cs, 0);
        t2_int(&mut cs, 80);
        t2_int(&mut cs, -80);
        t2_int(&mut cs, 0);
        t2_op(&mut cs, RLINETO); // (90,10) -> (90,90) -> (10,90)
        t2_op(&mut cs, ENDCHAR);

        let notdef: &[u8] = &[ENDCHAR];
        let font =
            CffFont::parse(build_glyph_fixture(&[notdef, &cs], &[])).expect("fixture parses");

        assert_eq!(
            font.glyph_path(1),
            vec![
                Seg::Move(10.0, 10.0),
                Seg::Line(90.0, 10.0),
                Seg::Line(90.0, 90.0),
                Seg::Line(10.0, 90.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_decodes_rrcurveto_as_cubic() {
        const RMOVETO: u8 = 21;
        const RRCURVETO: u8 = 8;

        let mut cs = Vec::new();
        t2_int(&mut cs, 0);
        t2_int(&mut cs, 0);
        t2_op(&mut cs, RMOVETO); // move to (0, 0)
        t2_int(&mut cs, 10);
        t2_int(&mut cs, 20);
        t2_int(&mut cs, 30);
        t2_int(&mut cs, 40);
        t2_int(&mut cs, 5);
        t2_int(&mut cs, 6);
        t2_op(&mut cs, RRCURVETO);
        t2_op(&mut cs, ENDCHAR);

        let notdef: &[u8] = &[ENDCHAR];
        let font =
            CffFont::parse(build_glyph_fixture(&[notdef, &cs], &[])).expect("fixture parses");

        // c1 = (0,0)+(10,20); c2 = c1+(30,40); end = c2+(5,6).
        assert_eq!(
            font.glyph_path(1),
            vec![
                Seg::Move(0.0, 0.0),
                Seg::Cubic(10.0, 20.0, 40.0, 60.0, 45.0, 66.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn glyph_path_follows_callsubr_with_correct_bias() {
        const RMOVETO: u8 = 21;
        const RLINETO: u8 = 5;
        const CALLSUBR: u8 = 10;
        const RETURN: u8 = 11;

        // One local subr -> nSubrs (1) < 1240 -> bias 107 (Tech Note 5177).
        // To invoke subr 0 the charstring pushes 0 - 107 = -107, which is
        // representable in the compact single-byte operand form.
        let mut subr0 = Vec::new();
        t2_int(&mut subr0, 30);
        t2_int(&mut subr0, 0);
        t2_op(&mut subr0, RLINETO); // relative line (30, 0)
        t2_op(&mut subr0, RETURN);

        let mut cs = Vec::new();
        t2_int(&mut cs, 20);
        t2_int(&mut cs, 20);
        t2_op(&mut cs, RMOVETO); // move to (20, 20)
        t2_int(&mut cs, -107); // subr index 0, biased
        t2_op(&mut cs, CALLSUBR);
        t2_op(&mut cs, ENDCHAR);

        let notdef: &[u8] = &[ENDCHAR];
        let font =
            CffFont::parse(build_glyph_fixture(&[notdef, &cs], &[&subr0])).expect("fixture parses");

        assert_eq!(
            font.glyph_path(1),
            vec![Seg::Move(20.0, 20.0), Seg::Line(50.0, 20.0), Seg::Close,]
        );
    }

    // --- CID-keyed fixture ---------------------------------------------------

    fn build_ros_operator(out: &mut Vec<u8>) {
        // ROS operands (registry SID, ordering SID, supplement number) are
        // irrelevant to this parser; only the operator's *presence* flags a
        // CID-keyed font.
        dict_int_operand(out, 0);
        dict_int_operand(out, 0);
        dict_int_operand(out, 0);
        dict_operator(out, ROS_OP);
    }

    fn build_fd_dict(private_size: i32, private_off: i32) -> Vec<u8> {
        let mut d = Vec::new();
        dict_int_operand(&mut d, private_size);
        dict_int_operand(&mut d, private_off);
        dict_operator(&mut d, PRIVATE_OP);
        d
    }

    fn build_cid_top_dict(
        charstrings_off: i32,
        charset_off: i32,
        fd_array_off: i32,
        fd_select_off: i32,
    ) -> Vec<u8> {
        let mut d = Vec::new();
        build_ros_operator(&mut d);
        dict_int_operand(&mut d, charset_off);
        dict_operator(&mut d, CHARSET_OP);
        dict_int_operand(&mut d, charstrings_off);
        dict_operator(&mut d, CHARSTRINGS_OP);
        dict_int_operand(&mut d, fd_array_off);
        dict_operator(&mut d, FDARRAY_OP);
        dict_int_operand(&mut d, fd_select_off);
        dict_operator(&mut d, FDSELECT_OP);
        d
    }

    /// Assembles a minimal CID-keyed CFF blob: a `ROS`-bearing Top DICT, a
    /// `FDArray` with one font DICT (Private DICT, no local Subrs), a format-0
    /// `FDSelect`, and a format-0 charset mapping gid 1 -> CID 5.
    fn build_fixture_cid() -> Vec<u8> {
        const ENDCHAR: u8 = 14;

        let header = vec![1u8, 0, 4, 4];
        let name_index = build_index(&[b"SyntheticCID"]);
        let string_index = build_index(&[]); // CID fonts carry no glyph names
        let global_subr_index = build_index(&[]);
        let charstrings_index = build_index(&[&[ENDCHAR], &[ENDCHAR]]);
        let charset = charset_format0(&[5]); // gid 1 -> CID 5
        let fd_select = vec![0u8, 0, 0]; // format 0, fd 0 for both glyphs
        let private = build_private_dict(100, 50);

        let placeholder_top = build_cid_top_dict(0, 0, 0, 0);
        let top_dict_index_len = build_index(&[&placeholder_top]).len();

        let prefix_len = header.len()
            + name_index.len()
            + top_dict_index_len
            + string_index.len()
            + global_subr_index.len();

        // Layout after the leading indexes: charset, Private (pointed to by
        // the FD dict), the FDArray INDEX itself, FDSelect, then CharStrings.
        let charset_off = prefix_len as i32;
        let private_off = charset_off + charset.len() as i32;
        let fd_dict = build_fd_dict(private.len() as i32, private_off);
        let fd_array_index = build_index(&[&fd_dict]);
        let fd_array_off = private_off + private.len() as i32;
        let fd_select_off = fd_array_off + fd_array_index.len() as i32;
        let charstrings_off = fd_select_off + fd_select.len() as i32;

        let top_dict =
            build_cid_top_dict(charstrings_off, charset_off, fd_array_off, fd_select_off);
        let top_dict_index = build_index(&[&top_dict]);
        assert_eq!(top_dict_index.len(), top_dict_index_len);

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&top_dict_index);
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&private);
        out.extend_from_slice(&fd_array_index);
        out.extend_from_slice(&fd_select);
        out.extend_from_slice(&charstrings_index);
        out
    }

    // --- glyph.rs's embedded-CFF paint fixtures --------------------------------
    //
    // A real box-glyph charstring (not just `endchar`), reused by both a
    // named non-CID charset entry and a CID-keyed one so glyph.rs's tier-gate
    // render tests can exercise the whole `GlyphFont::load` -> `outline`
    // path, not just container parsing.

    /// The Type2 charstring for a box glyph tracing (100,0)-(600,700) in
    /// 1000-upm font units -- the same rectangle `truetype::tests::
    /// build_font` uses, so callers can reuse its known device-pixel
    /// position. Every operand is a small decimal delta (Type2 charstrings
    /// are relative); a delta bigger than the compact single-byte operand
    /// range is chained as several smaller ones within one `rlineto`.
    fn box_glyph_charstring() -> Vec<u8> {
        const RMOVETO: u8 = 21;
        const RLINETO: u8 = 5;

        let mut cs = Vec::new();
        t2_int(&mut cs, 100);
        t2_int(&mut cs, 0);
        t2_op(&mut cs, RMOVETO); // move to (100, 0)
        for _ in 0..5 {
            t2_int(&mut cs, 100); // (100,0) -> (600,0): +500, as 5 x +100
            t2_int(&mut cs, 0);
        }
        for _ in 0..7 {
            t2_int(&mut cs, 0); // (600,0) -> (600,700): +700, as 7 x +100
            t2_int(&mut cs, 100);
        }
        for _ in 0..5 {
            t2_int(&mut cs, -100); // (600,700) -> (100,700): -500, as 5 x -100
            t2_int(&mut cs, 0);
        }
        t2_op(&mut cs, RLINETO); // one rlineto call draws all 17 segments above
        t2_op(&mut cs, ENDCHAR);
        cs
    }

    /// Builds a minimal non-CID CFF font: gid 0 is `.notdef`; gid 1 is the
    /// box glyph from `box_glyph_charstring`, named `glyph_name` via a custom
    /// String INDEX entry (mirrors `build_fixture`'s charset/String-INDEX
    /// layout, but with a real outline instead of a bare `endchar`).
    pub(crate) fn build_box_glyph_fixture(glyph_name: &str) -> Vec<u8> {
        let cs = box_glyph_charstring();
        let notdef: &[u8] = &[ENDCHAR];

        let header = vec![1u8, 0, 4, 4];
        let name_index = build_index(&[b"Synthetic"]);
        let string_index = build_index(&[glyph_name.as_bytes()]);
        let global_subr_index = build_index(&[]);
        let charstrings_index = build_index(&[notdef, &cs]);
        let charset = charset_format0(&[391]); // gid 1 -> SID 391 (glyph_name)
        let private = build_private_dict(0, 0);

        let placeholder_top = build_top_dict(0, 0, private.len() as i32, 0);
        let top_dict_index_len = build_index(&[&placeholder_top]).len();

        let prefix_len = header.len()
            + name_index.len()
            + top_dict_index_len
            + string_index.len()
            + global_subr_index.len();

        let charset_off = prefix_len as i32;
        let private_off = charset_off + charset.len() as i32;
        let charstrings_off = private_off + private.len() as i32;

        let top_dict = build_top_dict(
            charstrings_off,
            charset_off,
            private.len() as i32,
            private_off,
        );
        let top_dict_index = build_index(&[&top_dict]);
        assert_eq!(top_dict_index.len(), top_dict_index_len);

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&top_dict_index);
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&private);
        out.extend_from_slice(&charstrings_index);
        out
    }

    /// Builds a minimal CID-keyed CFF font: gid 0 is `.notdef`; gid 1 is the
    /// box glyph from `box_glyph_charstring`, mapped to `cid` via the charset
    /// (mirrors `build_fixture_cid`'s FDArray/FDSelect layout, but with a
    /// real outline instead of a bare `endchar`).
    pub(crate) fn build_box_glyph_fixture_cid(cid: u16) -> Vec<u8> {
        let cs = box_glyph_charstring();
        let notdef: &[u8] = &[ENDCHAR];

        let header = vec![1u8, 0, 4, 4];
        let name_index = build_index(&[b"SyntheticCID"]);
        let string_index = build_index(&[]); // CID fonts carry no glyph names
        let global_subr_index = build_index(&[]);
        let charstrings_index = build_index(&[notdef, &cs]);
        let charset = charset_format0(&[cid]); // gid 1 -> CID `cid`
        let fd_select = vec![0u8, 0, 0]; // format 0, fd 0 for both glyphs
        let private = build_private_dict(0, 0);

        let placeholder_top = build_cid_top_dict(0, 0, 0, 0);
        let top_dict_index_len = build_index(&[&placeholder_top]).len();

        let prefix_len = header.len()
            + name_index.len()
            + top_dict_index_len
            + string_index.len()
            + global_subr_index.len();

        let charset_off = prefix_len as i32;
        let private_off = charset_off + charset.len() as i32;
        let fd_dict = build_fd_dict(private.len() as i32, private_off);
        let fd_array_index = build_index(&[&fd_dict]);
        let fd_array_off = private_off + private.len() as i32;
        let fd_select_off = fd_array_off + fd_array_index.len() as i32;
        let charstrings_off = fd_select_off + fd_select.len() as i32;

        let top_dict =
            build_cid_top_dict(charstrings_off, charset_off, fd_array_off, fd_select_off);
        let top_dict_index = build_index(&[&top_dict]);
        assert_eq!(top_dict_index.len(), top_dict_index_len);

        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);
        out.extend_from_slice(&top_dict_index);
        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&private);
        out.extend_from_slice(&fd_array_index);
        out.extend_from_slice(&fd_select);
        out.extend_from_slice(&charstrings_index);
        out
    }

    #[test]
    fn box_glyph_fixture_paints_expected_rectangle() {
        // Locks in `box_glyph_charstring`'s geometry independent of
        // glyph.rs's render tests, which only assert a single pixel.
        let font = CffFont::parse(build_box_glyph_fixture("thebox")).expect("fixture parses");
        assert_eq!(font.gid_for_name("thebox"), Some(1));
        assert_eq!(
            font.glyph_path(1),
            vec![
                Seg::Move(100.0, 0.0),
                Seg::Line(200.0, 0.0),
                Seg::Line(300.0, 0.0),
                Seg::Line(400.0, 0.0),
                Seg::Line(500.0, 0.0),
                Seg::Line(600.0, 0.0),
                Seg::Line(600.0, 100.0),
                Seg::Line(600.0, 200.0),
                Seg::Line(600.0, 300.0),
                Seg::Line(600.0, 400.0),
                Seg::Line(600.0, 500.0),
                Seg::Line(600.0, 600.0),
                Seg::Line(600.0, 700.0),
                Seg::Line(500.0, 700.0),
                Seg::Line(400.0, 700.0),
                Seg::Line(300.0, 700.0),
                Seg::Line(200.0, 700.0),
                Seg::Line(100.0, 700.0),
                Seg::Close,
            ]
        );
    }

    #[test]
    fn cid_font_maps_cid_to_gid_via_charset() {
        let font = CffFont::parse(build_fixture_cid()).expect("CID fixture parses");
        assert_eq!(font.num_glyphs(), 2);
        assert_eq!(font.gid_for_cid(5), Some(1));
        assert_eq!(font.gid_for_cid(0), Some(0), ".notdef is always gid 0");
        assert_eq!(font.gid_for_cid(9), None);
        assert_eq!(
            font.gid_for_name("anything"),
            None,
            "CID-keyed fonts have no glyph names"
        );
    }

    // --- Index reader ---------------------------------------------------------

    #[test]
    fn index_parses_objects_and_length() {
        let raw = build_index(&[b"AB", b"CDE"]);
        let (idx, consumed) = Index::parse(&raw, 0).expect("valid index");
        assert_eq!(consumed, raw.len());
        assert_eq!(idx.count(), 2);
        assert_eq!(idx.get(0), Some(&b"AB"[..]));
        assert_eq!(idx.get(1), Some(&b"CDE"[..]));
        assert_eq!(idx.get(2), None);
    }

    #[test]
    fn empty_index_has_zero_count() {
        let raw = build_index(&[]);
        let (idx, consumed) = Index::parse(&raw, 0).expect("valid empty index");
        assert_eq!(consumed, 2);
        assert_eq!(idx.count(), 0);
        assert_eq!(idx.get(0), None);
    }

    // --- Dict decoder -----------------------------------------------------

    #[test]
    fn dict_decodes_integer_and_real_operands() {
        let d: Vec<u8> = vec![
            139,  // compact integer operand: value 0 (139 - 139)
            30,   // real operand: -1.5
            0xe1, // nibbles '-','1'
            0xa5, // nibbles '.','5'
            0xff, // nibble 'end', then a padding nibble (also ignored)
            6,    // an arbitrary one-byte operator to close the entry
        ];

        let dict = parse_dict(&d).expect("dict parses");
        assert_eq!(dict.get(&6), Some(&vec![0.0, -1.5]));
    }

    #[test]
    fn dict_decodes_escape_operator() {
        let mut d = Vec::new();
        dict_int_operand(&mut d, 1000);
        dict_operator(&mut d, ROS_OP); // 12 30
        let dict = parse_dict(&d).expect("dict parses");
        assert_eq!(dict.get(&ROS_OP), Some(&vec![1000.0]));
    }

    #[test]
    fn units_per_em_reads_font_matrix() {
        let mut top = Dict::default();
        top.insert(FONT_MATRIX_OP, vec![0.002, 0.0, 0.0, 0.002, 0.0, 0.0]);
        assert_eq!(units_per_em_from_top_dict(&top), 500.0);
    }

    #[test]
    fn units_per_em_defaults_without_font_matrix() {
        assert_eq!(units_per_em_from_top_dict(&Dict::default()), 1000.0);
    }

    #[test]
    fn garbage_input_does_not_panic() {
        assert!(CffFont::parse(vec![]).is_none());
        assert!(CffFont::parse(vec![0u8; 4]).is_none());
        assert!(CffFont::parse(vec![1, 0, 4, 4]).is_none());
        assert!(CffFont::parse(vec![0xff; 16]).is_none());
        assert!(CffFont::parse(vec![7, 200, 1, 9, 255, 0, 128, 3]).is_none());
        assert!(CffFont::parse(vec![42u8; 32]).is_none());
    }

    #[test]
    fn leniency_top_dict_offset_overflow_does_not_panic() {
        // A Top-DICT BCD real operand of "9E999" parses (per `parse_real`'s
        // use of `str::parse`) to `f64::INFINITY`, which then casts to
        // `usize::MAX` at the `first_num(...)? as usize` call site for
        // `CHARSTRINGS_OP`. Every downstream offset read (`be16`/`bei32`/
        // `read_uint` and their callers) must fail gracefully via
        // `checked_add` rather than overflow-panic on `offset + N` -- this
        // is the exact "leniency panic" this review fixed.
        let mut top_dict = vec![
            30, // real-number operand marker
            0x9b, 0x99, 0x9f, // packed nibbles: '9' 'E' '9' '9' '9' <end>
        ];
        dict_operator(&mut top_dict, CHARSTRINGS_OP);

        let header = vec![1u8, 0, 4, 4];
        let name_index = build_index(&[]);
        let top_dict_index = build_index(&[&top_dict]);
        let string_index = build_index(&[]);
        let global_subr_index = build_index(&[]);

        let mut data = Vec::new();
        data.extend_from_slice(&header);
        data.extend_from_slice(&name_index);
        data.extend_from_slice(&top_dict_index);
        data.extend_from_slice(&string_index);
        data.extend_from_slice(&global_subr_index);

        assert!(CffFont::parse(data).is_none());
    }
}
