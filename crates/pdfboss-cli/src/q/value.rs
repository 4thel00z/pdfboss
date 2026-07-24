//! Conversion of a document's elements into a `serde_json::Value` tree — the
//! input to `pdfboss q` and `pdfboss json`. Wire format (pinned by the spec):
//! metadata keys are underscore-prefixed JSON keys (`_span`, `_ref`, `_kind`,
//! `_objstm`); indirect references are `{"_r": [num, gen]}`; names are plain
//! strings; PDF strings are UTF-8 where valid else `{"_bytes": "<base64>"}`;
//! streams are `{"_stream": {"dict": …, "length": N}}` with data embedded
//! only under `--raw` / `--decode`.
//!
//! ## Stability notes
//!
//! Three details of the wire format are documented here rather than pinned
//! by the spec, because they are either inherently non-contractual or easy
//! to misread as something they are not:
//!
//! - `content_ops[].op` is a best-effort rendering, currently Rust's
//!   `Debug` format of the operator (see `Element::ContentOp`). It is
//!   **not** a stable, parseable token: scripts must not rely on its exact
//!   text or structure. A future version may replace it with a stable
//!   operator name plus structured operands.
//! - Inside `_objstm`, the `span` key is bare (not `_span`) on purpose: it
//!   is a byte range within the *decoded* object-stream container, not a
//!   span in the file itself. `--hex` (which resolves `_span`/spans to file
//!   offsets) must never treat this `span` as a file offset — doing so
//!   would dump the wrong bytes entirely.
//! - Page objects carry a `content_ops` array only when content-op
//!   collection was requested (`--content-ops`); otherwise the key is
//!   absent from the page object, not present-and-empty. Callers checking
//!   for content ops must test for the key's presence, not just emptiness.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind};
use pdfboss_core::{Dict, Object, Stream};
use serde_json::{json, Map, Value};

/// How stream data is embedded in the value tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StreamData {
    /// Data omitted; only the dict and the stored byte count appear (default).
    Omit,
    /// `data` carries the raw (still encoded) bytes, base64.
    Raw,
    /// `data` carries the decoded bytes, base64.
    Decode,
}

/// Flags shared by `json` and `q` that shape the value tree.
pub struct TreeFlags {
    pub raw: bool,
    pub decode: bool,
    pub pages: Option<Vec<usize>>,
    pub no_logical: bool,
    pub content_ops: bool,
}

impl TreeFlags {
    /// `--decode` wins over `--raw` (clap marks them conflicting anyway).
    pub fn stream_data(&self) -> StreamData {
        if self.decode {
            StreamData::Decode
        } else if self.raw {
            StreamData::Raw
        } else {
            StreamData::Omit
        }
    }

    /// Maps the CLI flags onto core's `ElementOpts`. `--pages` is 1-based on
    /// the command line (matching `--page` elsewhere) and 0-based in core.
    pub fn element_opts(&self) -> Result<ElementOpts, String> {
        let pages = match &self.pages {
            None => None,
            Some(numbers) => {
                let mut indices = Vec::with_capacity(numbers.len());
                for &n in numbers {
                    if n == 0 {
                        return Err("--pages is 1-based; page 0 does not exist".to_string());
                    }
                    indices.push(n - 1);
                }
                Some(indices)
            }
        };
        Ok(ElementOpts {
            physical: true,
            logical: !self.no_logical,
            pages,
            content_ops: self.content_ops,
        })
    }
}

/// `[start, end]` per the wire format.
fn span_value(span: Span) -> Value {
    json!([span.start, span.end])
}

/// Converts one PDF object to JSON per the wire format.
pub fn object_to_value(
    obj: &Object,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    match obj {
        Object::Null => Value::Null,
        Object::Bool(b) => Value::Bool(*b),
        Object::Int(i) => Value::from(*i),
        Object::Real(r) => serde_json::Number::from_f64(*r)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Object::String(bytes) => string_to_value(bytes),
        Object::Name(name) => Value::String(name.0.clone()),
        Object::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| object_to_value(item, mode, decode))
                .collect(),
        ),
        Object::Dict(dict) => dict_to_value(dict, mode, decode),
        Object::Stream(s) => stream_to_value(s, mode, decode),
        Object::Ref(r) => json!({ "_r": [r.num, r.gen] }),
    }
}

fn string_to_value(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_string()),
        Err(_) => json!({ "_bytes": BASE64.encode(bytes) }),
    }
}

