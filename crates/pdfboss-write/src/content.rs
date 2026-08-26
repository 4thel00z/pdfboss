//! Content-stream serialization: `pdfboss_core::content::Op` values back to
//! operator syntax. The writer emits from the same IR the reader parses
//! into, so `parse_content(serialize_ops(ops)) == ops` is the module's
//! defining property — every variant of [`Op`] must round-trip.
//!
//! Inline images emit exactly the dictionary entries present in
//! [`ImageParams::dict`], never an invented `/L`: the parser keeps a
//! declared length key (`/L` or `/Length`) in the parsed dictionary, so
//! re-emitting the entries reproduces the trusted length, and data
//! containing a spurious ` EI ` round-trips whenever the dictionary
//! carries one — the only way the parser can produce such data. Without a
//! length key the parser stops at the first `EI` token boundary, so
//! parser-produced data never contains a premature boundary and re-locating
//! `EI` finds the true end. The parser skips one whitespace byte after `ID`
//! and strips one before `EI`; the writer emits exactly one of each.
//!
//! Two parser-producible corners cannot round-trip byte-exactly and are
//! accepted: a dictionary or properties value holding an integral `Real`
//! reparses as `Int` (the crate serializes `2.0` as `2`), and a truncated
//! source whose declared length exceeds the actual data yields an op whose
//! dictionary promises more bytes than `data` holds.

use pdfboss_core::content::{ImageParams, Op, TextItem};
use pdfboss_core::{Name, Object};

use crate::ser::{serialize_object, write_name, write_real_f32, write_string};

/// Serializes a sequence of content operators: operands space-separated,
/// one space before each operator keyword, a newline after it.
/// Inline-image dictionaries are written with their canonical
/// (unabbreviated) keys, which the parser passes through unchanged.
/// Infallible: a stream inside a `DP`/`BDC` properties value — impossible
/// in parser-produced ops — is emitted as `null`.
pub fn serialize_ops(ops: &[Op]) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        push_op(op, &mut out);
    }
    out
}

