//! Lazy iteration over a document's elements: the physical file structure
//! (header, indirect objects, cross-reference sections, trailer, startxref,
//! eof) with byte spans, and the logical document structure (pages, fonts,
//! images, annotations, content operators). ISO 32000 §7.5 (file structure)
//! and §7.7 (document structure).

use std::collections::VecDeque;

use crate::content::Op;
use crate::document::Document;
use crate::error::{Error, Result};
use crate::hash::FastMap;
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::xref::{parse_section_at, XrefEntry};

/// Byte range in the physical file, end-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

impl Span {
    /// A span from `start` (inclusive) to `end` (exclusive).
    pub fn new(start: u64, end: u64) -> Span {
        Span { start, end }
    }

    /// Number of bytes covered; inverted spans count as zero.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Kind of a cross-reference section (ISO 32000 §7.5.4 / §7.5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefKind {
    /// A classic `xref` table.
    Table,
    /// A cross-reference stream.
    Stream,
}

/// One element of a document, physical or logical.
#[derive(Debug, Clone)]
pub enum Element {
    /// The `%PDF-x.y` header.
    Header { version: (u8, u8), span: Span },
    /// One indirect object.
    IndirectObject {
        r: ObjRef,
        object: Object,
        /// Span of `N G obj … endobj` in the file. For objects stored in an
        /// object stream this is the container stream object's span.
        span: Span,
        /// For objects inside an object stream: the container's reference
        /// and this object's byte range within the *decoded* stream data.
        in_objstm: Option<(ObjRef, Span)>,
    },
    /// One cross-reference section (table or stream).
    XrefSection {
        kind: XrefKind,
        span: Span,
        entries: usize,
    },
    /// The trailer: the merged trailer dictionary plus the byte range of the
    /// newest trailer region (classic `trailer << … >>`, or the newest
    /// cross-reference stream object when no classic trailer exists).
    Trailer { dict: Dict, span: Span },
    /// The `startxref` keyword and its offset operand.
    StartXref { offset: u64, span: Span },
    /// The `%%EOF` marker.
    Eof { span: Span },

    /// One page (logical).
    Page { index: usize, r: ObjRef },
    /// One font referenced from a page's resources.
    Font {
        page: Option<usize>,
        r: ObjRef,
        subtype: Name,
        base_font: Option<Name>,
    },
    /// One image XObject referenced from a page's resources.
    Image {
        page: Option<usize>,
        r: ObjRef,
        width: u32,
        height: u32,
    },
    /// One annotation on a page.
    Annotation {
        page: usize,
        r: ObjRef,
        subtype: Name,
    },
    /// One content-stream operator of a page.
    ContentOp {
        page: usize,
        op: Op,
        /// Byte range within the page's decoded, concatenated content.
        span_in_content: Span,
    },
}

/// Selects which element layers [`crate::Document::elements`] yields.
#[derive(Debug, Clone)]
pub struct ElementOpts {
    /// Yield physical file-structure elements.
    pub physical: bool,
    /// Yield logical document-structure elements.
    pub logical: bool,
    /// Restrict logical elements to these 0-based page indices.
    pub pages: Option<Vec<usize>>,
    /// Yield [`Element::ContentOp`] items (high-volume; off by default).
    pub content_ops: bool,
}

impl Default for ElementOpts {
    fn default() -> Self {
        ElementOpts {
            physical: true,
            logical: true,
            pages: None,
            content_ops: false,
        }
    }
}

impl Document {
    /// Lazy iteration over the document's elements. Physical elements come
    /// in file order (header, objects by offset with object-stream members
    /// after their container, xref sections newest→oldest, trailer,
    /// startxref, eof); logical elements follow in document order. Nothing
    /// is parsed or decoded before it is yielded; an element that fails to
    /// parse yields `Err` for that item and iteration continues.
    pub fn elements(&self, opts: ElementOpts) -> Elements<'_> {
        Elements {
            doc: self,
            opts,
            stage: Stage::Start,
            container_spans: FastMap::default(),
        }
    }
}

/// Iterator state. Each `next()` parses at most one element.
pub struct Elements<'a> {
    doc: &'a Document,
    opts: ElementOpts,
    stage: Stage,
    /// File spans of already-parsed object-stream containers.
    container_spans: FastMap<u32, Span>,
}