/// Dictionary entries sorted by name: core's `Dict` iteration order is not
/// deterministic, and dumps must be byte-stable across runs.
fn dict_to_value(
    dict: &Dict,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut entries: Vec<_> = dict.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut map = Map::new();
    for (name, value) in entries {
        map.insert(name.0.clone(), object_to_value(value, mode, decode));
    }
    Value::Object(map)
}

fn stream_to_value(
    s: &Stream,
    mode: StreamData,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut inner = Map::new();
    inner.insert("dict".to_string(), dict_to_value(&s.dict, mode, decode));
    inner.insert("length".to_string(), Value::from(s.data.len() as u64));
    match mode {
        StreamData::Omit => {}
        StreamData::Raw => {
            inner.insert("data".to_string(), Value::String(BASE64.encode(&s.data)));
        }
        StreamData::Decode => match decode(s) {
            Ok(data) => {
                inner.insert("data".to_string(), Value::String(BASE64.encode(&data)));
            }
            Err(message) => {
                inner.insert("decode_error".to_string(), Value::String(message));
            }
        },
    }
    let mut outer = Map::new();
    outer.insert("_stream".to_string(), Value::Object(inner));
    Value::Object(outer)
}

/// Per-page accumulator while walking the logical elements.
#[derive(Default)]
struct PageAcc {
    r: Option<Value>,
    fonts: Vec<Value>,
    images: Vec<Value>,
    annotations: Vec<Value>,
    content_ops: Vec<Value>,
}