/// Emits one operator: each operand followed by a space, then the keyword
/// and a newline.
fn push_op(op: &Op, out: &mut Vec<u8>) {
    match op {
        Op::Save => push_kw(b"q", out),
        Op::Restore => push_kw(b"Q", out),
        Op::Concat(m) => push_nums(&[m.a, m.b, m.c, m.d, m.e, m.f], b"cm", out),
        Op::SetLineWidth(w) => push_nums(&[*w], b"w", out),
        Op::SetLineCap(cap) => push_int(*cap, b"J", out),
        Op::SetLineJoin(join) => push_int(*join, b"j", out),
        Op::SetMiterLimit(limit) => push_nums(&[*limit], b"M", out),
        Op::SetDash(dashes, phase) => {
            push_f32_array(dashes, out);
            out.push(b' ');
            push_nums(&[*phase], b"d", out);
        }
        Op::SetRenderingIntent(n) => push_name_op(n, b"ri", out),
        Op::SetFlatness(f) => push_nums(&[*f], b"i", out),
        Op::SetExtGState(n) => push_name_op(n, b"gs", out),
        Op::MoveTo(x, y) => push_nums(&[*x, *y], b"m", out),
        Op::LineTo(x, y) => push_nums(&[*x, *y], b"l", out),
        Op::CurveTo(x1, y1, x2, y2, x3, y3) => {
            push_nums(&[*x1, *y1, *x2, *y2, *x3, *y3], b"c", out);
        }
        Op::CurveToV(x2, y2, x3, y3) => push_nums(&[*x2, *y2, *x3, *y3], b"v", out),
        Op::CurveToY(x1, y1, x3, y3) => push_nums(&[*x1, *y1, *x3, *y3], b"y", out),
        Op::ClosePath => push_kw(b"h", out),
        Op::Rect(x, y, w, h) => push_nums(&[*x, *y, *w, *h], b"re", out),
        Op::Stroke => push_kw(b"S", out),
        Op::CloseStroke => push_kw(b"s", out),
        Op::Fill => push_kw(b"f", out),
        Op::FillEvenOdd => push_kw(b"f*", out),
        Op::FillStroke => push_kw(b"B", out),
        Op::FillStrokeEvenOdd => push_kw(b"B*", out),
        Op::CloseFillStroke => push_kw(b"b", out),
        Op::CloseFillStrokeEvenOdd => push_kw(b"b*", out),
        Op::EndPath => push_kw(b"n", out),
        Op::ClipNonZero => push_kw(b"W", out),
        Op::ClipEvenOdd => push_kw(b"W*", out),
        Op::SetStrokeColorSpace(n) => push_name_op(n, b"CS", out),
        Op::SetFillColorSpace(n) => push_name_op(n, b"cs", out),
        Op::SetStrokeColor(comps) => push_nums(comps, b"SC", out),
        Op::SetStrokeColorN(comps, pattern) => {
            push_color_n(comps, pattern.as_ref(), b"SCN", out);
        }
        Op::SetFillColor(comps) => push_nums(comps, b"sc", out),
        Op::SetFillColorN(comps, pattern) => push_color_n(comps, pattern.as_ref(), b"scn", out),
        Op::SetStrokeGray(g) => push_nums(&[*g], b"G", out),
        Op::SetFillGray(g) => push_nums(&[*g], b"g", out),
        Op::SetStrokeRGB(r, g, b) => push_nums(&[*r, *g, *b], b"RG", out),
        Op::SetFillRGB(r, g, b) => push_nums(&[*r, *g, *b], b"rg", out),
        Op::SetStrokeCMYK(c, m, y, k) => push_nums(&[*c, *m, *y, *k], b"K", out),
        Op::SetFillCMYK(c, m, y, k) => push_nums(&[*c, *m, *y, *k], b"k", out),
        Op::BeginText => push_kw(b"BT", out),
        Op::EndText => push_kw(b"ET", out),
        Op::SetCharSpacing(v) => push_nums(&[*v], b"Tc", out),
        Op::SetWordSpacing(v) => push_nums(&[*v], b"Tw", out),
        Op::SetHorizScaling(v) => push_nums(&[*v], b"Tz", out),
        Op::SetLeading(v) => push_nums(&[*v], b"TL", out),
        Op::SetFont(n, size) => {
            write_name(&n.0, out);
            out.push(b' ');
            push_nums(&[*size], b"Tf", out);
        }
        Op::SetGlyphWidth(wx, wy) => push_nums(&[*wx, *wy], b"d0", out),
        Op::SetGlyphWidthBBox(wx, wy, llx, lly, urx, ury) => {
            push_nums(&[*wx, *wy, *llx, *lly, *urx, *ury], b"d1", out);
        }
        Op::SetTextRender(mode) => push_int(*mode, b"Tr", out),
        Op::SetTextRise(v) => push_nums(&[*v], b"Ts", out),
        Op::TextMove(tx, ty) => push_nums(&[*tx, *ty], b"Td", out),
        Op::TextMoveSetLeading(tx, ty) => push_nums(&[*tx, *ty], b"TD", out),
        Op::SetTextMatrix(m) => push_nums(&[m.a, m.b, m.c, m.d, m.e, m.f], b"Tm", out),
        Op::TextNextLine => push_kw(b"T*", out),
        Op::ShowText(s) => push_string_op(s, b"Tj", out),
        Op::ShowTextAdjusted(items) => push_text_adjusted(items, out),
        Op::NextLineShowText(s) => push_string_op(s, b"'", out),
        Op::NextLineShowTextSpaced(aw, ac, s) => {
            write_real_f32(*aw, out);
            out.push(b' ');
            write_real_f32(*ac, out);
            out.push(b' ');
            push_string_op(s, b"\"", out);
        }
        Op::XObject(n) => push_name_op(n, b"Do", out),
        Op::InlineImage(img) => push_inline_image(img, out),
        Op::Shading(n) => push_name_op(n, b"sh", out),
        Op::MarkedContentPoint(n) => push_name_op(n, b"MP", out),
        Op::MarkedContentPointProps(tag, props) => push_tag_props(tag, props, b"DP", out),
        Op::BeginMarkedContent(n) => push_name_op(n, b"BMC", out),
        Op::BeginMarkedContentProps(tag, props) => push_tag_props(tag, props, b"BDC", out),
        Op::EndMarkedContent => push_kw(b"EMC", out),
        Op::BeginCompat => push_kw(b"BX", out),
        Op::EndCompat => push_kw(b"EX", out),
    }
}

/// Writes the operator keyword and its terminating newline.
fn push_kw(kw: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(kw);
    out.push(b'\n');
}

/// Writes numeric operands, each followed by a space, then the keyword.
fn push_nums(vals: &[f32], kw: &[u8], out: &mut Vec<u8>) {
    for v in vals {
        write_real_f32(*v, out);
        out.push(b' ');
    }
    push_kw(kw, out);
}