enum Stage {
    Start,
    Objects {
        order: Vec<OrderEntry>,
        next: usize,
    },
    Sections {
        /// Offsets still to visit: the newest section first, then each
        /// section's hybrid `/XRefStm` (queued right after that section, so
        /// it is visited before the `/Prev` chain continues), then `/Prev`.
        pending: VecDeque<usize>,
        visited: Vec<usize>,
        /// Newest classic trailer span, or newest stream-section span.
        trailer_span: Option<Span>,
    },
    Trailer {
        span: Option<Span>,
    },
    StartXref,
    Eof,
    // Constructed (with real values) at the physical→logical handoff, but
    // not yet read anywhere: Task 8 fills in the logical-layer arm that
    // actually inspects `page`/`part`. Until then this only transitions
    // straight to `Done`, so rustc's dead_code lint sees the fields as
    // written but never read.
    #[allow(dead_code)]
    Logical {
        page: usize,
        part: PagePart,
    },
    Done,
}

/// Sub-state within one page during logical iteration (Task 8 fills the
/// variants in; the physical layer only needs the entry point).
enum PagePart {
    PageItself,
}

/// One object scheduled for physical iteration, pre-sorted by file position.
struct OrderEntry {
    num: u32,
    entry: XrefEntry,
    /// The object's own offset, or its container's offset for members.
    sort_offset: u64,
    /// 0 for in-file objects; 1 + member index for object-stream members,
    /// so members directly follow their container.
    sort_member: u64,
}

impl<'a> Iterator for Elements<'a> {
    type Item = Result<Element>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.stage {
                Stage::Start => {
                    let order = if self.opts.physical {
                        build_order(self.doc)
                    } else {
                        Vec::new()
                    };
                    let header = self.opts.physical.then(|| header_element(self.doc));
                    self.stage = Stage::Objects { order, next: 0 };
                    if let Some(Some(header)) = header {
                        return Some(Ok(header));
                    }
                }
                Stage::Objects { order, next } => {
                    if *next >= order.len() {
                        self.stage = if self.opts.physical {
                            Stage::Sections {
                                pending: find_startxref_offset(self.doc.bytes())
                                    .into_iter()
                                    .collect(),
                                visited: Vec::new(),
                                trailer_span: None,
                            }
                        } else {
                            Stage::Logical {
                                page: 0,
                                part: PagePart::PageItself,
                            }
                        };
                        continue;
                    }
                    let index = *next;
                    *next += 1;
                    // Copy the (Copy) num/entry out of the borrowed order
                    // slice first: `self.object_element` needs `&mut self`,
                    // which would otherwise conflict with the still-live
                    // borrow of `self.stage` (via `order`) that a reference
                    // used inline as call arguments would keep alive.
                    let (num, entry) = {
                        let e = &order[index];
                        (e.num, e.entry)
                    };
                    return Some(self.object_element(num, entry));
                }
                Stage::Sections {
                    pending,
                    visited,
                    trailer_span,
                } => {
                    let Some(off) = pending.pop_front() else {
                        self.stage = Stage::Trailer {
                            span: *trailer_span,
                        };
                        continue;
                    };
                    if visited.contains(&off) {
                        continue; // already visited via another path; skip
                    }
                    visited.push(off);
                    match parse_section_at(self.doc.bytes(), off) {
                        Ok(info) => {
                            if trailer_span.is_none() {
                                *trailer_span = info.trailer_span.or(Some(info.span));
                            }
                            let bytes_len = self.doc.bytes().len();
                            // Hybrid files: the classic trailer's /XRefStm
                            // names a supplementary cross-reference stream at
                            // the same revision. Queue it right after this
                            // section (and ahead of /Prev) so it is visited
                            // before the chain walks further back; stream
                            // sections never carry an /XRefStm of their own.
                            if let Some(xs) = info
                                .xrefstm
                                .and_then(|v| usize::try_from(v).ok())
                                .filter(|&o| o < bytes_len && !visited.contains(&o))
                            {
                                pending.push_back(xs);
                            }
                            if let Some(prev) = info
                                .prev
                                .and_then(|v| usize::try_from(v).ok())
                                .filter(|&o| o < bytes_len && !visited.contains(&o))
                            {
                                pending.push_back(prev);
                            }
                            let element = Element::XrefSection {
                                kind: info.kind,
                                span: info.span,
                                entries: info.xref.len(),
                            };
                            return Some(Ok(element));
                        }
                        Err(err) => {
                            // Salvage: report the broken section, then stop
                            // walking the whole chain.
                            pending.clear();
                            return Some(Err(err));
                        }
                    }
                }
                Stage::Trailer { span } => {
                    let span = *span;
                    self.stage = Stage::StartXref;
                    if let Some(span) = span {
                        return Some(Ok(Element::Trailer {
                            dict: self.doc.xref().trailer.clone(),
                            span,
                        }));
                    }
                }
                Stage::StartXref => {
                    self.stage = Stage::Eof;
                    if let Some(element) = startxref_element(self.doc.bytes()) {
                        return Some(Ok(element));
                    }
                }
                Stage::Eof => {
                    self.stage = Stage::Logical {
                        page: 0,
                        part: PagePart::PageItself,
                    };
                    if let Some(element) = eof_element(self.doc.bytes()) {
                        return Some(Ok(element));
                    }
                }
                Stage::Logical { .. } => {
                    // Task 8 implements the logical layer; until then it ends
                    // the iteration.
                    self.stage = Stage::Done;
                }
                Stage::Done => return None,
            }
        }
    }
}

