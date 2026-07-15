//! Content-stream operator parsing (ISO 32000 §8/§9), including inline
//! images. Lenient: unknown operators and arity mismatches are skipped
//! (the operand stack is cleared), never an error.

use crate::error::{Error, Result};
use crate::geom::Matrix;
use crate::lexer::{is_whitespace, Lexer, Token};
use crate::object::{Dict, Name, Object};

/// Maximum operand container (array/dictionary) nesting depth. Composing
/// values recurses per nesting level, so without a bound a content stream
/// of e.g. 50k `[`s overflows the stack and aborts the process. Genuine
/// operands (dash arrays, `TJ` arrays, property dicts) nest one or two
/// levels deep.
const MAX_NESTING_DEPTH: usize = 128;

/// The syntax error reported when `MAX_NESTING_DEPTH` is exceeded.
fn too_deep(lexer: &Lexer) -> Error {
    Error::Syntax {
        offset: lexer.pos(),
        msg: "operand nesting too deep".into(),
    }
}

/// An inline image (`BI ... ID ... EI`) with its dictionary keys and
/// colorspace abbreviations expanded to their canonical names
/// (e.g. `/BPC` -> `/BitsPerComponent`, `/RGB` -> `/DeviceRGB`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImageParams {
    pub dict: Dict,
    pub data: Vec<u8>,
}

/// One element of a `TJ` (show text with adjustments) array.
#[derive(Debug, Clone, PartialEq)]
pub enum TextItem {
    /// A string to show.
    Str(Vec<u8>),
    /// A position adjustment in thousandths of text-space units.
    Offset(f32),
}

/// A parsed content-stream operator with its operands.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // Graphics state.
    /// `q`
    Save,
    /// `Q`
    Restore,
    /// `cm`
    Concat(Matrix),
    /// `w`
    SetLineWidth(f32),
    /// `J`
    SetLineCap(i32),
    /// `j`
    SetLineJoin(i32),
    /// `M`
    SetMiterLimit(f32),
    /// `d` (dash array, phase)
    SetDash(Vec<f32>, f32),
    /// `ri`
    SetRenderingIntent(Name),
    /// `i`
    SetFlatness(f32),
    /// `gs`
    SetExtGState(Name),

    // Path construction.
    /// `m`
    MoveTo(f32, f32),
    /// `l`
    LineTo(f32, f32),
    /// `c`
    CurveTo(f32, f32, f32, f32, f32, f32),
    /// `v` (first control point = current point)
    CurveToV(f32, f32, f32, f32),
    /// `y` (second control point = end point)
    CurveToY(f32, f32, f32, f32),
    /// `h`
    ClosePath,
    /// `re` (x, y, width, height)
    Rect(f32, f32, f32, f32),

    // Path painting.
    /// `S`
    Stroke,
    /// `s`
    CloseStroke,
    /// `f` / `F`
    Fill,
    /// `f*`
    FillEvenOdd,
    /// `B`
    FillStroke,
    /// `B*`
    FillStrokeEvenOdd,
    /// `b`
    CloseFillStroke,
    /// `b*`
    CloseFillStrokeEvenOdd,
    /// `n`
    EndPath,
    /// `W`
    ClipNonZero,
    /// `W*`
    ClipEvenOdd,

    // Color.
    /// `CS`
    SetStrokeColorSpace(Name),
    /// `cs`
    SetFillColorSpace(Name),
    /// `SC`
    SetStrokeColor(Vec<f32>),
    /// `SCN` (components, optional pattern name)
    SetStrokeColorN(Vec<f32>, Option<Name>),
    /// `sc`
    SetFillColor(Vec<f32>),
    /// `scn` (components, optional pattern name)
    SetFillColorN(Vec<f32>, Option<Name>),
    /// `G`
    SetStrokeGray(f32),
    /// `g`
    SetFillGray(f32),
    /// `RG`
    SetStrokeRGB(f32, f32, f32),
    /// `rg`
    SetFillRGB(f32, f32, f32),
    /// `K`
    SetStrokeCMYK(f32, f32, f32, f32),
    /// `k`
    SetFillCMYK(f32, f32, f32, f32),

    // Text.
    /// `BT`
    BeginText,
    /// `ET`
    EndText,
    /// `Tc`
    SetCharSpacing(f32),
    /// `Tw`
    SetWordSpacing(f32),
    /// `Tz`
    SetHorizScaling(f32),
    /// `TL`
    SetLeading(f32),
    /// `Tf` (font resource name, size)
    SetFont(Name, f32),
    /// `d0` (wx, wy): sets the glyph width for a colored Type3 glyph,
    /// whose content sets its own color (ISO 32000-1 Table 113).
    SetGlyphWidth(f32, f32),
    /// `d1` (wx, wy, llx, lly, urx, ury): sets the glyph width and bounding
    /// box for an uncolored Type3 glyph description; color comes from the
    /// text state and color operators in the proc are ignored
    /// (ISO 32000-1 Table 113).
    SetGlyphWidthBBox(f32, f32, f32, f32, f32, f32),
    /// `Tr`
    SetTextRender(i32),
    /// `Ts`
    SetTextRise(f32),
    /// `Td`
    TextMove(f32, f32),
    /// `TD`
    TextMoveSetLeading(f32, f32),
    /// `Tm`
    SetTextMatrix(Matrix),
    /// `T*`
    TextNextLine,
    /// `Tj`
    ShowText(Vec<u8>),
    /// `TJ`
    ShowTextAdjusted(Vec<TextItem>),
    /// `'`
    NextLineShowText(Vec<u8>),
    /// `"` (word spacing, char spacing, string)
    NextLineShowTextSpaced(f32, f32, Vec<u8>),

    // XObjects, images, shading, marked content.
    /// `Do`
    XObject(Name),
    /// `BI ... ID ... EI`
    InlineImage(ImageParams),
    /// `sh`
    Shading(Name),
    /// `MP`
    MarkedContentPoint(Name),
    /// `DP` (tag, properties: inline dict or resource name)
    MarkedContentPointProps(Name, Object),
    /// `BMC`
    BeginMarkedContent(Name),
    /// `BDC` (tag, properties: inline dict or resource name)
    BeginMarkedContentProps(Name, Object),
    /// `EMC`
    EndMarkedContent,
    /// `BX`
    BeginCompat,
    /// `EX`
    EndCompat,
}