/// Builds the full value tree: top-level `header`, `objects` (map keyed
/// `"N G"`), `pages`, `xref`, `trailer`, `startxref`. `include_content_ops`
/// controls whether page entries carry a `content_ops` array (the elements
/// only contain ops when `ElementOpts::content_ops` was set).
pub fn build_tree(
    elements: &[Element],
    mode: StreamData,
    include_content_ops: bool,
    decode: &mut dyn FnMut(&Stream) -> Result<Vec<u8>, String>,
) -> Value {
    let mut header = Value::Null;
    let mut objects = Map::new();
    let mut xref: Vec<Value> = Vec::new();
    let mut trailer = Value::Null;
    let mut startxref = Value::Null;
    // Elements stream xref sections in chain order (newest first), so the
    // active startxref is the one physically last in the file, not the last
    // one yielded.
    let mut startxref_pos: Option<u64> = None;
    let mut page_acc: std::collections::BTreeMap<usize, PageAcc> =
        std::collections::BTreeMap::new();

    for element in elements {
        match element {
            Element::Header { version, span } => {
                header = json!({
                    "version": format!("{}.{}", version.0, version.1),
                    "_span": span_value(*span),
                    "_kind": "header",
                });
            }
            Element::IndirectObject {
                r,
                object,
                span,
                in_objstm,
            } => {
                let objstm = match in_objstm {
                    None => Value::Null,
                    Some((container, inner)) => json!({
                        "_r": [container.num, container.gen],
                        "span": span_value(*inner),
                    }),
                };
                let entry = json!({
                    "_kind": "object",
                    "_ref": [r.num, r.gen],
                    "_span": span_value(*span),
                    "_objstm": objstm,
                    "value": object_to_value(object, mode, decode),
                });
                objects.insert(format!("{} {}", r.num, r.gen), entry);
            }
            Element::XrefSection {
                kind,
                span,
                entries,
            } => {
                let kind = match kind {
                    XrefKind::Table => "table",
                    XrefKind::Stream => "stream",
                };
                xref.push(json!({
                    "kind": kind,
                    "entries": *entries,
                    "_span": span_value(*span),
                }));
            }
            Element::Trailer { dict, span } => {
                // Emitted once per document (merged trailer dict; span is the
                // newest trailer region).
                trailer = json!({
                    "_span": span_value(*span),
                    "value": dict_to_value(dict, mode, decode),
                });
            }
            Element::StartXref { offset, span } => {
                if startxref_pos.is_none_or(|pos| span.start >= pos) {
                    startxref_pos = Some(span.start);
                    startxref = Value::from(*offset);
                }
            }
            Element::Eof { .. } => {}
            Element::Page { index, r } => {
                let acc = page_acc.entry(*index).or_default();
                acc.r = Some(json!([r.num, r.gen]));
            }
            Element::Font {
                page,
                r,
                subtype,
                base_font,
            } => {
                if let Some(page) = page {
                    page_acc.entry(*page).or_default().fonts.push(json!({
                        "_ref": [r.num, r.gen],
                        "subtype": subtype.0.clone(),
                        "base_font": base_font.as_ref().map(|n| n.0.clone()),
                    }));
                }
            }
            Element::Image {
                page,
                r,
                width,
                height,
            } => {
                if let Some(page) = page {
                    page_acc.entry(*page).or_default().images.push(json!({
                        "_ref": [r.num, r.gen],
                        "width": *width,
                        "height": *height,
                    }));
                }
            }
            Element::Annotation { page, r, subtype } => {
                page_acc.entry(*page).or_default().annotations.push(json!({
                    "_ref": [r.num, r.gen],
                    "subtype": subtype.0.clone(),
                }));
            }
            Element::ContentOp {
                page,
                op,
                span_in_content,
            } => {
                page_acc.entry(*page).or_default().content_ops.push(json!({
                    "op": format!("{op:?}"),
                    "_span_in_content": span_value(*span_in_content),
                }));
            }
        }
    }

    let pages: Vec<Value> = page_acc
        .into_iter()
        .map(|(index, acc)| {
            let mut page = Map::new();
            page.insert("index".to_string(), Value::from(index as u64));
            page.insert("_ref".to_string(), acc.r.unwrap_or(Value::Null));
            page.insert("fonts".to_string(), Value::Array(acc.fonts));
            page.insert("images".to_string(), Value::Array(acc.images));
            page.insert("annotations".to_string(), Value::Array(acc.annotations));
            if include_content_ops {
                page.insert("content_ops".to_string(), Value::Array(acc.content_ops));
            }
            Value::Object(page)
        })
        .collect();

    let mut root = Map::new();
    root.insert("header".to_string(), header);
    root.insert("objects".to_string(), Value::Object(objects));
    root.insert("pages".to_string(), Value::Array(pages));
    root.insert("xref".to_string(), Value::Array(xref));
    root.insert("trailer".to_string(), trailer);
    root.insert("startxref".to_string(), startxref);
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Name, ObjRef};

    fn no_decode() -> impl FnMut(&Stream) -> Result<Vec<u8>, String> {
        |s: &Stream| {
            let _ = s;
            Err("decode must not be called".to_string())
        }
    }

    fn plain_stream() -> Stream {
        let mut dict = Dict::new();
        dict.insert(Name("Length".to_string()), Object::Int(2));
        Stream {
            dict,
            data: b"hi".to_vec(),
        }
    }

    #[test]
    fn scalars_convert_directly() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(&Object::Null, StreamData::Omit, &mut decode),
            Value::Null
        );
        assert_eq!(
            object_to_value(&Object::Bool(true), StreamData::Omit, &mut decode),
            json!(true)
        );
        assert_eq!(
            object_to_value(&Object::Int(-42), StreamData::Omit, &mut decode),
            json!(-42)
        );
        assert_eq!(
            object_to_value(&Object::Real(1.5), StreamData::Omit, &mut decode),
            json!(1.5)
        );
        assert_eq!(
            object_to_value(
                &Object::Name(Name("Page".to_string())),
                StreamData::Omit,
                &mut decode
            ),
            json!("Page")
        );
        assert_eq!(
            object_to_value(
                &Object::Ref(ObjRef { num: 13, gen: 0 }),
                StreamData::Omit,
                &mut decode
            ),
            json!({ "_r": [13, 0] })
        );
    }

    #[test]
    fn nan_real_becomes_null() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(&Object::Real(f64::NAN), StreamData::Omit, &mut decode),
            Value::Null
        );
    }

    #[test]
    fn strings_are_utf8_or_base64_bytes() {
        let mut decode = no_decode();
        assert_eq!(
            object_to_value(
                &Object::String(b"hello".to_vec()),
                StreamData::Omit,
                &mut decode
            ),
            json!("hello")
        );
        assert_eq!(
            object_to_value(
                &Object::String(vec![0xff, 0xfe]),
                StreamData::Omit,
                &mut decode
            ),
            json!({ "_bytes": "//4=" })
        );
    }

    #[test]
    fn dict_keys_are_sorted_names() {
        let mut dict = Dict::new();
        dict.insert(Name("B".to_string()), Object::Int(2));
        dict.insert(Name("A".to_string()), Object::Int(1));
        let mut decode = no_decode();
        let v = object_to_value(&Object::Dict(dict), StreamData::Omit, &mut decode);
        // preserve_order keeps insertion order, so serialization proves the
        // conversion inserted keys sorted.
        assert_eq!(
            serde_json::to_string(&v).expect("serializes"),
            r#"{"A":1,"B":2}"#
        );
    }

    #[test]
    fn arrays_convert_recursively() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Array(vec![Object::Int(1), Object::Array(vec![Object::Int(2)])]),
            StreamData::Omit,
            &mut decode,
        );
        assert_eq!(v, json!([1, [2]]));
    }

    #[test]
    fn stream_data_is_omitted_by_default() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Omit,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2 } })
        );
    }

    #[test]
    fn raw_mode_embeds_raw_base64() {
        let mut decode = no_decode();
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Raw,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "data": "aGk=" } })
        );
    }

    #[test]
    fn decode_mode_embeds_decoded_base64() {
        let mut decode = |s: &Stream| {
            let _ = s;
            Ok(b"HI".to_vec())
        };
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Decode,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "data": "SEk=" } })
        );
    }

    #[test]
    fn decode_failure_is_reported_inline() {
        let mut decode = |s: &Stream| {
            let _ = s;
            Err("kaput".to_string())
        };
        let v = object_to_value(
            &Object::Stream(plain_stream()),
            StreamData::Decode,
            &mut decode,
        );
        assert_eq!(
            v,
            json!({ "_stream": { "dict": { "Length": 2 }, "length": 2, "decode_error": "kaput" } })
        );
    }

    #[test]
    fn build_tree_matches_the_wire_format() {
        let mut dict = Dict::new();
        dict.insert(
            Name("Type".to_string()),
            Object::Name(Name("Page".to_string())),
        );
        dict.insert(
            Name("Contents".to_string()),
            Object::Ref(ObjRef { num: 13, gen: 0 }),
        );
        let mut trailer_dict = Dict::new();
        trailer_dict.insert(
            Name("Root".to_string()),
            Object::Ref(ObjRef { num: 1, gen: 0 }),
        );
        trailer_dict.insert(Name("Size".to_string()), Object::Int(42));
        let elements = vec![
            Element::Header {
                version: (1, 7),
                span: Span { start: 0, end: 15 },
            },
            Element::IndirectObject {
                r: ObjRef { num: 12, gen: 0 },
                object: Object::Dict(dict),
                span: Span {
                    start: 6720,
                    end: 6914,
                },
                in_objstm: None,
            },
            Element::XrefSection {
                kind: XrefKind::Table,
                span: Span {
                    start: 7480,
                    end: 8322,
                },
                entries: 42,
            },
            Element::Trailer {
                dict: trailer_dict,
                span: Span {
                    start: 8322,
                    end: 8419,
                },
            },
            Element::StartXref {
                offset: 7480,
                span: Span {
                    start: 8419,
                    end: 8434,
                },
            },
            Element::Eof {
                span: Span {
                    start: 8434,
                    end: 8440,
                },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(
            tree,
            json!({
                "header": { "version": "1.7", "_span": [0, 15], "_kind": "header" },
                "objects": {
                    "12 0": {
                        "_kind": "object",
                        "_ref": [12, 0],
                        "_span": [6720, 6914],
                        "_objstm": null,
                        "value": { "Contents": {"_r": [13, 0]}, "Type": "Page" }
                    }
                },
                "pages": [],
                "xref": [ { "kind": "table", "entries": 42, "_span": [7480, 8322] } ],
                "trailer": { "_span": [8322, 8419], "value": { "Root": {"_r": [1, 0]}, "Size": 42 } },
                "startxref": 7480
            })
        );
    }

    #[test]
    fn missing_header_and_trailer_render_as_null() {
        let mut decode = no_decode();
        let tree = build_tree(&[], StreamData::Omit, false, &mut decode);
        assert_eq!(tree["header"], Value::Null);
        assert_eq!(tree["trailer"], Value::Null);
        assert_eq!(tree["startxref"], Value::Null);
        assert_eq!(tree["objects"], json!({}));
        assert_eq!(tree["pages"], json!([]));
        assert_eq!(tree["xref"], json!([]));
    }

    #[test]
    fn startxref_uses_the_physically_last_element_regardless_of_yield_order() {
        // Chain order yields the newest region first; the active startxref is
        // the one at the greatest file offset either way.
        let newest_first = vec![
            Element::StartXref {
                offset: 500,
                span: Span {
                    start: 900,
                    end: 915,
                },
            },
            Element::StartXref {
                offset: 100,
                span: Span {
                    start: 300,
                    end: 315,
                },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&newest_first, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["startxref"], json!(500));

        let oldest_first: Vec<Element> = newest_first.into_iter().rev().collect();
        let tree = build_tree(&oldest_first, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["startxref"], json!(500));
    }

    #[test]
    fn objstm_members_carry_container_and_inner_span() {
        let elements = vec![Element::IndirectObject {
            r: ObjRef { num: 1, gen: 0 },
            object: Object::Bool(true),
            span: Span {
                start: 100,
                end: 400,
            },
            in_objstm: Some((ObjRef { num: 6, gen: 0 }, Span { start: 20, end: 54 })),
        }];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(
            tree["objects"]["1 0"]["_objstm"],
            json!({ "_r": [6, 0], "span": [20, 54] })
        );
    }

    #[test]
    fn logical_elements_group_under_their_page() {
        let elements = vec![
            Element::Page {
                index: 0,
                r: ObjRef { num: 3, gen: 0 },
            },
            Element::Font {
                page: Some(0),
                r: ObjRef { num: 5, gen: 0 },
                subtype: Name("Type1".to_string()),
                base_font: Some(Name("Helvetica".to_string())),
            },
            Element::Image {
                page: Some(0),
                r: ObjRef { num: 7, gen: 0 },
                width: 100,
                height: 50,
            },
            Element::Annotation {
                page: 0,
                r: ObjRef { num: 9, gen: 0 },
                subtype: Name("Link".to_string()),
            },
            Element::ContentOp {
                page: 0,
                op: pdfboss_core::content::Op::Fill,
                span_in_content: Span { start: 4, end: 6 },
            },
        ];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, true, &mut decode);
        assert_eq!(
            tree["pages"],
            json!([{
                "index": 0,
                "_ref": [3, 0],
                "fonts": [ { "_ref": [5, 0], "subtype": "Type1", "base_font": "Helvetica" } ],
                "images": [ { "_ref": [7, 0], "width": 100, "height": 50 } ],
                "annotations": [ { "_ref": [9, 0], "subtype": "Link" } ],
                "content_ops": [ { "op": "Fill", "_span_in_content": [4, 6] } ]
            }])
        );

        let without_ops = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert!(without_ops["pages"][0].get("content_ops").is_none());
        assert_eq!(without_ops["pages"][0]["fonts"], tree["pages"][0]["fonts"]);
    }

    #[test]
    fn document_level_fonts_without_a_page_are_skipped() {
        let elements = vec![Element::Font {
            page: None,
            r: ObjRef { num: 5, gen: 0 },
            subtype: Name("Type1".to_string()),
            base_font: None,
        }];
        let mut decode = no_decode();
        let tree = build_tree(&elements, StreamData::Omit, false, &mut decode);
        assert_eq!(tree["pages"], json!([]));
    }

    #[test]
    fn tree_flags_map_to_element_opts() {
        let flags = TreeFlags {
            raw: false,
            decode: false,
            pages: Some(vec![1, 3]),
            no_logical: false,
            content_ops: true,
        };
        let opts = flags.element_opts().expect("valid pages");
        assert!(opts.physical);
        assert!(opts.logical);
        assert_eq!(opts.pages, Some(vec![0, 2]));
        assert!(opts.content_ops);

        let no_logical = TreeFlags {
            raw: false,
            decode: false,
            pages: None,
            no_logical: true,
            content_ops: false,
        };
        assert!(!no_logical.element_opts().expect("valid").logical);

        let zero = TreeFlags {
            raw: false,
            decode: false,
            pages: Some(vec![0]),
            no_logical: false,
            content_ops: false,
        };
        assert!(zero.element_opts().is_err());
    }

    #[test]
    fn stream_data_mode_precedence() {
        let base = |raw, decode| TreeFlags {
            raw,
            decode,
            pages: None,
            no_logical: false,
            content_ops: false,
        };
        assert_eq!(base(false, false).stream_data(), StreamData::Omit);
        assert_eq!(base(true, false).stream_data(), StreamData::Raw);
        assert_eq!(base(false, true).stream_data(), StreamData::Decode);
    }
}