/// Writes a plain-integer operand, then the keyword.
fn push_int(value: i32, kw: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(value.to_string().as_bytes());
    out.push(b' ');
    push_kw(kw, out);
}

/// Writes a single name operand, then the keyword.
fn push_name_op(n: &Name, kw: &[u8], out: &mut Vec<u8>) {
    write_name(&n.0, out);
    out.push(b' ');
    push_kw(kw, out);
}

/// Writes a single string operand, then the keyword.
fn push_string_op(s: &[u8], kw: &[u8], out: &mut Vec<u8>) {
    write_string(s, out);
    out.push(b' ');
    push_kw(kw, out);
}

/// Writes a `[n1 n2 …]` array of reals (the `d` dash array).
fn push_f32_array(vals: &[f32], out: &mut Vec<u8>) {
    out.push(b'[');
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        write_real_f32(*v, out);
    }
    out.push(b']');
}

/// Writes color components, the optional pattern name, then `SCN`/`scn`.
fn push_color_n(comps: &[f32], pattern: Option<&Name>, kw: &[u8], out: &mut Vec<u8>) {
    for v in comps {
        write_real_f32(*v, out);
        out.push(b' ');
    }
    if let Some(n) = pattern {
        write_name(&n.0, out);
        out.push(b' ');
    }
    push_kw(kw, out);
}

/// Writes a `TJ` array: strings and offsets space-separated in brackets.
fn push_text_adjusted(items: &[TextItem], out: &mut Vec<u8>) {
    out.push(b'[');
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        match item {
            TextItem::Str(s) => write_string(s, out),
            TextItem::Offset(v) => write_real_f32(*v, out),
        }
    }
    out.extend_from_slice(b"] ");
    push_kw(b"TJ", out);
}

/// Writes the tag and properties operands of `DP`/`BDC`, then the keyword.
fn push_tag_props(tag: &Name, props: &Object, kw: &[u8], out: &mut Vec<u8>) {
    write_name(&tag.0, out);
    out.push(b' ');
    push_object_or_null(props, out);
    out.push(b' ');
    push_kw(kw, out);
}

/// Serializes an object, falling back to `null` on the impossible nested
/// stream so [`serialize_ops`] stays infallible.
fn push_object_or_null(obj: &Object, out: &mut Vec<u8>) {
    let mark = out.len();
    if serialize_object(obj, out).is_err() {
        out.truncate(mark);
        out.extend_from_slice(b"null");
    }
}

/// Writes `BI`, the dictionary entries present (keys sorted bytewise),
/// `ID`, one whitespace byte, the raw data, one whitespace byte, and `EI`.
fn push_inline_image(img: &ImageParams, out: &mut Vec<u8>) {
    out.extend_from_slice(b"BI");
    let mut entries: Vec<(&Name, &Object)> = img.dict.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (key, value) in entries {
        out.push(b' ');
        write_name(&key.0, out);
        out.push(b' ');
        push_object_or_null(value, out);
    }
    out.extend_from_slice(b" ID ");
    out.extend_from_slice(&img.data);
    out.extend_from_slice(b" EI\n");
}

#[cfg(test)]
mod tests {
    use pdfboss_core::content::{parse_content, ImageParams, TextItem};
    use pdfboss_core::geom::Matrix;
    use pdfboss_core::{Dict, Name, Object};

    use super::*;

    /// Number of `Op` variants; [`variant_index`] fails to compile when the
    /// enum grows, forcing this constant and the sample table to follow.
    const VARIANT_COUNT: usize = 70;

    fn name(s: &str) -> Name {
        Name(s.to_string())
    }