/// Parses a decoded content stream into a sequence of operators. Inline
/// image data runs from after `ID` plus one whitespace byte to `EI` at a
/// token boundary (or the declared `/L`ength when present, which is
/// trusted).
pub fn parse_content(data: &[u8]) -> Result<Vec<Op>> {
    let mut lexer = Lexer::new(data);
    let mut ops = Vec::new();
    let mut stack: Vec<Object> = Vec::new();
    loop {
        match lexer.next_token()? {
            Token::Eof => break,
            Token::Int(i) => stack.push(Object::Int(i)),
            Token::Real(r) => stack.push(Object::Real(r)),
            Token::Name(n) => stack.push(Object::Name(n)),
            Token::LitString(s) | Token::HexString(s) => stack.push(Object::String(s)),
            Token::ArrayOpen => {
                let a = parse_array(&mut lexer, 0)?;
                stack.push(a);
            }
            Token::DictOpen => {
                let d = parse_dict(&mut lexer, 0)?;
                stack.push(Object::Dict(d));
            }
            // Stray closers: malformed input, drop pending operands.
            Token::ArrayClose | Token::DictClose => stack.clear(),
            Token::Keyword(kw) => match kw.as_slice() {
                b"true" => stack.push(Object::Bool(true)),
                b"false" => stack.push(Object::Bool(false)),
                b"null" => stack.push(Object::Null),
                b"BI" => {
                    if let Some(op) = parse_inline_image(&mut lexer) {
                        ops.push(op);
                    }
                    stack.clear();
                }
                _ => {
                    if let Some(op) = dispatch(&kw, &stack) {
                        ops.push(op);
                    }
                    stack.clear();
                }
            },
        }
    }
    Ok(ops)
}

/// Composes an array value; the opening `[` has already been consumed.
/// Unexpected tokens inside are skipped leniently; nesting deeper than
/// `MAX_NESTING_DEPTH` is a syntax error.
fn parse_array(lexer: &mut Lexer, depth: usize) -> Result<Object> {
    if depth >= MAX_NESTING_DEPTH {
        return Err(too_deep(lexer));
    }
    let mut items = Vec::new();
    loop {
        match lexer.next_token()? {
            Token::ArrayClose | Token::Eof => break,
            tok => {
                if let Some(v) = compose_value(lexer, tok, depth)? {
                    items.push(v);
                }
            }
        }
    }
    Ok(Object::Array(items))
}

/// Numeric coercion for operands: `Int` and `Real` both become `f32`.
fn num(o: &Object) -> Option<f32> {
    match o {
        Object::Int(i) => Some(*i as f32),
        Object::Real(r) => Some(*r as f32),
        _ => None,
    }
}

/// The last `N` operands as numbers, or `None` on arity/type mismatch.
fn nums<const N: usize>(stack: &[Object]) -> Option<[f32; N]> {
    if stack.len() < N {
        return None;
    }
    let tail = &stack[stack.len() - N..];
    let mut out = [0.0f32; N];
    for (slot, o) in out.iter_mut().zip(tail) {
        *slot = num(o)?;
    }
    Some(out)
}

/// The last operand as an integer (`Real` truncated leniently).
fn int1(stack: &[Object]) -> Option<i32> {
    match stack.last()? {
        Object::Int(i) => Some(*i as i32),
        Object::Real(r) => Some(*r as i32),
        _ => None,
    }
}

/// The last operand as a name.
fn name1(stack: &[Object]) -> Option<Name> {
    match stack.last()? {
        Object::Name(n) => Some(n.clone()),
        _ => None,
    }
}

