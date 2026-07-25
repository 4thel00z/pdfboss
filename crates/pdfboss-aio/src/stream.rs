//! A lazy element stream mirroring the sync iterator's ordering and
//! salvage semantics: physical elements first (header when present,
//! objects by offset with object-stream members after their container,
//! xref sections in chain order, one merged trailer, startxref, eof),
//! then logical elements in document order. Nothing is fetched, parsed or
//! decoded before it is yielded; logical elements are prepared one page
//! at a time.

use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::stream::BoxStream;
use pdfboss_core::elements::{Element, ElementOpts};
use pdfboss_core::xref::XrefEntry;
use pdfboss_core::{Dict, Name, ObjRef, Object};

use crate::document::{AsyncDocument, PageRecord};
use crate::error::{Error, Result};

/// Async counterpart of core's sync element iterator. `Send + 'static`
/// (it owns a cheap `Arc` clone of the document, not a borrow of it), so it
/// can drive work on multi-threaded runtimes and outlive the call that
/// created it — e.g. crossing a PyO3 binding boundary.
pub struct ElementStream {
    inner: BoxStream<'static, Result<Element>>,
}

impl futures_core::Stream for ElementStream {
    type Item = Result<Element>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

/// One unit of deferred work; producing an element (or a batch of logical
/// elements) may fetch and parse, which is exactly what laziness defers.
enum WorkItem {
    Header,
    InFile {
        r: ObjRef,
        offset: u64,
    },
    InStream {
        r: ObjRef,
        container: u32,
        index: u32,
    },
    Section(usize),
    Trailer,
    StartXref,
    Eof,
    Page(usize),
    PageResources(usize),
    PageContentOps(usize),
}

/// Owns a cheap `Arc` clone of the document (not a borrow): this is what
/// lets [`ElementStream`] be `'static`, un-tethered from the `AsyncDocument`
/// that created it.
struct StreamState {
    doc: AsyncDocument,
    work: VecDeque<WorkItem>,
    pending: VecDeque<Result<Element>>,
}

/// Builds the stream: the worklist is computed synchronously from state
/// the open flow already holds (no fetches); each work item is executed
/// only when the consumer polls for it.
pub(crate) fn element_stream(doc: &AsyncDocument, opts: ElementOpts) -> ElementStream {
    let state = StreamState {
        doc: doc.clone(),
        work: build_worklist(doc, &opts),
        pending: VecDeque::new(),
    };
    ElementStream {
        inner: Box::pin(futures_util::stream::unfold(
            state,
            |mut state| async move {
                loop {
                    if let Some(item) = state.pending.pop_front() {
                        return Some((item, state));
                    }
                    let work = state.work.pop_front()?;
                    produce(&mut state, work).await;
                }
            },
        )),
    }
}

/// One entry scheduled for physical object iteration, pre-sorted by file
/// position — the async mirror of core's `elements::OrderEntry`
/// (`build_order`), which is the parity arbiter for this ordering.
struct OrderEntry {
    num: u32,
    entry: XrefEntry,
    /// The object's own offset, or its container's offset for members.
    sort_offset: u64,
    /// 0 for in-file objects; 1 + member index for object-stream members,
    /// so members directly follow their container. Members whose container
    /// is missing, free, or itself in a stream sort last (`u64::MAX`
    /// `sort_offset`, checked at build time, per adopted CORE-PARITY rule).
    sort_member: u64,
}

/// Lays out the element order up front (cheap: xref entries and section
/// records are already in memory). Object order matches core's
/// `elements::build_order` exactly: `(sort_offset, sort_member, num)` —
/// in-file objects at their own offset (member 0), object-stream members
/// at their container's offset (`1 + index`) directly after it, and
/// members whose container is missing/free/itself-in-a-stream sorted last
/// (`u64::MAX`), where they yield `Err` once produced.
fn build_worklist(doc: &AsyncDocument, opts: &ElementOpts) -> VecDeque<WorkItem> {
    let mut work = VecDeque::new();
    if opts.physical {
        work.push_back(WorkItem::Header);
        let entries = doc.xref_entries();
        let by_num: HashMap<u32, XrefEntry> = entries.iter().copied().collect();
        let mut order: Vec<OrderEntry> = entries
            .into_iter()
            .filter_map(|(num, entry)| match entry {
                XrefEntry::Free => None,
                XrefEntry::InFile { offset, .. } => Some(OrderEntry {
                    num,
                    entry,
                    sort_offset: offset,
                    sort_member: 0,
                }),
                XrefEntry::InStream { stream_num, index } => {
                    let sort_offset = match by_num.get(&stream_num) {
                        Some(XrefEntry::InFile { offset, .. }) => *offset,
                        // Missing, free, or itself-in-a-stream containers
                        // have no bytes: their members sort last and yield
                        // Err once `produce` tries to fetch them.
                        _ => u64::MAX,
                    };
                    Some(OrderEntry {
                        num,
                        entry,
                        sort_offset,
                        sort_member: 1 + u64::from(index),
                    })
                }
            })
            .collect();
        order.sort_by_key(|e| (e.sort_offset, e.sort_member, e.num));
        for e in order {
            match e.entry {
                XrefEntry::InFile { offset, gen } => {
                    work.push_back(WorkItem::InFile {
                        r: ObjRef { num: e.num, gen },
                        offset,
                    });
                }
                XrefEntry::InStream { stream_num, index } => {
                    work.push_back(WorkItem::InStream {
                        r: ObjRef { num: e.num, gen: 0 },
                        container: stream_num,
                        index,
                    });
                }
                XrefEntry::Free => unreachable!("filtered out above"),
            }
        }
        // Sections in chain order (as stored by the open flow), then the
        // single merged trailer (adopted rule 4), startxref, eof.
        for section_index in 0..doc.sections().len() {
            work.push_back(WorkItem::Section(section_index));
        }
        work.push_back(WorkItem::Trailer);
        work.push_back(WorkItem::StartXref);
        work.push_back(WorkItem::Eof);
    }
    if opts.logical {
        for index in 0..doc.page_count() {
            if let Some(filter) = &opts.pages {
                if !filter.contains(&index) {
                    continue;
                }
            }
            work.push_back(WorkItem::Page(index));
            work.push_back(WorkItem::PageResources(index));
            if opts.content_ops {
                work.push_back(WorkItem::PageContentOps(index));
            }
        }
    }
    work
}

/// Executes one work item, pushing its element(s) — or a salvage `Err` —
/// into the pending queue.
async fn produce(state: &mut StreamState, work: WorkItem) {
    let doc = state.doc.clone();
    match work {
        WorkItem::Header => {
            if let Some(span) = doc.header_span() {
                state.pending.push_back(Ok(Element::Header {
                    version: doc.version(),
                    span,
                }));
            }
        }
        WorkItem::InFile { r, offset } => match doc.physical_object(r, offset).await {
            Ok((span, object)) => state.pending.push_back(Ok(Element::IndirectObject {
                r,
                object,
                span,
                in_objstm: None,
            })),
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::InStream {
            r,
            container,
            index,
        } => match doc.objstm_cache(container).await {
            Ok(cache) => {
                let member = cache.member_span(index).and_then(|member_span| {
                    cache.object(index).map(|object| (member_span, object))
                });
                match member {
                    Ok((member_span, object)) => {
                        state.pending.push_back(Ok(Element::IndirectObject {
                            r,
                            object,
                            span: cache.container_span,
                            in_objstm: Some((cache.container, member_span)),
                        }))
                    }
                    Err(err) => state.pending.push_back(Err(err)),
                }
            }
            Err(err) => state.pending.push_back(Err(err)),
        },
        WorkItem::Section(index) => {
            let record = &doc.sections()[index];
            state.pending.push_back(Ok(Element::XrefSection {
                kind: record.kind,
                span: record.span,
                entries: record.entries,
            }));
        }
        WorkItem::Trailer => {
            let (dict, span) = doc.merged_trailer();
            state.pending.push_back(Ok(Element::Trailer { dict, span }));
        }
        WorkItem::StartXref => {
            let (offset, span) = doc.startxref_record();
            state
                .pending
                .push_back(Ok(Element::StartXref { offset, span }));
        }
        WorkItem::Eof => {
            if let Some(span) = doc.eof_span() {
                state.pending.push_back(Ok(Element::Eof { span }));
            }
        }
        WorkItem::Page(index) => {
            if let Some(record) = doc.page_record(index) {
                if let Some(r) = record.r {
                    state.pending.push_back(Ok(Element::Page { index, r }));
                }
            }
        }
        WorkItem::PageResources(index) => logical_resources(state, index).await,
        WorkItem::PageContentOps(index) => content_ops(state, index).await,
    }
}

/// Produces a page's fonts, images and annotations (in that order; fonts
/// and images sorted by resource key name, annotations in `/Annots`
/// order — adopted rule 7). Only entries that are indirect references
/// yield elements; a font or annotation missing `/Subtype` still yields
/// its element with an empty name (lenient, pinned by the core iterator).
/// A resolve failure here can only be [`pdfboss_core::Error::CircularReference`]
/// (a missing or unreadable target instead resolves leniently to `Null`);
/// core's sync counterpart (`referenced_dict_entries`, the annotation loop)
/// silently skips such an entry rather than surfacing it, so this mirrors
/// that exactly — no salvage `Err` is pushed for it (CORE-PARITY).
async fn logical_resources(state: &mut StreamState, page: usize) {
    let doc = state.doc.clone();
    let Some(record) = doc.page_record(page) else {
        return;
    };
    let font_dict = resolved_category_dict(&doc, record.resources.get("Font")).await;
    for value in sorted_dict_values(font_dict.as_ref()) {
        let Some(r) = value.as_ref() else { continue };
        let Ok(resolved) = doc.resolve(&value).await else {
            continue; // CircularReference: skip, matching core exactly
        };
        let Some(dict) = resolved.as_dict() else {
            continue;
        };
        let subtype = dict
            .get_name("Subtype")
            .cloned()
            .unwrap_or_else(|| Name(String::new()));
        let base_font = dict.get_name("BaseFont").cloned();
        state.pending.push_back(Ok(Element::Font {
            page: Some(page),
            r,
            subtype,
            base_font,
        }));
    }
    let xobject_dict = resolved_category_dict(&doc, record.resources.get("XObject")).await;
    for value in sorted_dict_values(xobject_dict.as_ref()) {
        let Some(r) = value.as_ref() else { continue };
        let Ok(resolved) = doc.resolve(&value).await else {
            continue; // CircularReference: skip, matching core exactly
        };
        let Some(dict) = resolved.as_dict() else {
            continue;
        };
        if dict.get_name("Subtype").map(|n| n.0.as_str()) != Some("Image") {
            continue; // form XObjects are not image elements
        }
        let width = dict_u32(dict, "Width");
        let height = dict_u32(dict, "Height");
        state.pending.push_back(Ok(Element::Image {
            page: Some(page),
            r,
            width,
            height,
        }));
    }
    let annotations = match record.dict.get("Annots") {
        Some(value) => match doc.resolve(value).await {
            Ok(Object::Array(items)) => items,
            // Both a non-array result and a resolve failure
            // (CircularReference) yield no annotations, matching core's
            // `if let Ok(Object::Array(items)) = self.doc.resolve(annots)`.
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    for item in annotations {
        let Some(r) = item.as_ref() else { continue };
        let Ok(resolved) = doc.resolve(&item).await else {
            continue; // CircularReference: skip, matching core exactly
        };
        let Some(dict) = resolved.as_dict() else {
            continue;
        };
        let subtype = dict
            .get_name("Subtype")
            .cloned()
            .unwrap_or_else(|| Name(String::new()));
        state
            .pending
            .push_back(Ok(Element::Annotation { page, r, subtype }));
    }
}

/// Resolves a resource-category value (e.g. the `/Font` entry of
/// `/Resources`) to its dictionary. The category itself may be an
/// indirect reference (legal PDF, e.g. `/Font 9 0 R`), so it must be
/// resolved before being read as a dict — mirroring core's
/// `referenced_dict_entries`. Lenient: a missing category, a resolve
/// failure, or a non-dict result all yield `None` (no elements, no
/// salvage `Err`), exactly as core's sync counterpart drops them.
async fn resolved_category_dict(doc: &AsyncDocument, value: Option<&Object>) -> Option<Dict> {
    let value = value?;
    doc.resolve(value).await.ok()?.as_dict().cloned()
}

/// Values of an optional dictionary, sorted by key name (deterministic
/// logical ordering — adopted rule 7).
fn sorted_dict_values(dict: Option<&Dict>) -> Vec<Object> {
    let Some(dict) = dict else {
        return Vec::new();
    };
    let mut entries: Vec<(String, Object)> = dict
        .iter()
        .map(|(key, value)| (key.0.clone(), value.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|entry| entry.1).collect()
}

/// `dict[key]` as a `u32`, defaulting to 0 when missing or not a direct
/// integer (adopted rule 7). Deliberately does not resolve indirect
/// references: core's committed `page_elements` reads `Width`/`Height`
/// with a plain `Dict::get_int` (no resolve), so an indirect value there
/// is treated as invalid and defaults to 0 — this mirrors that exactly
/// (CORE-PARITY).
fn dict_u32(dict: &Dict, key: &str) -> u32 {
    dict.get_int(key)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0)
}

/// Produces a page's content operators with their byte ranges within the
/// decoded, concatenated content stream (adopted rule 8). Parsing itself
/// is delegated to core's own `parse_content_spanned` — the exact function
/// core's `page_elements` calls — so op/span boundaries (including inline
/// images and unknown-operator drops) can never diverge from core.
async fn content_ops(state: &mut StreamState, page: usize) {
    let doc = state.doc.clone();
    let Some(record) = doc.page_record(page) else {
        return;
    };
    let decoded = match page_content(&doc, &record).await {
        Ok(decoded) => decoded,
        Err(err) => {
            state.pending.push_back(Err(err));
            return;
        }
    };
    match pdfboss_core::content::parse_content_spanned(&decoded) {
        Ok(spanned) => {
            for (op, span) in spanned {
                state.pending.push_back(Ok(Element::ContentOp {
                    page,
                    op,
                    span_in_content: span,
                }));
            }
        }
        Err(err) => state.pending.push_back(Err(Error::Core(err))),
    }
}

/// The page's decoded content: the `/Contents` stream, or all streams of a
/// `/Contents` array decoded and joined with `b"\n"`, mirroring the sync
/// page API. A missing `/Contents` yields empty content (lenient).
async fn page_content(doc: &AsyncDocument, record: &PageRecord) -> Result<Vec<u8>> {
    let Some(contents) = record.dict.get("Contents") else {
        return Ok(Vec::new());
    };
    match doc.resolve(contents).await? {
        Object::Stream(ref s) => doc.decode_stream(s).await,
        Object::Array(items) => {
            let mut out = Vec::new();
            let mut first = true;
            for item in &items {
                let part = doc.resolve(item).await?;
                let Some(stream) = part.as_stream() else {
                    continue; // non-stream entries are skipped (lenient)
                };
                if !first {
                    out.push(b'\n');
                }
                out.extend_from_slice(&doc.decode_stream(stream).await?);
                first = false;
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use pdfboss_core::elements::{Element, ElementOpts, Span, XrefKind};
    use pdfboss_core::ObjRef;
    use pdfboss_testkit::{multi_page_doc, simple_doc, PdfBuilder};

    use crate::document::AsyncDocument;
    use crate::error::Result;

    fn physical_opts() -> ElementOpts {
        ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        }
    }

    async fn collect(doc: &AsyncDocument, opts: ElementOpts) -> Vec<Result<Element>> {
        let mut stream = doc.elements(opts);
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }
        items
    }

    #[tokio::test]
    async fn physical_sequence_shape_for_a_classic_document() {
        let data = simple_doc("elements");
        let file_len = data.len() as u64;
        // The fixture's `%%EOF` is followed by a trailing newline, so the
        // marker's own span (mirroring core's `eof_element`) ends short of
        // `file_len` by exactly that byte.
        let eof_pos = data
            .windows(b"%%EOF".len())
            .position(|w| w == b"%%EOF")
            .unwrap() as u64;
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let elements: Vec<Element> = collect(&doc, physical_opts())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        // `%PDF-1.7` at offset 0: the header span covers the version run.
        assert!(matches!(
            elements[0],
            Element::Header { version: (1, 7), span } if span.start == 0 && span.end == 8
        ));
        let object_numbers: Vec<u32> = elements
            .iter()
            .filter_map(|el| match el {
                Element::IndirectObject { r, .. } => Some(r.num),
                _ => None,
            })
            .collect();
        assert_eq!(
            object_numbers,
            vec![1, 2, 3, 4, 5],
            "objects in offset order"
        );
        let mut previous_end = 0;
        for element in &elements {
            if let Element::IndirectObject { span, .. } = element {
                assert!(span.start >= previous_end, "object spans are disjoint");
                assert!(span.end <= file_len, "spans stay in bounds");
                previous_end = span.end;
            }
        }
        // Tail shape (adopted rule 4): xref, trailer, startxref, eof.
        assert!(matches!(
            elements[elements.len() - 4],
            Element::XrefSection {
                kind: XrefKind::Table,
                entries: 6,
                ..
            }
        ));
        assert!(matches!(
            &elements[elements.len() - 3],
            Element::Trailer { dict, .. } if dict.get("Root").is_some()
        ));
        assert!(matches!(
            elements[elements.len() - 2],
            Element::StartXref { .. }
        ));
        assert!(matches!(
            elements[elements.len() - 1],
            Element::Eof { span } if span.start == eof_pos && span.end == eof_pos + 5
        ));
    }

    #[tokio::test]
    async fn objstm_members_follow_their_container() {
        let (dict, payload) = pdfboss_testkit::objstm_payload(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (5, "(member)"),
        ]);
        let mut b = pdfboss_testkit::PdfBuilder::new();
        b.stream(6, &dict, &payload);
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let doc = AsyncDocument::from_bytes(b.build_xref_stream(1))
            .await
            .unwrap();
        let elements: Vec<Element> = collect(&doc, physical_opts())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let object_sequence: Vec<(u32, bool)> = elements
            .iter()
            .filter_map(|el| match el {
                Element::IndirectObject { r, in_objstm, .. } => Some((r.num, in_objstm.is_some())),
                _ => None,
            })
            .collect();
        let container_pos = object_sequence
            .iter()
            .position(|&(num, is_member)| num == 6 && !is_member)
            .expect("container element present");
        assert_eq!(object_sequence[container_pos + 1], (1, true));
        assert_eq!(object_sequence[container_pos + 2], (5, true));
        let member = elements
            .iter()
            .find_map(|el| match el {
                Element::IndirectObject {
                    r,
                    in_objstm: Some((container, member_span)),
                    ..
                } if r.num == 5 => Some((*container, *member_span)),
                _ => None,
            })
            .expect("member element present");
        assert_eq!(member.0, ObjRef { num: 6, gen: 0 });
        assert!(member.1.start < member.1.end);
    }

    #[tokio::test]
    async fn broken_objects_yield_err_and_the_stream_continues() {
        // Corrupt one object header without moving offsets: object 5's
        // header keyword becomes garbage of equal length.
        let mut data = simple_doc("salvage");
        let pos = data
            .windows(b"5 0 obj".len())
            .position(|w| w == b"5 0 obj")
            .unwrap();
        data[pos..pos + 7].copy_from_slice(b"5 0 ob!");
        let doc = AsyncDocument::from_bytes(data).await.unwrap();
        let items = collect(&doc, physical_opts()).await;
        assert!(
            items.iter().any(|item| item.is_err()),
            "the bad object surfaces as Err"
        );
        let good: Vec<u32> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Element::IndirectObject { r, .. }) => Some(r.num),
                _ => None,
            })
            .collect();
        assert_eq!(good, vec![1, 2, 3, 4], "all other objects still stream");
        assert!(
            items
                .iter()
                .any(|item| matches!(item, Ok(Element::Eof { .. }))),
            "the stream runs to the end"
        );
    }

    #[tokio::test]
    async fn element_stream_is_send() {
        fn assert_send<T: Send>(value: T) -> T {
            value
        }
        // Proves `ElementStream` carries no borrow of the `AsyncDocument`
        // that created it (Plan 03/PyO3 needs `'static + Send` streams).
        fn requires_static<T: Send + 'static>(value: T) -> T {
            value
        }
        let doc = AsyncDocument::from_bytes(simple_doc("send")).await.unwrap();
        let mut stream = requires_static(assert_send(doc.elements(physical_opts())));
        drop(doc); // the stream must not depend on `doc` staying alive
        assert!(stream.next().await.is_some());
    }