impl<'a> Elements<'a> {
    /// Builds the `IndirectObject` element for one xref entry.
    fn object_element(&mut self, num: u32, entry: XrefEntry) -> Result<Element> {
        match entry {
            XrefEntry::Free => Err(Error::ObjectNotFound(num, 0)),
            XrefEntry::InFile { offset, .. } => {
                let offset = usize::try_from(offset)
                    .ok()
                    .filter(|&o| o < self.doc.bytes().len())
                    .ok_or(Error::ObjectNotFound(num, 0))?;
                let (r, object, span) = self.doc.object_at_spanned(offset)?;
                self.container_spans.insert(r.num, span);
                Ok(Element::IndirectObject {
                    r,
                    object,
                    span,
                    in_objstm: None,
                })
            }
            XrefEntry::InStream { stream_num, index } => {
                let container_span = self.container_span(stream_num)?;
                let stm = self.doc.objstm_handle(stream_num)?;
                let (object, (start, end)) = stm.object_spanned(index)?;
                Ok(Element::IndirectObject {
                    r: ObjRef { num, gen: 0 },
                    object,
                    span: container_span,
                    in_objstm: Some((
                        ObjRef {
                            num: stream_num,
                            gen: 0,
                        },
                        Span::new(start as u64, end as u64),
                    )),
                })
            }
        }
    }

    /// The file span of an object-stream container, parsed at most once.
    fn container_span(&mut self, stream_num: u32) -> Result<Span> {
        if let Some(span) = self.container_spans.get(&stream_num) {
            return Ok(*span);
        }
        let offset = match self.doc.xref().get(stream_num) {
            Some(XrefEntry::InFile { offset, .. }) => usize::try_from(offset)
                .ok()
                .filter(|&o| o < self.doc.bytes().len())
                .ok_or(Error::ObjectNotFound(stream_num, 0))?,
            _ => return Err(Error::ObjectNotFound(stream_num, 0)),
        };
        let (.., span) = self.doc.object_at_spanned(offset)?;
        self.container_spans.insert(stream_num, span);
        Ok(span)
    }
}

/// All live objects sorted into file order: in-file objects by offset, then
/// object-stream members grouped after their container by member index.
fn build_order(doc: &Document) -> Vec<OrderEntry> {
    let mut order: Vec<OrderEntry> = doc
        .xref()
        .iter()
        .filter_map(|(num, entry)| match entry {
            XrefEntry::Free => None,
            XrefEntry::InFile { offset, .. } => Some(OrderEntry {
                num,
                entry,
                sort_offset: offset,
                sort_member: 0,
            }),
            XrefEntry::InStream { stream_num, index } => {
                let container_offset = match doc.xref().get(stream_num) {
                    Some(XrefEntry::InFile { offset, .. }) => offset,
                    // A member whose container is missing sorts last and
                    // surfaces as Err from object_element.
                    None | Some(XrefEntry::Free) | Some(XrefEntry::InStream { .. }) => u64::MAX,
                };
                Some(OrderEntry {
                    num,
                    entry,
                    sort_offset: container_offset,
                    sort_member: 1 + u64::from(index),
                })
            }
        })
        .collect();
    order.sort_by_key(|e| (e.sort_offset, e.sort_member, e.num));
    order
}

