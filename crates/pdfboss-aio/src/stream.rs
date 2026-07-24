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
use pdfboss_core::ObjRef;

use crate::document::AsyncDocument;
use crate::error::Result;

/// Async counterpart of core's sync element iterator. `Send`, so it can
/// drive work on multi-threaded runtimes.
pub struct ElementStream<'a> {
    inner: BoxStream<'a, Result<Element>>,
}

impl<'a> futures_core::Stream for ElementStream<'a> {
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
}

struct StreamState<'a> {
    doc: &'a AsyncDocument,
    work: VecDeque<WorkItem>,
    pending: VecDeque<Result<Element>>,
}

/// Builds the stream: the worklist is computed synchronously from state
/// the open flow already holds (no fetches); each work item is executed
/// only when the consumer polls for it.
pub(crate) fn element_stream(doc: &AsyncDocument, opts: ElementOpts) -> ElementStream<'_> {
    let state = StreamState {
        doc,
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
    work
}

/// Executes one work item, pushing its element(s) — or a salvage `Err` —
/// into the pending queue.
async fn produce(state: &mut StreamState<'_>, work: WorkItem) {
    let doc = state.doc;
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
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use pdfboss_core::elements::{Element, ElementOpts, XrefKind};
    use pdfboss_core::ObjRef;
    use pdfboss_testkit::simple_doc;

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
        let doc = AsyncDocument::from_bytes(simple_doc("send")).await.unwrap();
        let mut stream = assert_send(doc.elements(physical_opts()));
        assert!(stream.next().await.is_some());
    }
}