    #[tokio::test]
    async fn logical_layer_lists_pages_fonts_images_annotations() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Im0 7 0 R >> >> \
             /Contents 4 0 R /Annots [8 0 R] >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (pic) Tj ET");
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.stream(
            7,
            "/Type /XObject /Subtype /Image /Width 2 /Height 3 \
             /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0u8; 6],
        );
        b.object(8, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts)
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        assert!(matches!(
            elements[0],
            Element::Page { index: 0, r } if r.num == 3
        ));
        assert!(matches!(
            &elements[1],
            Element::Font { page: Some(0), r, subtype, base_font: Some(base) }
                if r.num == 5 && subtype.0 == "Type1" && base.0 == "Helvetica"
        ));
        assert!(matches!(
            &elements[2],
            Element::Image { page: Some(0), r, width: 2, height: 3 } if r.num == 7
        ));
        assert!(matches!(
            &elements[3],
            Element::Annotation { page: 0, r, subtype }
                if r.num == 8 && subtype.0 == "Link"
        ));
        assert_eq!(elements.len(), 4);
    }

    #[tokio::test]
    async fn content_ops_spans_reslice_to_the_same_op() {
        let doc = AsyncDocument::from_bytes(simple_doc("ops")).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: true,
        };
        let items = collect(&doc, opts).await;
        // Recompute the decoded content the same way the sync page API does.
        let sync_doc = pdfboss_core::Document::load(simple_doc("ops")).unwrap();
        let decoded = sync_doc.page(0).unwrap().content(&sync_doc).unwrap();
        let ops: Vec<(pdfboss_core::content::Op, Span)> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Element::ContentOp {
                    op,
                    span_in_content,
                    ..
                }) => Some((op.clone(), *span_in_content)),
                _ => None,
            })
            .collect();
        assert!(!ops.is_empty());
        // The streamed op list matches a straight parse of the content.
        let expected = pdfboss_core::content::parse_content(&decoded).unwrap();
        let streamed: Vec<pdfboss_core::content::Op> =
            ops.iter().map(|entry| entry.0.clone()).collect();
        assert_eq!(streamed, expected);
        // Re-lexing each span yields exactly that op again.
        for (op, span) in &ops {
            let slice = &decoded[span.start as usize..span.end as usize];
            let reparsed = pdfboss_core::content::parse_content(slice).unwrap();
            assert_eq!(reparsed.len(), 1, "span {span:?} holds one op");
            assert_eq!(&reparsed[0], op);
        }
    }

    #[tokio::test]
    async fn pages_filter_restricts_the_logical_layer() {
        let doc = AsyncDocument::from_bytes(multi_page_doc(&["a", "b", "c"]))
            .await
            .unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: Some(vec![1]),
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts)
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let page_indices: Vec<usize> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Page { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(page_indices, vec![1]);
        assert!(elements.iter().all(|el| match el {
            Element::Font { page, .. } => *page == Some(1),
            _ => true,
        }));
    }

    #[tokio::test]
    async fn page_records_follow_document_order() {
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let doc = AsyncDocument::from_bytes(multi_page_doc(&["a", "b", "c"]))
            .await
            .unwrap();
        let elements: Vec<Element> = collect(&doc, opts.clone())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let async_pages: Vec<(usize, ObjRef)> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Page { index, r } => Some((*index, *r)),
                _ => None,
            })
            .collect();

        // Independently re-derive the same order via the sync core walk —
        // the parity arbiter for logical ordering.
        let sync_doc = pdfboss_core::Document::load(multi_page_doc(&["a", "b", "c"])).unwrap();
        let sync_pages: Vec<(usize, ObjRef)> = sync_doc
            .elements(opts)
            .collect::<pdfboss_core::Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .filter_map(|el| match el {
                Element::Page { index, r } => Some((index, r)),
                _ => None,
            })
            .collect();

        assert_eq!(
            async_pages, sync_pages,
            "page order and object refs match the sync core walk exactly"
        );
        assert_eq!(
            async_pages,
            vec![
                (0, ObjRef { num: 4, gen: 0 }),
                (1, ObjRef { num: 6, gen: 0 }),
                (2, ObjRef { num: 8, gen: 0 }),
            ]
        );
    }

    #[tokio::test]
    async fn inline_page_kid_yields_no_page_element() {
        // A page tree whose /Kids holds an inline (non-Ref) page dict as
        // its first child and a normal indirect reference as its second.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Kids [<< /Type /Page /Parent 2 0 R \
             /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> >> \
             3 0 R] /Count 2 >>",
        );
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>");
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let doc = AsyncDocument::from_bytes(b.build(1)).await.unwrap();
        assert_eq!(doc.page_count(), 2, "both children count as pages");
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts)
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let page_indices: Vec<usize> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Page { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(
            page_indices,
            vec![1],
            "the inline kid (index 0) yields no Page element; the Ref kid (index 1) still does"
        );
        let font_pages: Vec<Option<usize>> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Font { page, .. } => Some(*page),
                _ => None,
            })
            .collect();
        assert!(
            font_pages.contains(&Some(0)),
            "the inline page's own resources still yield child elements"
        );
    }

    #[tokio::test]
    async fn indirect_width_or_height_defaults_to_zero_matching_core() {
        // Core's committed `page_elements` reads Width/Height with a plain
        // `Dict::get_int` (no resolve): an indirect value there is
        // "invalid" and defaults to 0. This must hold here too, even
        // though the async layer resolves everything else through the
        // same `resolve()` path.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /XObject << /Im0 7 0 R >> >> >>",
        );
        b.object(9, "2"); // the indirect integer /Width points at
        b.stream(
            7,
            "/Type /XObject /Subtype /Image /Width 9 0 R /Height 3 \
             /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0u8; 6],
        );
        let bytes = b.build(1);
        let doc = AsyncDocument::from_bytes(bytes.clone()).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts.clone())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let image = elements
            .iter()
            .find_map(|el| match el {
                Element::Image { width, height, .. } => Some((*width, *height)),
                _ => None,
            })
            .expect("image element present");
        assert_eq!(
            image,
            (0, 3),
            "an indirect /Width is not resolved: it defaults to 0, matching core"
        );

        // Cross-check against the sync core walk directly.
        let sync_doc = pdfboss_core::Document::load(bytes).unwrap();
        let sync_image = sync_doc
            .elements(opts)
            .collect::<pdfboss_core::Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .find_map(|el| match el {
                Element::Image { width, height, .. } => Some((width, height)),
                _ => None,
            })
            .expect("sync image element present");
        assert_eq!(image, sync_image, "matches the sync core walk exactly");
    }

    #[tokio::test]
    async fn content_ops_match_core_across_varied_operators() {
        // End-to-end check of the async content-ops path (page-content
        // decode/concatenation, then core's own `parse_content_spanned`)
        // against the sync core walk on the same bytes: a TJ array
        // operand, an unrecognized operator (dropped, no arity match), and
        // an inline image, alongside plain operators.
        let content = "q 1 0 0 1 10 20 cm 0 0 10 10 re f Q \
                        BT /F1 12 Tf [(Hi) -250 (there)] TJ ET \
                        zzUnknownOp 1 2 \
                        BI /W 1 /H 1 /BPC 8 /CS /G ID \x01 EI";
        let bytes = pdfboss_testkit::doc_with_graphics(content);
        let doc = AsyncDocument::from_bytes(bytes.clone()).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: true,
        };
        let items = collect(&doc, opts).await;
        assert!(
            items.iter().all(|item| item.is_ok()),
            "no salvage errors expected on well-formed content: {items:?}"
        );
        let streamed: Vec<(pdfboss_core::content::Op, Span)> = items
            .iter()
            .filter_map(|item| match item {
                Ok(Element::ContentOp {
                    op,
                    span_in_content,
                    ..
                }) => Some((op.clone(), *span_in_content)),
                _ => None,
            })
            .collect();

        let sync_doc = pdfboss_core::Document::load(bytes).unwrap();
        let decoded = sync_doc.page(0).unwrap().content(&sync_doc).unwrap();
        let expected = pdfboss_core::content::parse_content_spanned(&decoded).unwrap();
        assert_eq!(
            streamed, expected,
            "op sequence and spans match core's own spanned parse exactly"
        );
    }

    #[tokio::test]
    async fn indirect_resource_category_still_enumerates() {
        // /Resources /Font is itself an indirect reference to the font
        // category dict (legal PDF) rather than an inline dict — the
        // category value must be resolved before its entries are read.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font 6 0 R >> >>",
        );
        b.object(6, "<< /F1 5 0 R >>");
        b.object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let bytes = b.build(1);
        let doc = AsyncDocument::from_bytes(bytes.clone()).await.unwrap();
        let opts = ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        };
        let elements: Vec<Element> = collect(&doc, opts.clone())
            .await
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let fonts: Vec<(ObjRef, String, Option<String>)> = elements
            .iter()
            .filter_map(|el| match el {
                Element::Font {
                    r,
                    subtype,
                    base_font,
                    ..
                } => Some((
                    *r,
                    subtype.0.clone(),
                    base_font.as_ref().map(|n| n.0.clone()),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            fonts,
            vec![(
                ObjRef { num: 5, gen: 0 },
                "Type1".to_string(),
                Some("Helvetica".to_string())
            )],
            "an indirect /Font category dict is still resolved and enumerated"
        );

        // Cross-check parity against the sync core walk on the same bytes.
        let sync_doc = pdfboss_core::Document::load(bytes).unwrap();
        let sync_fonts: Vec<(ObjRef, String, Option<String>)> = sync_doc
            .elements(opts)
            .collect::<pdfboss_core::Result<Vec<_>>>()
            .unwrap()
            .into_iter()
            .filter_map(|el| match el {
                Element::Font {
                    r,
                    subtype,
                    base_font,
                    ..
                } => Some((
                    r,
                    subtype.0.clone(),
                    base_font.as_ref().map(|n| n.0.clone()),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(fonts, sync_fonts, "matches the sync core walk exactly");
    }
}