/// The `%PDF-x.y` header element, when a header is physically present.
fn header_element(doc: &Document) -> Option<Element> {
    let data = doc.bytes();
    let window = &data[..data.len().min(1024)];
    let pos = memchr::memmem::find(window, b"%PDF-")?;
    let digits_end = window[pos + 5..]
        .iter()
        .position(|&b| !(b.is_ascii_digit() || b == b'.'))
        .map(|rel| pos + 5 + rel)
        .unwrap_or(window.len());
    Some(Element::Header {
        version: doc.version(),
        span: Span::new(pos as u64, digits_end as u64),
    })
}

/// The byte offset announced by the last `startxref` keyword (the offset the
/// section walk starts from), bounded to the file.
fn find_startxref_offset(data: &[u8]) -> Option<usize> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"startxref")?;
    let mut lexer = Lexer::at(data, tail + rel + b"startxref".len());
    match lexer.next_token() {
        Ok(Token::Int(v)) => usize::try_from(v).ok().filter(|&o| o < data.len()),
        _ => None,
    }
}

/// The `startxref` element: keyword through its integer operand.
fn startxref_element(data: &[u8]) -> Option<Element> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"startxref")?;
    let start = tail + rel;
    let mut lexer = Lexer::at(data, start + b"startxref".len());
    match lexer.next_token() {
        Ok(Token::Int(v)) if v >= 0 => Some(Element::StartXref {
            offset: v as u64,
            span: Span::new(start as u64, lexer.pos() as u64),
        }),
        _ => None,
    }
}