/// The last operand as a string's bytes.
fn str1(stack: &[Object]) -> Option<Vec<u8>> {
    match stack.last()? {
        Object::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Every operand on the stack as a number (for `SC`/`sc`).
fn all_nums(stack: &[Object]) -> Option<Vec<f32>> {
    stack.iter().map(num).collect()
}

/// A six-number tail as a [`Matrix`] (for `cm`/`Tm`).
fn matrix(stack: &[Object]) -> Option<Matrix> {
    let [a, b, c, d, e, f] = nums::<6>(stack)?;
    Some(Matrix { a, b, c, d, e, f })
}

/// Composes a dictionary; the opening `<<` has already been consumed.
/// Nesting deeper than `MAX_NESTING_DEPTH` is a syntax error.
fn parse_dict(lexer: &mut Lexer, depth: usize) -> Result<Dict> {
    if depth >= MAX_NESTING_DEPTH {
        return Err(too_deep(lexer));
    }
    let mut dict = Dict::new();
    loop {
        match lexer.next_token()? {
            Token::DictClose | Token::Eof => break,
            Token::Name(key) => {
                let tok = lexer.next_token()?;
                if matches!(tok, Token::DictClose | Token::Eof) {
                    // Key with no value: drop it.
                    break;
                }
                if let Some(v) = compose_value(lexer, tok, depth)? {
                    dict.insert(key, v);
                }
            }
            // Non-name key: malformed, skip the token.
            _ => {}
        }
    }
    Ok(dict)
}

/// Maps an operator keyword plus its operand stack to a typed [`Op`].
/// Returns `None` (operator skipped) for unknown keywords or operand
/// arity/type mismatches.
fn dispatch(kw: &[u8], stack: &[Object]) -> Option<Op> {
    Some(match kw {
        // Graphics state.
        b"q" => Op::Save,
        b"Q" => Op::Restore,
        b"cm" => Op::Concat(matrix(stack)?),
        b"w" => Op::SetLineWidth(nums::<1>(stack)?[0]),
        b"J" => Op::SetLineCap(int1(stack)?),
        b"j" => Op::SetLineJoin(int1(stack)?),
        b"M" => Op::SetMiterLimit(nums::<1>(stack)?[0]),
        b"d" => {
            if stack.len() < 2 {
                return None;
            }
            let phase = num(&stack[stack.len() - 1])?;
            let Object::Array(items) = &stack[stack.len() - 2] else {
                return None;
            };
            let dashes: Vec<f32> = items.iter().map(num).collect::<Option<_>>()?;
            Op::SetDash(dashes, phase)
        }
        b"ri" => Op::SetRenderingIntent(name1(stack)?),
        b"i" => Op::SetFlatness(nums::<1>(stack)?[0]),
        b"gs" => Op::SetExtGState(name1(stack)?),

        // Path construction.
        b"m" => {
            let [x, y] = nums(stack)?;
            Op::MoveTo(x, y)
        }
        b"l" => {
            let [x, y] = nums(stack)?;
            Op::LineTo(x, y)
        }
        b"c" => {
            let [x1, y1, x2, y2, x3, y3] = nums(stack)?;
            Op::CurveTo(x1, y1, x2, y2, x3, y3)
        }
        b"v" => {
            let [x2, y2, x3, y3] = nums(stack)?;
            Op::CurveToV(x2, y2, x3, y3)
        }
        b"y" => {
            let [x1, y1, x3, y3] = nums(stack)?;
            Op::CurveToY(x1, y1, x3, y3)
        }
        b"h" => Op::ClosePath,
        b"re" => {
            let [x, y, w, h] = nums(stack)?;
            Op::Rect(x, y, w, h)
        }

        // Path painting and clipping.
        b"S" => Op::Stroke,
        b"s" => Op::CloseStroke,
        b"f" | b"F" => Op::Fill,
        b"f*" => Op::FillEvenOdd,
        b"B" => Op::FillStroke,
        b"B*" => Op::FillStrokeEvenOdd,
        b"b" => Op::CloseFillStroke,
        b"b*" => Op::CloseFillStrokeEvenOdd,
        b"n" => Op::EndPath,
        b"W" => Op::ClipNonZero,
        b"W*" => Op::ClipEvenOdd,

        _ => return dispatch_color_text(kw, stack),
    })
}

/// Continuation of [`dispatch`]: color and text operators.
fn dispatch_color_text(kw: &[u8], stack: &[Object]) -> Option<Op> {
    Some(match kw {
        // Color.
        b"CS" => Op::SetStrokeColorSpace(name1(stack)?),
        b"cs" => Op::SetFillColorSpace(name1(stack)?),
        b"SC" => Op::SetStrokeColor(all_nums(stack)?),
        b"sc" => Op::SetFillColor(all_nums(stack)?),
        b"SCN" => {
            let (comps, pattern) = color_n(stack)?;
            Op::SetStrokeColorN(comps, pattern)
        }
        b"scn" => {
            let (comps, pattern) = color_n(stack)?;
            Op::SetFillColorN(comps, pattern)
        }
        b"G" => Op::SetStrokeGray(nums::<1>(stack)?[0]),
        b"g" => Op::SetFillGray(nums::<1>(stack)?[0]),
        b"RG" => {
            let [r, g, b] = nums(stack)?;
            Op::SetStrokeRGB(r, g, b)
        }
        b"rg" => {
            let [r, g, b] = nums(stack)?;
            Op::SetFillRGB(r, g, b)
        }
        b"K" => {
            let [c, m, y, k] = nums(stack)?;
            Op::SetStrokeCMYK(c, m, y, k)
        }
        b"k" => {
            let [c, m, y, k] = nums(stack)?;
            Op::SetFillCMYK(c, m, y, k)
        }

        // Text.
        b"BT" => Op::BeginText,
        b"ET" => Op::EndText,
        b"Tc" => Op::SetCharSpacing(nums::<1>(stack)?[0]),
        b"Tw" => Op::SetWordSpacing(nums::<1>(stack)?[0]),
        b"Tz" => Op::SetHorizScaling(nums::<1>(stack)?[0]),
        b"TL" => Op::SetLeading(nums::<1>(stack)?[0]),
        b"Tf" => {
            if stack.len() < 2 {
                return None;
            }
            let size = num(&stack[stack.len() - 1])?;
            let Object::Name(font) = &stack[stack.len() - 2] else {
                return None;
            };
            Op::SetFont(font.clone(), size)
        }
        b"d0" => {
            let [wx, wy] = nums::<2>(stack)?;
            Op::SetGlyphWidth(wx, wy)
        }
        b"d1" => {
            let [wx, wy, llx, lly, urx, ury] = nums::<6>(stack)?;
            Op::SetGlyphWidthBBox(wx, wy, llx, lly, urx, ury)
        }
        b"Tr" => Op::SetTextRender(int1(stack)?),
        b"Ts" => Op::SetTextRise(nums::<1>(stack)?[0]),
        b"Td" => {
            let [tx, ty] = nums(stack)?;
            Op::TextMove(tx, ty)
        }
        b"TD" => {
            let [tx, ty] = nums(stack)?;
            Op::TextMoveSetLeading(tx, ty)
        }
        b"Tm" => Op::SetTextMatrix(matrix(stack)?),
        b"T*" => Op::TextNextLine,
        b"Tj" => Op::ShowText(str1(stack)?),
        b"TJ" => {
            let Object::Array(items) = stack.last()? else {
                return None;
            };
            let adjusted = items
                .iter()
                .filter_map(|o| match o {
                    Object::String(s) => Some(TextItem::Str(s.clone())),
                    other => num(other).map(TextItem::Offset),
                })
                .collect();
            Op::ShowTextAdjusted(adjusted)
        }
        b"'" => Op::NextLineShowText(str1(stack)?),
        b"\"" => {
            if stack.len() < 3 {
                return None;
            }
            let s = str1(stack)?;
            let [aw, ac] = nums_at::<2>(stack, stack.len() - 3)?;
            Op::NextLineShowTextSpaced(aw, ac, s)
        }

        _ => return dispatch_misc(kw, stack),
    })
}

/// Continuation of [`dispatch`]: XObject, shading, marked-content, and
/// compatibility operators.
fn dispatch_misc(kw: &[u8], stack: &[Object]) -> Option<Op> {
    Some(match kw {
        b"Do" => Op::XObject(name1(stack)?),
        b"sh" => Op::Shading(name1(stack)?),
        b"MP" => Op::MarkedContentPoint(name1(stack)?),
        b"DP" => {
            let (tag, props) = tag_props(stack)?;
            Op::MarkedContentPointProps(tag, props)
        }
        b"BMC" => Op::BeginMarkedContent(name1(stack)?),
        b"BDC" => {
            let (tag, props) = tag_props(stack)?;
            Op::BeginMarkedContentProps(tag, props)
        }
        b"EMC" => Op::EndMarkedContent,
        b"BX" => Op::BeginCompat,
        b"EX" => Op::EndCompat,
        _ => return None,
    })
}

/// `N` numbers starting at `start` (for operators whose numeric operands
/// are not last on the stack).
fn nums_at<const N: usize>(stack: &[Object], start: usize) -> Option<[f32; N]> {
    let slice = stack.get(start..start + N)?;
    let mut out = [0.0f32; N];
    for (slot, o) in out.iter_mut().zip(slice) {
        *slot = num(o)?;
    }
    Some(out)
}

/// Operands of `SCN`/`scn`: numeric components optionally followed by a
/// pattern name.
fn color_n(stack: &[Object]) -> Option<(Vec<f32>, Option<Name>)> {
    match stack.last() {
        Some(Object::Name(n)) => {
            let comps = all_nums(&stack[..stack.len() - 1])?;
            Some((comps, Some(n.clone())))
        }
        _ => Some((all_nums(stack)?, None)),
    }
}

/// Operands of `DP`/`BDC`: a tag name and a properties value (an inline
/// dictionary or a resource name).
fn tag_props(stack: &[Object]) -> Option<(Name, Object)> {
    if stack.len() < 2 {
        return None;
    }
    let props = match &stack[stack.len() - 1] {
        o @ (Object::Dict(_) | Object::Name(_)) => o.clone(),
        _ => return None,
    };
    let Object::Name(tag) = &stack[stack.len() - 2] else {
        return None;
    };
    Some((tag.clone(), props))
}

/// Parses an inline image; the `BI` keyword has already been consumed.
/// Returns `None` (image skipped) when the stream ends before `ID` or no
/// `EI` terminator can be located.
fn parse_inline_image(lexer: &mut Lexer) -> Option<Op> {
    let mut dict = Dict::new();
    loop {
        match lexer.next_token().ok()? {
            Token::Keyword(kw) if kw == b"ID" => break,
            Token::Eof => return None,
            Token::Name(key) => {
                let tok = lexer.next_token().ok()?;
                if matches!(tok, Token::Eof) {
                    return None;
                }
                let Some(value) = compose_value(lexer, tok, 0).ok()? else {
                    continue;
                };
                let canon = expand_image_key(&key.0);
                let value = if canon == "ColorSpace" {
                    expand_colorspace(value)
                } else {
                    value
                };
                dict.insert(Name(canon.to_string()), value);
            }
            // Malformed entry: skip the stray token.
            _ => {}
        }
    }
    let bytes = lexer.data();
    let mut start = lexer.pos();
    // Exactly one whitespace byte separates `ID` from the image data.
    if bytes.get(start).is_some_and(|&b| is_whitespace(b)) {
        start += 1;
    }
    let declared = dict.get_int("Length").or_else(|| dict.get_int("L"));
    let data = if let Some(n) = declared.and_then(|n| usize::try_from(n).ok()) {
        // A declared length is trusted.
        let end = (start + n).min(bytes.len());
        lexer.seek(end);
        bytes[start..end].to_vec()
    } else {
        let Some(ei) = find_ei(bytes, start) else {
            // No terminator anywhere: drop the image and stop lexing what
            // can only be binary garbage.
            lexer.seek(bytes.len());
            return None;
        };
        // The whitespace byte before `EI` is a separator, not data.
        let mut end = ei;
        if end > start && is_whitespace(bytes[end - 1]) {
            end -= 1;
        }
        lexer.seek(ei);
        bytes[start..end].to_vec()
    };
    consume_ei(lexer);
    Some(Op::InlineImage(ImageParams { dict, data }))
}

/// Finds the offset of an `EI` terminator at a token boundary: preceded
/// by whitespace (or the data start) and followed by a non-regular byte
/// or end of input.
fn find_ei(bytes: &[u8], start: usize) -> Option<usize> {
    let mut from = start;
    while let Some(off) = memchr::memchr(b'E', &bytes[from..]) {
        let p = from + off;
        from = p + 1;
        if bytes.get(p + 1) != Some(&b'I') {
            continue;
        }
        let before_ok = p == start || is_whitespace(bytes[p - 1]);
        let after_ok = match bytes.get(p + 2) {
            None => true,
            Some(&b) => !crate::lexer::is_regular(b),
        };
        if before_ok && after_ok {
            return Some(p);
        }
    }
    None
}

/// Consumes the `EI` keyword after inline image data, scanning forward
/// leniently if the very next token is something else.
fn consume_ei(lexer: &mut Lexer) {
    let save = lexer.pos();
    if let Ok(Token::Keyword(kw)) = lexer.next_token() {
        if kw == b"EI" {
            return;
        }
    }
    match find_ei(lexer.data(), save) {
        Some(p) => lexer.seek(p + 2),
        None => lexer.seek(lexer.data().len()),
    }
}

/// Canonical spelling of an inline image dictionary key (ISO 32000
/// §8.9.7, Table 91).
fn expand_image_key(key: &str) -> &str {
    match key {
        "BPC" => "BitsPerComponent",
        "CS" => "ColorSpace",
        "D" => "Decode",
        "DP" => "DecodeParms",
        "F" => "Filter",
        "H" => "Height",
        "W" => "Width",
        "IM" => "ImageMask",
        "I" => "Interpolate",
        other => other,
    }
}

/// Expands colorspace name abbreviations inside a `/CS` value, including
/// names nested in an `Indexed` array.
fn expand_colorspace(value: Object) -> Object {
    match value {
        Object::Name(Name(n)) => Object::Name(Name(match n.as_str() {
            "G" => "DeviceGray".to_string(),
            "RGB" => "DeviceRGB".to_string(),
            "CMYK" => "DeviceCMYK".to_string(),
            "I" => "Indexed".to_string(),
            _ => n,
        })),
        Object::Array(items) => Object::Array(items.into_iter().map(expand_colorspace).collect()),
        other => other,
    }
}

/// Turns a leading token into an [`Object`], recursing for containers
/// (bounded by `MAX_NESTING_DEPTH`). Returns `None` for tokens that
/// carry no value (stray delimiters, unexpected keywords).
fn compose_value(lexer: &mut Lexer, tok: Token, depth: usize) -> Result<Option<Object>> {
    Ok(match tok {
        Token::Int(i) => Some(Object::Int(i)),
        Token::Real(r) => Some(Object::Real(r)),
        Token::Name(n) => Some(Object::Name(n)),
        Token::LitString(s) | Token::HexString(s) => Some(Object::String(s)),
        Token::ArrayOpen => Some(parse_array(lexer, depth + 1)?),
        Token::DictOpen => Some(Object::Dict(parse_dict(lexer, depth + 1)?)),
        Token::Keyword(kw) => match kw.as_slice() {
            b"true" => Some(Object::Bool(true)),
            b"false" => Some(Object::Bool(false)),
            b"null" => Some(Object::Null),
            _ => None,
        },
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(src: &[u8]) -> Vec<Op> {
        parse_content(src).expect("content parses")
    }

    fn name(s: &str) -> Name {
        Name(s.to_string())
    }

    #[test]
    fn shapes_fixture_parses_to_expected_ops() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/shapes.pdf"
        );
        let doc = crate::document::Document::open(path).expect("open shapes.pdf");
        let page = doc.page(0).expect("page 0");
        let content = page.content(&doc).expect("page content");
        let got = ops(&content);
        let m = |a, b, c, d, e, f| Matrix { a, b, c, d, e, f };
        assert_eq!(
            got,
            vec![
                Op::SetFillRGB(1.0, 0.0, 0.0),
                Op::Rect(72.0, 600.0, 100.0, 80.0),
                Op::Fill,
                Op::SetFillRGB(0.0, 0.5, 1.0),
                Op::Rect(200.0, 600.0, 120.0, 60.0),
                Op::Fill,
                Op::SetFillRGB(0.2, 0.8, 0.2),
                Op::Rect(340.0, 590.0, 90.0, 90.0),
                Op::Fill,
                Op::SetStrokeRGB(0.0, 0.0, 0.0),
                Op::SetLineWidth(2.0),
                Op::MoveTo(100.0, 300.0),
                Op::CurveTo(150.0, 400.0, 250.0, 400.0, 300.0, 300.0),
                Op::Stroke,
                Op::Save,
                Op::Concat(m(0.5, 0.0, 0.0, 0.5, 300.0, 100.0)),
                Op::SetFillRGB(0.8, 0.0, 0.8),
                Op::Rect(0.0, 0.0, 200.0, 200.0),
                Op::Fill,
                Op::Restore,
            ]
        );
    }

    #[test]
    fn graphics_state_ops_round_trip() {
        let got = ops(b"q Q 1 0 0 1 10 20 cm 2 w 1 J 2 j 3.5 M [3 1] 0.5 d \
                        /Perceptual ri 1 i /GS1 gs");
        assert_eq!(
            got,
            vec![
                Op::Save,
                Op::Restore,
                Op::Concat(Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: 10.0,
                    f: 20.0
                }),
                Op::SetLineWidth(2.0),
                Op::SetLineCap(1),
                Op::SetLineJoin(2),
                Op::SetMiterLimit(3.5),
                Op::SetDash(vec![3.0, 1.0], 0.5),
                Op::SetRenderingIntent(name("Perceptual")),
                Op::SetFlatness(1.0),
                Op::SetExtGState(name("GS1")),
            ]
        );
    }

    #[test]
    fn empty_dash_array_round_trips() {
        assert_eq!(ops(b"[] 0 d"), vec![Op::SetDash(vec![], 0.0)]);
    }

    #[test]
    fn path_ops_round_trip() {
        let got = ops(b"10 20 m 30 40 l 1 2 3 4 5 6 c 1 2 3 4 v 5 6 7 8 y h \
                        72 600 100 80 re");
        assert_eq!(
            got,
            vec![
                Op::MoveTo(10.0, 20.0),
                Op::LineTo(30.0, 40.0),
                Op::CurveTo(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
                Op::CurveToV(1.0, 2.0, 3.0, 4.0),
                Op::CurveToY(5.0, 6.0, 7.0, 8.0),
                Op::ClosePath,
                Op::Rect(72.0, 600.0, 100.0, 80.0),
            ]
        );
    }

    #[test]
    fn negative_and_real_coordinates_coerce() {
        assert_eq!(
            ops(b"-1.5 +2 m .5 -3 l"),
            vec![Op::MoveTo(-1.5, 2.0), Op::LineTo(0.5, -3.0)]
        );
    }

    #[test]
    fn xobject_shading_marked_content_round_trip() {
        let got = ops(b"/Im1 Do /Sh1 sh /Tag MP /Tag /P DP /Span BMC \
                        /Span << /MCID 3 >> BDC EMC BX EX");
        let mut props = Dict::new();
        props.insert(name("MCID"), Object::Int(3));
        assert_eq!(
            got,
            vec![
                Op::XObject(name("Im1")),
                Op::Shading(name("Sh1")),
                Op::MarkedContentPoint(name("Tag")),
                Op::MarkedContentPointProps(name("Tag"), Object::Name(name("P"))),
                Op::BeginMarkedContent(name("Span")),
                Op::BeginMarkedContentProps(name("Span"), Object::Dict(props)),
                Op::EndMarkedContent,
                Op::BeginCompat,
                Op::EndCompat,
            ]
        );
    }

    #[test]
    fn unknown_operator_is_skipped_without_breaking_following_ops() {
        assert_eq!(
            ops(b"zz 1 2 10 20 m 30 40 l"),
            vec![Op::MoveTo(10.0, 20.0), Op::LineTo(30.0, 40.0)]
        );
        // Unknown operator with operands before it: operands are dropped.
        assert_eq!(ops(b"1 2 zz 3 4 l"), vec![Op::LineTo(3.0, 4.0)]);
    }

    #[test]
    fn malformed_operands_are_skipped() {
        // Too few operands.
        assert_eq!(ops(b"1 m 5 6 l"), vec![Op::LineTo(5.0, 6.0)]);
        // Wrong operand types.
        assert_eq!(ops(b"(a) (b) m S"), vec![Op::Stroke]);
        assert_eq!(ops(b"/N w"), Vec::<Op>::new());
        assert_eq!(ops(b"5 Tf"), Vec::<Op>::new());
        // `TJ` whose operand is not an array.
        assert_eq!(ops(b"(x) TJ q"), vec![Op::Save]);
        // Non-string, non-number entries inside a `TJ` array are dropped.
        assert_eq!(
            ops(b"[(y) /Bad 5] TJ"),
            vec![Op::ShowTextAdjusted(vec![
                TextItem::Str(b"y".to_vec()),
                TextItem::Offset(5.0),
            ])]
        );
        // Dash without an array operand.
        assert_eq!(ops(b"3 0 d"), Vec::<Op>::new());
        // Extra operands: the trailing ones are used.
        assert_eq!(ops(b"9 9 1 2 m"), vec![Op::MoveTo(1.0, 2.0)]);
    }

    #[test]
    fn inline_image_with_hex_data_ending_in_ei() {
        let got = ops(b"q BI /W 2 /H 2 /BPC 8 /CS /G /F /AHx ID 00FF80FF> EI Q");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Op::Save);
        assert_eq!(got[2], Op::Restore);
        let Op::InlineImage(img) = &got[1] else {
            panic!("expected inline image, got {:?}", got[1]);
        };
        assert_eq!(img.data, b"00FF80FF>");
        assert_eq!(img.dict.get_int("Width"), Some(2));
        assert_eq!(img.dict.get_int("Height"), Some(2));
        assert_eq!(img.dict.get_int("BitsPerComponent"), Some(8));
        assert_eq!(img.dict.get_name("ColorSpace"), Some(&name("DeviceGray")));
        assert_eq!(img.dict.get_name("Filter"), Some(&name("AHx")));
    }

    #[test]
    fn inline_image_trusts_declared_length() {
        // Binary payload contains a spurious ` EI ` sequence; /L must win.
        let mut src = b"BI /W 3 /H 1 /BPC 8 /CS /RGB /L 9 ID ".to_vec();
        src.extend_from_slice(b"ab EI wxy");
        src.extend_from_slice(b" EI 1 2 m");
        let got = ops(&src);
        assert_eq!(got.len(), 2);
        let Op::InlineImage(img) = &got[0] else {
            panic!("expected inline image, got {:?}", got[0]);
        };
        assert_eq!(img.data, b"ab EI wxy");
        assert_eq!(img.dict.get_name("ColorSpace"), Some(&name("DeviceRGB")));
        assert_eq!(got[1], Op::MoveTo(1.0, 2.0));
    }

    #[test]
    fn inline_image_expands_indexed_colorspace_array() {
        let got = ops(
            b"BI /W 1 /H 1 /BPC 8 /CS [/I /RGB 1 <FF0000>] /IM false /I true \
                        ID \xde\xad EI",
        );
        let Op::InlineImage(img) = &got[0] else {
            panic!("expected inline image, got {:?}", got[0]);
        };
        assert_eq!(img.data, b"\xde\xad");
        assert_eq!(img.dict.get("ImageMask"), Some(&Object::Bool(false)));
        assert_eq!(img.dict.get("Interpolate"), Some(&Object::Bool(true)));
        let cs = img.dict.get_array("ColorSpace").expect("CS array");
        assert_eq!(cs[0], Object::Name(name("Indexed")));
        assert_eq!(cs[1], Object::Name(name("DeviceRGB")));
        assert_eq!(cs[2], Object::Int(1));
        assert_eq!(cs[3], Object::String(b"\xff\x00\x00".to_vec()));
    }

    #[test]
    fn inline_image_without_terminator_is_skipped() {
        assert_eq!(ops(b"BI /W 1 ID \x01\x02\x03"), Vec::<Op>::new());
        assert_eq!(ops(b"BI /W 1"), Vec::<Op>::new());
    }

    #[test]
    fn inline_image_with_empty_data() {
        let got = ops(b"BI /W 0 /H 0 ID  EI n");
        assert_eq!(got.len(), 2);
        let Op::InlineImage(img) = &got[0] else {
            panic!("expected inline image, got {:?}", got[0]);
        };
        assert!(img.data.is_empty());
        assert_eq!(got[1], Op::EndPath);
    }

    #[test]
    fn stray_delimiters_and_comments_are_tolerated() {
        assert_eq!(
            ops(b"% comment\n1 2 m ] >> 3 4 l"),
            vec![Op::MoveTo(1.0, 2.0), Op::LineTo(3.0, 4.0)]
        );
        assert_eq!(ops(b""), Vec::<Op>::new());
        assert_eq!(ops(b"   \n  "), Vec::<Op>::new());
    }

    #[test]
    fn text_state_ops_round_trip() {
        let got = ops(b"BT 0.5 Tc 1 Tw 90 Tz 14 TL /F1 12 Tf 3 Tr 4.5 Ts ET");
        assert_eq!(
            got,
            vec![
                Op::BeginText,
                Op::SetCharSpacing(0.5),
                Op::SetWordSpacing(1.0),
                Op::SetHorizScaling(90.0),
                Op::SetLeading(14.0),
                Op::SetFont(name("F1"), 12.0),
                Op::SetTextRender(3),
                Op::SetTextRise(4.5),
                Op::EndText,
            ]
        );
    }

    #[test]
    fn text_positioning_and_showing_round_trip() {
        let got = ops(b"BT 72 720 Td 0 -14 TD 1 0 0 1 50 60 Tm T* \
                        (Hi) Tj (there) ' 2 3 (spaced) \" ET");
        assert_eq!(
            got,
            vec![
                Op::BeginText,
                Op::TextMove(72.0, 720.0),
                Op::TextMoveSetLeading(0.0, -14.0),
                Op::SetTextMatrix(Matrix {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: 50.0,
                    f: 60.0
                }),
                Op::TextNextLine,
                Op::ShowText(b"Hi".to_vec()),
                Op::NextLineShowText(b"there".to_vec()),
                Op::NextLineShowTextSpaced(2.0, 3.0, b"spaced".to_vec()),
                Op::EndText,
            ]
        );
    }

    #[test]
    fn parses_d0_glyph_width() {
        let ops = parse_content(b"1000 0 d0").expect("parse");
        assert_eq!(ops, vec![Op::SetGlyphWidth(1000.0, 0.0)]);
    }

    #[test]
    fn parses_d1_glyph_width_bbox() {
        let ops = parse_content(b"1000 0 0 0 750 700 d1").expect("parse");
        assert_eq!(
            ops,
            vec![Op::SetGlyphWidthBBox(1000.0, 0.0, 0.0, 0.0, 750.0, 700.0)]
        );
    }

    #[test]
    fn d0_with_wrong_arity_is_skipped() {
        // Too few operands: leniently skipped (like any arity mismatch), not a panic.
        let ops = parse_content(b"1000 d0").expect("parse");
        assert!(ops.is_empty());
    }

    #[test]
    fn tj_array_mixes_strings_and_numbers() {
        let got = ops(b"[(He) -120 (llo) 33.5 <20>] TJ");
        assert_eq!(
            got,
            vec![Op::ShowTextAdjusted(vec![
                TextItem::Str(b"He".to_vec()),
                TextItem::Offset(-120.0),
                TextItem::Str(b"llo".to_vec()),
                TextItem::Offset(33.5),
                TextItem::Str(b" ".to_vec()),
            ])]
        );
    }

    #[test]
    fn color_ops_round_trip() {
        let got = ops(b"/DeviceRGB CS /DeviceGray cs 1 0 0 SC 0.5 sc \
                        0.3 G 0.7 g 1 0 0 RG 0 1 0 rg 0 0 0 1 K 1 0 0 0 k");
        assert_eq!(
            got,
            vec![
                Op::SetStrokeColorSpace(name("DeviceRGB")),
                Op::SetFillColorSpace(name("DeviceGray")),
                Op::SetStrokeColor(vec![1.0, 0.0, 0.0]),
                Op::SetFillColor(vec![0.5]),
                Op::SetStrokeGray(0.3),
                Op::SetFillGray(0.7),
                Op::SetStrokeRGB(1.0, 0.0, 0.0),
                Op::SetFillRGB(0.0, 1.0, 0.0),
                Op::SetStrokeCMYK(0.0, 0.0, 0.0, 1.0),
                Op::SetFillCMYK(1.0, 0.0, 0.0, 0.0),
            ]
        );
    }

    #[test]
    fn scn_with_and_without_pattern_name() {
        let got = ops(b"0.2 0.4 0.6 scn /P1 scn 0.1 0.2 /P2 SCN 1 SCN");
        assert_eq!(
            got,
            vec![
                Op::SetFillColorN(vec![0.2, 0.4, 0.6], None),
                Op::SetFillColorN(vec![], Some(name("P1"))),
                Op::SetStrokeColorN(vec![0.1, 0.2], Some(name("P2"))),
                Op::SetStrokeColorN(vec![1.0], None),
            ]
        );
    }

    #[test]
    fn painting_and_clipping_ops_round_trip() {
        let got = ops(b"S s f F f* B B* b b* n W W*");
        assert_eq!(
            got,
            vec![
                Op::Stroke,
                Op::CloseStroke,
                Op::Fill,
                Op::Fill,
                Op::FillEvenOdd,
                Op::FillStroke,
                Op::FillStrokeEvenOdd,
                Op::CloseFillStroke,
                Op::CloseFillStrokeEvenOdd,
                Op::EndPath,
                Op::ClipNonZero,
                Op::ClipEvenOdd,
            ]
        );
    }

    /// Runs `f` on a deliberately small stack so that unbounded recursion
    /// would abort the test binary instead of silently passing on a large
    /// main-thread stack.
    fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(f)
            .expect("spawn test thread")
            .join()
            .expect("content parser must not overflow the stack")
    }

    #[test]
    fn deeply_nested_array_is_rejected_not_stack_overflow() {
        let data = vec![b'['; 50_000];
        let result = on_small_stack(move || parse_content(&data));
        assert!(matches!(result, Err(Error::Syntax { .. })));
    }

    #[test]
    fn deeply_nested_dict_is_rejected_not_stack_overflow() {
        let mut data = Vec::new();
        for _ in 0..50_000 {
            data.extend_from_slice(b"<</K");
        }
        let result = on_small_stack(move || parse_content(&data));
        assert!(matches!(result, Err(Error::Syntax { .. })));
    }

    #[test]
    fn nesting_within_the_limit_still_parses() {
        // A BDC property dict holding a modestly nested array operand.
        let mut data: Vec<u8> = b"/Tag <</Deep ".to_vec();
        data.extend(std::iter::repeat_n(b'[', 50));
        data.extend(std::iter::repeat_n(b']', 50));
        data.extend_from_slice(b" >> BDC");
        let got = ops(&data);
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], Op::BeginMarkedContentProps(_, _)));
    }
}