    fn m(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Matrix {
        Matrix { a, b, c, d, e, f }
    }

    fn image(entries: &[(&str, Object)], data: &[u8]) -> Op {
        let mut dict = Dict::new();
        for (key, value) in entries {
            dict.insert(name(key), value.clone());
        }
        Op::InlineImage(ImageParams {
            dict,
            data: data.to_vec(),
        })
    }

    fn round_trip(ops: &[Op]) {
        let bytes = serialize_ops(ops);
        let parsed = parse_content(&bytes).unwrap_or_else(|e| {
            panic!(
                "serialized ops parse: {e:?}\n{}",
                String::from_utf8_lossy(&bytes)
            )
        });
        assert_eq!(
            parsed,
            ops,
            "round-trip of {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    fn variant_index(op: &Op) -> usize {
        match op {
            Op::Save => 0,
            Op::Restore => 1,
            Op::Concat(..) => 2,
            Op::SetLineWidth(..) => 3,
            Op::SetLineCap(..) => 4,
            Op::SetLineJoin(..) => 5,
            Op::SetMiterLimit(..) => 6,
            Op::SetDash(..) => 7,
            Op::SetRenderingIntent(..) => 8,
            Op::SetFlatness(..) => 9,
            Op::SetExtGState(..) => 10,
            Op::MoveTo(..) => 11,
            Op::LineTo(..) => 12,
            Op::CurveTo(..) => 13,
            Op::CurveToV(..) => 14,
            Op::CurveToY(..) => 15,
            Op::ClosePath => 16,
            Op::Rect(..) => 17,
            Op::Stroke => 18,
            Op::CloseStroke => 19,
            Op::Fill => 20,
            Op::FillEvenOdd => 21,
            Op::FillStroke => 22,
            Op::FillStrokeEvenOdd => 23,
            Op::CloseFillStroke => 24,
            Op::CloseFillStrokeEvenOdd => 25,
            Op::EndPath => 26,
            Op::ClipNonZero => 27,
            Op::ClipEvenOdd => 28,
            Op::SetStrokeColorSpace(..) => 29,
            Op::SetFillColorSpace(..) => 30,
            Op::SetStrokeColor(..) => 31,
            Op::SetStrokeColorN(..) => 32,
            Op::SetFillColor(..) => 33,
            Op::SetFillColorN(..) => 34,
            Op::SetStrokeGray(..) => 35,
            Op::SetFillGray(..) => 36,
            Op::SetStrokeRGB(..) => 37,
            Op::SetFillRGB(..) => 38,
            Op::SetStrokeCMYK(..) => 39,
            Op::SetFillCMYK(..) => 40,
            Op::BeginText => 41,
            Op::EndText => 42,
            Op::SetCharSpacing(..) => 43,
            Op::SetWordSpacing(..) => 44,
            Op::SetHorizScaling(..) => 45,
            Op::SetLeading(..) => 46,
            Op::SetFont(..) => 47,
            Op::SetGlyphWidth(..) => 48,
            Op::SetGlyphWidthBBox(..) => 49,
            Op::SetTextRender(..) => 50,
            Op::SetTextRise(..) => 51,
            Op::TextMove(..) => 52,
            Op::TextMoveSetLeading(..) => 53,
            Op::SetTextMatrix(..) => 54,
            Op::TextNextLine => 55,
            Op::ShowText(..) => 56,
            Op::ShowTextAdjusted(..) => 57,
            Op::NextLineShowText(..) => 58,
            Op::NextLineShowTextSpaced(..) => 59,
            Op::XObject(..) => 60,
            Op::InlineImage(..) => 61,
            Op::Shading(..) => 62,
            Op::MarkedContentPoint(..) => 63,
            Op::MarkedContentPointProps(..) => 64,
            Op::BeginMarkedContent(..) => 65,
            Op::BeginMarkedContentProps(..) => 66,
            Op::EndMarkedContent => 67,
            Op::BeginCompat => 68,
            Op::EndCompat => 69,
        }
    }

    fn sample_ops() -> Vec<Op> {
        let mut props = Dict::new();
        props.insert(name("MCID"), Object::Int(3));
        vec![
            Op::Save,
            Op::Restore,
            Op::Concat(m(1.0, 0.0, 0.0, 1.0, 10.5, 20.0)),
            Op::SetLineWidth(2.5),
            Op::SetLineCap(1),
            Op::SetLineJoin(2),
            Op::SetMiterLimit(3.5),
            Op::SetDash(vec![3.0, 1.5], 0.5),
            Op::SetRenderingIntent(name("Perceptual")),
            Op::SetFlatness(1.5),
            Op::SetExtGState(name("GS1")),
            Op::MoveTo(10.0, 20.0),
            Op::LineTo(-30.5, 40.0),
            Op::CurveTo(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
            Op::CurveToV(1.5, 2.5, 3.5, 4.5),
            Op::CurveToY(5.0, 6.0, 7.0, 8.0),
            Op::ClosePath,
            Op::Rect(72.0, 600.0, 100.0, 80.0),
            Op::Stroke,
            Op::CloseStroke,
            Op::Fill,
            Op::FillEvenOdd,
            Op::FillStroke,
            Op::FillStrokeEvenOdd,
            Op::CloseFillStroke,
            Op::CloseFillStrokeEvenOdd,
            Op::EndPath,
            Op::ClipNonZero,
            Op::ClipEvenOdd,
            Op::SetStrokeColorSpace(name("DeviceRGB")),
            Op::SetFillColorSpace(name("Pattern")),
            Op::SetStrokeColor(vec![1.0, 0.5, 0.25]),
            Op::SetStrokeColorN(vec![0.1, 0.2], Some(name("P2"))),
            Op::SetFillColor(vec![0.5]),
            Op::SetFillColorN(vec![0.2, 0.4, 0.6], None),
            Op::SetStrokeGray(0.3),
            Op::SetFillGray(0.7),
            Op::SetStrokeRGB(1.0, 0.0, 0.5),
            Op::SetFillRGB(0.0, 1.0, 0.0),
            Op::SetStrokeCMYK(0.0, 0.25, 0.5, 1.0),
            Op::SetFillCMYK(1.0, 0.0, 0.0, 0.125),
            Op::BeginText,
            Op::EndText,
            Op::SetCharSpacing(0.5),
            Op::SetWordSpacing(1.5),
            Op::SetHorizScaling(90.0),
            Op::SetLeading(14.5),
            Op::SetFont(name("F1"), 12.0),
            Op::SetGlyphWidth(1000.0, 0.0),
            Op::SetGlyphWidthBBox(1000.0, 0.0, 0.0, 0.0, 750.0, 700.0),
            Op::SetTextRender(3),
            Op::SetTextRise(4.5),
            Op::TextMove(72.0, 720.0),
            Op::TextMoveSetLeading(0.0, -14.0),
            Op::SetTextMatrix(m(2.0, 0.0, 0.0, 2.0, 50.5, 60.0)),
            Op::TextNextLine,
            Op::ShowText(b"Hi (there)\\".to_vec()),
            Op::ShowTextAdjusted(vec![
                TextItem::Str(b"He".to_vec()),
                TextItem::Offset(-120.0),
                TextItem::Str(b"llo".to_vec()),
                TextItem::Offset(33.5),
            ]),
            Op::NextLineShowText(b"next line".to_vec()),
            Op::NextLineShowTextSpaced(2.0, 3.0, b"spaced".to_vec()),
            Op::XObject(name("Im1")),
            image(
                &[
                    ("ColorSpace", Object::Name(name("DeviceGray"))),
                    ("Width", Object::Int(2)),
                    ("BitsPerComponent", Object::Int(8)),
                    ("Height", Object::Int(2)),
                ],
                &[0, 255, 128, 127],
            ),
            Op::Shading(name("Sh1")),
            Op::MarkedContentPoint(name("Tag")),
            Op::MarkedContentPointProps(name("Tag"), Object::Name(name("P"))),
            Op::BeginMarkedContent(name("Span")),
            Op::BeginMarkedContentProps(name("Span"), Object::Dict(props)),
            Op::EndMarkedContent,
            Op::BeginCompat,
            Op::EndCompat,
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        let samples = sample_ops();
        let covered: std::collections::BTreeSet<usize> =
            samples.iter().map(variant_index).collect();
        assert_eq!(covered, (0..VARIANT_COUNT).collect());
        for op in &samples {
            round_trip(std::slice::from_ref(op));
        }
        round_trip(&samples);
    }

    #[test]
    fn mixed_sequence_round_trips() {
        let mut props = Dict::new();
        props.insert(name("MCID"), Object::Int(0));
        let ops = vec![
            Op::Save,
            Op::Concat(m(0.5, 0.0, 0.0, 0.5, 300.0, 100.0)),
            Op::Rect(0.0, 0.0, 200.0, 200.0),
            Op::Fill,
            Op::BeginMarkedContentProps(name("P"), Object::Dict(props)),
            Op::BeginText,
            Op::SetFont(name("F1"), 12.0),
            Op::TextMove(72.0, 720.0),
            Op::ShowText(b"Hello, world".to_vec()),
            Op::ShowTextAdjusted(vec![
                TextItem::Str(b"kern".to_vec()),
                TextItem::Offset(-15.5),
                TextItem::Str(b"ed".to_vec()),
            ]),
            Op::EndText,
            Op::EndMarkedContent,
            Op::XObject(name("Im1")),
            Op::Restore,
        ];
        round_trip(&ops);
    }

    #[test]
    fn inline_image_hazardous_data_round_trips_via_declared_length() {
        let op = image(
            &[
                ("Width", Object::Int(3)),
                ("Height", Object::Int(1)),
                ("BitsPerComponent", Object::Int(8)),
                ("ColorSpace", Object::Name(name("DeviceRGB"))),
                ("L", Object::Int(9)),
            ],
            b"ab EI wxy",
        );
        round_trip(std::slice::from_ref(&op));
        round_trip(&[op, Op::MoveTo(1.0, 2.0)]);
    }

    #[test]
    fn parser_produced_length_image_round_trips() {
        let mut src = b"BI /W 3 /H 1 /BPC 8 /CS /RGB /L 9 ID ".to_vec();
        src.extend_from_slice(b"ab EI wxy");
        src.extend_from_slice(b" EI 1 2 m");
        let parsed = parse_content(&src).expect("source parses");
        assert_eq!(parsed.len(), 2);
        round_trip(&parsed);
    }

    #[test]
    fn inline_image_ei_boundary_data_round_trips_without_length() {
        let base = [("Width", Object::Int(1)), ("Height", Object::Int(1))];
        for data in [
            b"noEIhazard".as_slice(),
            b"EIx\x01".as_slice(),
            b"zxEI".as_slice(),
            b"tail ".as_slice(),
        ] {
            round_trip(std::slice::from_ref(&image(&base, data)));
        }
    }

    #[test]
    fn f32_hard_cases_survive_round_trip() {
        let ops = vec![
            Op::SetLineWidth(0.1),
            Op::SetLineWidth(1e-7),
            Op::SetLineWidth(-1e-7),
            Op::MoveTo(-0.25, -123.456),
            Op::SetDash(vec![0.1, 1e-7, -0.3], 16_777_216.0),
            Op::ShowTextAdjusted(vec![TextItem::Offset(-120.25), TextItem::Offset(1e-7)]),
            Op::SetTextRise(f32::MIN_POSITIVE),
            Op::SetCharSpacing(3.4e38),
        ];
        for op in &ops {
            round_trip(std::slice::from_ref(op));
        }
        round_trip(&ops);
    }

    #[test]
    fn empty_operand_containers_round_trip() {
        let ops = vec![
            Op::SetDash(vec![], 0.0),
            Op::ShowTextAdjusted(vec![]),
            Op::SetStrokeColor(vec![]),
            Op::SetStrokeColorN(vec![], None),
            Op::SetFillColorN(vec![], Some(name("P1"))),
            Op::InlineImage(ImageParams {
                dict: Dict::new(),
                data: Vec::new(),
            }),
        ];
        for op in &ops {
            round_trip(std::slice::from_ref(op));
        }
        round_trip(&ops);
    }

    #[test]
    fn layout_pins_exact_bytes() {
        assert_eq!(serialize_ops(&[Op::Save]), b"q\n");
        assert_eq!(
            serialize_ops(&[Op::Concat(m(1.0, 0.0, 0.0, 1.0, 10.0, 20.0))]),
            b"1 0 0 1 10 20 cm\n"
        );
        assert_eq!(
            serialize_ops(&[Op::SetDash(vec![3.0, 1.0], 0.5)]),
            b"[3 1] 0.5 d\n"
        );
        assert_eq!(
            serialize_ops(&[Op::ShowTextAdjusted(vec![
                TextItem::Str(b"He".to_vec()),
                TextItem::Offset(-120.0),
                TextItem::Str(b"llo".to_vec()),
            ])]),
            b"[(He) -120 (llo)] TJ\n"
        );
        assert_eq!(
            serialize_ops(&[Op::SetStrokeColorN(vec![0.1, 0.2], Some(name("P2")))]),
            b"0.1 0.2 /P2 SCN\n"
        );
        assert_eq!(
            serialize_ops(&[Op::NextLineShowTextSpaced(2.0, 3.0, b"spaced".to_vec())]),
            b"2 3 (spaced) \"\n"
        );
        assert_eq!(
            serialize_ops(&[Op::SetFont(name("F1"), 12.0)]),
            b"/F1 12 Tf\n"
        );
        assert_eq!(serialize_ops(&[Op::SetTextRender(2)]), b"2 Tr\n");
        let mut want = b"BI /Width 2 ID ".to_vec();
        want.extend_from_slice(&[0, 255]);
        want.extend_from_slice(b" EI\n");
        assert_eq!(
            serialize_ops(&[image(&[("Width", Object::Int(2))], &[0, 255])]),
            want
        );
    }
}