/// The last `%%EOF` marker.
fn eof_element(data: &[u8]) -> Option<Element> {
    let tail = data.len().saturating_sub(64 * 1024);
    let rel = memchr::memmem::rfind(&data[tail..], b"%%EOF")?;
    let start = tail + rel;
    Some(Element::Eof {
        span: Span::new(start as u64, (start + b"%%EOF".len()) as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_opts_defaults() {
        let opts = ElementOpts::default();
        assert!(opts.physical);
        assert!(opts.logical);
        assert!(opts.pages.is_none());
        assert!(!opts.content_ops);
    }

    #[test]
    fn span_length_and_emptiness() {
        let span = Span::new(10, 25);
        assert_eq!(span.len(), 15);
        assert!(!span.is_empty());
        assert!(Span::new(7, 7).is_empty());
        assert_eq!(Span::new(9, 3).len(), 0);
    }

    use crate::document::Document;
    use crate::error::Result;
    use crate::object::ObjRef;
    use crate::parser::{NoResolve, Parser};

    fn physical(doc: &Document) -> Vec<Element> {
        let opts = ElementOpts {
            logical: false,
            ..ElementOpts::default()
        };
        doc.elements(opts).collect::<Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn simple_doc_physical_walk() {
        let data = pdfboss_testkit::simple_doc("walk");
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);

        let Element::Header { version, span } = &elements[0] else {
            panic!("first element must be the header, got {:?}", elements[0]);
        };
        assert_eq!(*version, (1, 7));
        assert!(doc.bytes()[span.start as usize..].starts_with(b"%PDF-1.7"));

        let mut object_count = 0usize;
        let mut previous_end = 0u64;
        for element in &elements {
            if let Element::IndirectObject {
                r,
                object,
                span,
                in_objstm,
            } = element
            {
                assert!(in_objstm.is_none());
                assert!(span.start >= previous_end, "objects come in file order");
                previous_end = span.end;
                let slice = &doc.bytes()[span.start as usize..span.end as usize];
                let (r2, object2) = Parser::new(slice).parse_indirect(&NoResolve).unwrap();
                assert_eq!(r2, *r);
                assert_eq!(object2, *object);
                object_count += 1;
            }
        }
        // `Xref::len` counts every entry including free ones (e.g. object 0's
        // free-list head); only non-free entries become `IndirectObject`s.
        let live_entries = doc
            .xref()
            .iter()
            .filter(|(_, entry)| !matches!(entry, crate::xref::XrefEntry::Free))
            .count();
        assert_eq!(object_count, live_entries);

        // Exactly one of each closing element, in order, after the objects.
        let tail_kinds: Vec<&str> = elements
            .iter()
            .filter_map(|e| match e {
                Element::XrefSection { .. } => Some("xref"),
                Element::Trailer { .. } => Some("trailer"),
                Element::StartXref { .. } => Some("startxref"),
                Element::Eof { .. } => Some("eof"),
                _ => None,
            })
            .collect();
        assert_eq!(tail_kinds, ["xref", "trailer", "startxref", "eof"]);

        for element in &elements {
            match element {
                Element::XrefSection {
                    kind,
                    span,
                    entries,
                } => {
                    assert_eq!(*kind, XrefKind::Table);
                    assert!(*entries > 0);
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"xref"));
                }
                Element::Trailer { dict, span } => {
                    assert!(dict.get("Root").is_some());
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"trailer"));
                }
                Element::StartXref { offset, span } => {
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"startxref"));
                    assert!(*offset > 0);
                }
                Element::Eof { span } => {
                    assert!(doc.bytes()[span.start as usize..].starts_with(b"%%EOF"));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn objstm_members_follow_their_container() {
        let data = pdfboss_testkit::objstm_doc(&[(7, "(seven)"), (8, "(eight)")]);
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);
        let order: Vec<(u32, bool)> = elements
            .iter()
            .filter_map(|e| match e {
                Element::IndirectObject { r, in_objstm, .. } => Some((r.num, in_objstm.is_some())),
                _ => None,
            })
            .collect();
        // Container 4 comes first (lowest offset), then its members in
        // index order (1, 2, 3, 7, 8), then the xref stream object 5.
        assert_eq!(
            order,
            [
                (4, false),
                (1, true),
                (2, true),
                (3, true),
                (7, true),
                (8, true),
                (5, false),
            ]
        );
        // Member spans index into the decoded container and reparse cleanly.
        for element in &elements {
            let Element::IndirectObject {
                object,
                in_objstm: Some((container, member_span)),
                ..
            } = element
            else {
                continue;
            };
            assert_eq!(*container, ObjRef { num: 4, gen: 0 });
            let stm = doc.objstm_handle(4).unwrap();
            let (reparsed, range) = stm
                .object_spanned(
                    // Recover the member's index by matching its span.
                    (0..)
                        .map(|i| (i, stm.object_spanned(i)))
                        .take_while(|pair| pair.1.is_ok())
                        .find(|pair| {
                            pair.1.as_ref().unwrap().1
                                == (member_span.start as usize, member_span.end as usize)
                        })
                        .map(|pair| pair.0)
                        .expect("member span maps to an index"),
                )
                .unwrap();
            assert_eq!(reparsed, *object);
            assert_eq!(
                range,
                (member_span.start as usize, member_span.end as usize)
            );
        }
    }

    #[test]
    fn xref_stream_docs_yield_stream_section_and_synthetic_trailer_span() {
        let data = pdfboss_testkit::objstm_doc(&[]);
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);
        let section = elements
            .iter()
            .find_map(|e| match e {
                Element::XrefSection { kind, span, .. } => Some((*kind, *span)),
                _ => None,
            })
            .expect("xref section present");
        assert_eq!(section.0, XrefKind::Stream);
        let trailer = elements
            .iter()
            .find_map(|e| match e {
                Element::Trailer { dict, span } => Some((dict.clone(), *span)),
                _ => None,
            })
            .expect("trailer present");
        assert!(trailer.0.get("Root").is_some());
        // No classic trailer keyword exists: the trailer span is the newest
        // xref stream object's span.
        assert_eq!(trailer.1, section.1);
    }

    /// Builds a minimal hybrid-reference file: a classic `xref` table whose
    /// trailer names `/XRefStm`, pointing at a separate cross-reference
    /// stream object. Adapted from (not shared with) the hybrid fixture in
    /// `xref::tests`, which is private to its own test module.
    fn hybrid_xrefstm_doc() -> Vec<u8> {
        let mut data = b"%PDF-1.5\n".to_vec();
        let obj1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let obj2 = data.len();
        data.extend_from_slice(b"2 0 obj\n(hidden)\nendobj\n");
        let stm_off = data.len();
        let mut fields = Vec::new();
        for offset in [obj2, stm_off] {
            fields.push(1u8);
            fields.extend_from_slice(&(offset as u32).to_be_bytes());
            fields.extend_from_slice(&0u16.to_be_bytes());
        }
        data.extend_from_slice(
            format!(
                "3 0 obj\n<< /Type /XRef /Size 4 /W [1 4 2] /Index [2 1 3 1] \
                 /Root 1 0 R /Length {} >>\nstream\n",
                fields.len()
            )
            .as_bytes(),
        );
        data.extend_from_slice(&fields);
        data.extend_from_slice(b"\nendstream\nendobj\n");
        let classic_off = data.len();
        data.extend_from_slice(b"xref\n0 3\n0000000000 65535 f\r\n");
        data.extend_from_slice(format!("{obj1:010} 00000 n\r\n").as_bytes());
        data.extend_from_slice(b"0000000000 00001 f\r\n"); // object 2 hidden
        data.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R /XRefStm {stm_off} >>\n").as_bytes(),
        );
        data.extend_from_slice(format!("startxref\n{classic_off}\n%%EOF\n").as_bytes());
        data
    }

    #[test]
    fn hybrid_xrefstm_yields_both_sections() {
        let data = hybrid_xrefstm_doc();
        let doc = Document::load(data).unwrap();
        let elements = physical(&doc);

        let sections: Vec<XrefKind> = elements
            .iter()
            .filter_map(|e| match e {
                Element::XrefSection { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            sections,
            [XrefKind::Table, XrefKind::Stream],
            "classic section first, then its hybrid /XRefStm section"
        );

        let trailers: Vec<Span> = elements
            .iter()
            .filter_map(|e| match e {
                Element::Trailer { span, .. } => Some(*span),
                _ => None,
            })
            .collect();
        assert_eq!(trailers.len(), 1, "exactly one trailer element");

        // Independently re-derive the classic section's trailer region and
        // confirm the emitted Trailer element's span matches it exactly.
        let startxref = memchr::memmem::rfind(doc.bytes(), b"startxref").unwrap();
        let mut lexer = Lexer::at(doc.bytes(), startxref + b"startxref".len());
        let classic_off = match lexer.next_token().unwrap() {
            Token::Int(v) => v as usize,
            other => panic!("expected startxref offset, got {other:?}"),
        };
        let info = parse_section_at(doc.bytes(), classic_off).unwrap();
        assert_eq!(info.kind, XrefKind::Table);
        let expected_trailer_span = info.trailer_span.expect("classic section has a trailer");
        assert_eq!(trailers[0], expected_trailer_span);
    }

    #[test]
    fn broken_object_yields_err_and_iteration_continues() {
        let mut builder = pdfboss_testkit::PdfBuilder::new();
        builder.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        builder.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        builder.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        builder.object(6, "<< /Broken >>");
        let mut data = builder.build(1);
        // Corrupt object 6's header in place: same length, no valid parse.
        let pos = memchr::memmem::find(&data, b"6 0 obj").unwrap();
        data[pos..pos + 7].copy_from_slice(b"6 ) obj");
        let doc = Document::load(data).unwrap();
        let opts = ElementOpts {
            logical: false,
            ..ElementOpts::default()
        };
        let items: Vec<Result<Element>> = doc.elements(opts).collect();
        assert!(
            items.iter().any(|i| i.is_err()),
            "corrupt object surfaces as Err"
        );
        let good: Vec<u32> = items
            .iter()
            .filter_map(|i| match i {
                Ok(Element::IndirectObject { r, .. }) => Some(r.num),
                _ => None,
            })
            .collect();
        for num in [1u32, 2, 3] {
            assert!(good.contains(&num), "object {num} still iterates");
        }
        assert!(items.iter().any(|i| matches!(i, Ok(Element::Eof { .. }))));
    }
}
