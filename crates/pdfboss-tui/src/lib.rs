//! Interactive terminal explorer for PDF internals, implemented from
//! ISO 32000 on top of `pdfboss-aio`'s async document model.
//!
//! State machine (`app`), pane models (`tree`, `inspector`, `hexview`,
//! `preview`, `markdown`, `search`), key mapping (`input`) and rendering
//! (`ui`) are pure and unit-testable; only [`run`] touches the terminal. The
//! event loop `tokio::select!`s over the crossterm event stream, a
//! background-task message channel and a 100 ms tick, so long operations
//! (element streaming, hex fetches, search, preview rasterization) never
//! block input.

pub mod app;
pub mod hexview;
pub mod input;
pub mod inspector;
pub mod markdown;
pub mod preview;
pub mod search;
pub mod tree;
pub mod ui;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::ObjRef;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{App, Cmd, Msg};
use crate::hexview::{HexSource, WINDOW_BYTES};
use crate::inspector::InspectorPayload;
use crate::preview::{fit_scale, PreviewFrame};
use crate::search::{object_matches, SearchHit};
use crate::tree::TreeReq;

/// Elements per tree batch message.
const TREE_BATCH: usize = 64;

/// Turns crossterm's raw terminal-attach failure into an actionable
/// message when there is no real terminal to attach to, leaving any other
/// I/O error untouched.
///
/// `enable_raw_mode` opens `/dev/tty` whenever stdin isn't itself a tty (a
/// piped-stdio run under a test harness, a script, or a CI job); with no
/// controlling terminal at all that open fails `ENXIO` ("Device not
/// configured" -- raw OS error 6 on macOS, confirmed empirically: opening
/// `/dev/tty` with no controlling terminal at all reports 6, not 25), and
/// against a real non-tty device it fails `ENOTTY` ("Inappropriate ioctl
/// for device", 25 on both macOS and Linux). Neither maps to a stable,
/// matchable `io::ErrorKind` on stable Rust (macOS reports the
/// nightly-only `ErrorKind::Uncategorized` here), so the raw OS error
/// number is the only portable signal available.
fn friendly_no_tty_error(err: std::io::Error) -> std::io::Error {
    match err.raw_os_error() {
        Some(6) | Some(25) => {
            std::io::Error::new(err.kind(), "pdfboss tui requires an interactive terminal")
        }
        _ => err,
    }
}

/// Restores the terminal on drop, so panics and early returns never leave
/// the shell in raw mode.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        disable_raw_mode().ok();
        execute!(std::io::stdout(), LeaveAlternateScreen).ok();
    }
}

/// Runs the explorer until the user quits. `doc` supplies all data (file-
/// or HTTP-backed); `title` labels the status bar. Document-level errors
/// become status-bar toasts; only terminal I/O errors are returned.
pub async fn run(doc: AsyncDocument, title: String) -> std::io::Result<()> {
    enable_raw_mode().map_err(friendly_no_tty_error)?;
    // The guard is constructed before `EnterAlternateScreen` (not after) so
    // that an early return from *that* fallible call still restores raw
    // mode: a local already constructed at the point of an early `?` return
    // is dropped, so there is no window where raw mode is enabled but
    // unguarded.
    let guard = TerminalGuard;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let size = terminal.size()?;
    let mut app = App::new(
        title,
        doc.version(),
        doc.page_count(),
        (size.width, size.height),
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let search_epoch = Arc::new(AtomicU64::new(0));
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(|frame| ui::draw(&app, frame))?;
        let msg = tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => Msg::Key(key),
                Some(Ok(Event::Resize(width, height))) => Msg::Resize(width, height),
                Some(Ok(..)) => continue,
                Some(Err(..)) | None => break,
            },
            Some(msg) = rx.recv() => msg,
            _ = tick.tick() => Msg::Tick,
        };
        for cmd in app.update(msg) {
            execute_cmd(&doc, &tx, &search_epoch, cmd);
        }
        if app.should_quit {
            break;
        }
    }
    drop(guard);
    Ok(())
}

/// Spawns the background task a [`Cmd`] describes; completions come back
/// to the loop as [`Msg`]s on the channel.
fn execute_cmd(
    doc: &AsyncDocument,
    tx: &UnboundedSender<Msg>,
    search_epoch: &Arc<AtomicU64>,
    cmd: Cmd,
) {
    let doc = doc.clone();
    let tx = tx.clone();
    match cmd {
        Cmd::LoadTree(req) => {
            tokio::spawn(load_tree(doc, tx, req));
        }
        Cmd::LoadContents { page, r } => {
            tokio::spawn(load_contents(doc, tx, page, r));
        }
        Cmd::LoadObject { generation, r } => {
            tokio::spawn(async move {
                let message = match doc.get_object(r).await {
                    Ok(object) => Msg::InspectorLoaded {
                        generation,
                        payload: InspectorPayload::Object { r, object },
                    },
                    Err(error) => Msg::InspectorFailed {
                        generation,
                        error: error.to_string(),
                    },
                };
                tx.send(message).ok();
            });
        }
        Cmd::DecodeStream { generation, r } => {
            tokio::spawn(async move {
                let message = match decoded_stream_data(&doc, r).await {
                    Ok((data, passthrough)) => Msg::InspectorLoaded {
                        generation,
                        payload: InspectorPayload::Decoded {
                            r,
                            data,
                            passthrough,
                        },
                    },
                    Err(error) => Msg::InspectorFailed { generation, error },
                };
                tx.send(message).ok();
            });
        }
        Cmd::LoadHex {
            generation,
            source,
            window_start,
        } => {
            tokio::spawn(load_hex(doc, tx, generation, source, window_start));
        }
        Cmd::StartSearch { generation, query } => {
            let epoch = Arc::clone(search_epoch);
            epoch.store(generation, Ordering::SeqCst);
            tokio::spawn(run_search(doc, tx, epoch, generation, query));
        }
        Cmd::CancelSearch { generation } => {
            search_epoch.store(generation, Ordering::SeqCst);
        }
        Cmd::RenderPreview {
            generation,
            page,
            max_w,
            max_h,
            file_bytes,
        } => {
            tokio::spawn(render_preview(
                doc, tx, generation, page, max_w, max_h, file_bytes,
            ));
        }
        Cmd::ExtractMarkdown { generation, page } => {
            tokio::spawn(extract_markdown(doc, tx, generation, page));
        }
    }
}

/// Whether a completed tree-population pass should be reported as an
/// outright failure (`Msg::TreeFailed`) instead of a normal `done` batch:
/// the pass delivered *zero* elements in total and recorded at least one
/// parse error along the way (total salvage failure — nothing usable came
/// out of it). A pass that delivered any real elements keeps the existing
/// partial-salvage behavior, even when it also recorded errors.
fn pass_failed(total_elements: usize, total_errors: usize) -> bool {
    total_elements == 0 && total_errors > 0
}

/// Streams a tree section's elements in batches. Per-element parse errors
/// are counted, never fatal (salvage semantics: a document with an
/// unusable logical layer still explores physically).
async fn load_tree(doc: AsyncDocument, tx: UnboundedSender<Msg>, req: TreeReq) {
    let opts = match req {
        TreeReq::Physical => ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        },
        TreeReq::Logical => ElementOpts {
            physical: false,
            logical: true,
            pages: None,
            content_ops: false,
        },
        // Contents folders load through `load_contents`.
        TreeReq::Contents { .. } => return,
    };
    let mut stream = doc.elements(opts);
    let mut batch: Vec<Element> = Vec::new();
    let mut errors = 0usize;
    // Totals persist across mid-stream flushes (which reset `batch` and
    // `errors` below) so the end-of-pass decision sees the whole pass,
    // not just the last unflushed chunk.
    let mut total_elements = 0usize;
    let mut total_errors = 0usize;
    while let Some(item) = stream.next().await {
        match item {
            Ok(element) => {
                batch.push(element);
                total_elements += 1;
            }
            Err(..) => {
                errors += 1;
                total_errors += 1;
            }
        }
        if batch.len() >= TREE_BATCH {
            let elements = std::mem::take(&mut batch);
            let batch_errors = std::mem::take(&mut errors);
            let sent = tx.send(Msg::TreeBatch {
                req,
                elements,
                errors: batch_errors,
                done: false,
            });
            if sent.is_err() {
                return;
            }
        }
    }
    if pass_failed(total_elements, total_errors) {
        // Total salvage failure: nothing usable ever came out of this
        // pass. Emitting `TreeFailed` (instead of a `done: true` batch
        // with zero elements) marks the section Failed so a re-expand
        // retries the load, rather than looking permanently empty.
        let error = format!("{total_errors} element(s) failed to parse, nothing salvaged");
        tx.send(Msg::TreeFailed { req, error }).ok();
    } else {
        tx.send(Msg::TreeBatch {
            req,
            elements: batch,
            errors,
            done: true,
        })
        .ok();
    }
}

/// Fetches a page dict and reports its `/Contents` refs (a single ref or
/// an array of refs).
async fn load_contents(doc: AsyncDocument, tx: UnboundedSender<Msg>, page: usize, r: ObjRef) {
    let message = match page_contents(&doc, r).await {
        Ok(refs) => Msg::ContentsLoaded { page, refs },
        Err(error) => Msg::ContentsFailed { page, error },
    };
    tx.send(message).ok();
}

async fn page_contents(doc: &AsyncDocument, r: ObjRef) -> Result<Vec<ObjRef>, String> {
    let object = doc.get_object(r).await.map_err(|error| error.to_string())?;
    let Some(dict) = object.as_dict() else {
        return Err(format!("object {} {} is not a page dict", r.num, r.gen));
    };
    let mut refs = Vec::new();
    match dict.get("Contents") {
        Some(pdfboss_core::Object::Ref(content_ref)) => refs.push(*content_ref),
        Some(pdfboss_core::Object::Array(items)) => {
            for item in items {
                if let pdfboss_core::Object::Ref(content_ref) = item {
                    refs.push(*content_ref);
                }
            }
        }
        Some(..) | None => {}
    }
    Ok(refs)
}

/// Decoded data of stream object `r`, plus the trailing image codec's name
/// when `decode_stream` leaves the bytes encoded for the image layer — the
/// Ops view labels such a passthrough instead of disassembling it.
async fn decoded_stream_data(
    doc: &AsyncDocument,
    r: ObjRef,
) -> Result<(Vec<u8>, Option<String>), String> {
    let object = doc.get_object(r).await.map_err(|error| error.to_string())?;
    let Some(stream) = object.as_stream() else {
        return Err(format!("object {} {} is not a stream", r.num, r.gen));
    };
    let passthrough = pdfboss_core::filters::trailing_filter_with(doc, &stream.dict)
        .await
        .filter(|name| pdfboss_core::filters::is_image_codec(&name.0))
        .map(|name| name.0);
    let data = doc
        .decode_stream(stream)
        .await
        .map_err(|error| error.to_string())?;
    Ok((data, passthrough))
}

/// Loads one hex window: a `read_span` window of a file span, or the whole
/// decoded object-stream container (decoded buffers are small).
async fn load_hex(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    generation: u64,
    source: HexSource,
    window_start: u64,
) {
    let outcome: Result<(u64, u64, Vec<u8>), String> = match source {
        HexSource::File { span } => {
            let total_len = span.end.saturating_sub(span.start);
            let start = span.start + window_start;
            let end = (start + WINDOW_BYTES as u64).min(span.end);
            match doc.read_span(Span { start, end }).await {
                Ok(bytes) => Ok((window_start, total_len, bytes)),
                Err(error) => Err(error.to_string()),
            }
        }
        HexSource::DecodedObjStm { container } => {
            match decoded_stream_data(&doc, container).await {
                Ok((bytes, _)) => Ok((0, bytes.len() as u64, bytes)),
                Err(error) => Err(error),
            }
        }
    };
    let message = match outcome {
        Ok((start, total_len, bytes)) => Msg::HexLoaded {
            generation,
            window_start: start,
            total_len,
            bytes,
        },
        Err(error) => Msg::HexFailed { generation, error },
    };
    tx.send(message).ok();
}

/// Visits physical objects lazily, streaming one message per match. A
/// newer search generation (shared epoch) terminates this task early.
async fn run_search(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    epoch: Arc<AtomicU64>,
    generation: u64,
    query: String,
) {
    let opts = ElementOpts {
        physical: true,
        logical: false,
        pages: None,
        content_ops: false,
    };
    let mut stream = doc.elements(opts);
    while let Some(item) = stream.next().await {
        if epoch.load(Ordering::SeqCst) != generation {
            return;
        }
        let Ok(Element::IndirectObject { r, object, .. }) = item else {
            continue;
        };
        if object_matches(&query, r.num, &object) {
            let hit = SearchHit { r };
            if tx.send(Msg::SearchResult { generation, hit }).is_err() {
                return;
            }
        }
    }
    tx.send(Msg::SearchDone { generation }).ok();
}

/// Renders a page preview. The whole file is fetched once (and cached by
/// the app for later renders); rasterization runs in `spawn_blocking`, and
/// the sync `Document` is created and dropped entirely inside the closure
/// (it is not `Send`).
async fn render_preview(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    generation: u64,
    page: usize,
    max_w: u32,
    max_h: u32,
    file_bytes: Option<Arc<Vec<u8>>>,
) {
    let bytes = match file_bytes {
        Some(bytes) => Ok(bytes),
        None => fetch_whole_file(&doc).await.map(Arc::new),
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            tx.send(Msg::PreviewReady {
                generation,
                result: Err(error),
            })
            .ok();
            return;
        }
    };
    let render_input = Arc::clone(&bytes);
    // The render is lenient: content pdfboss cannot read is skipped, so the
    // preview can come out blank with no error at all. The report's summary
    // rides along and becomes a status-bar toast.
    let rendered = tokio::task::spawn_blocking(
        move || -> Result<(pdfboss_render::Pixmap, Option<String>), String> {
            let document = pdfboss_core::Document::load(render_input.as_ref().clone())
                .map_err(|error| error.to_string())?;
            let page_object = document.page(page).map_err(|error| error.to_string())?;
            let (page_w, page_h) = page_object.size();
            let scale = fit_scale(page_w, page_h, max_w, max_h);
            let options = pdfboss_render::RenderOptions::default();
            let (pixmap, report) =
                pdfboss_render::render_page_reporting(&document, &page_object, scale, &options)
                    .map_err(|error| error.to_string())?;
            Ok((pixmap, report.summary()))
        },
    )
    .await;
    let result = match rendered {
        Ok(Ok((pixmap, notice))) => Ok(PreviewFrame {
            file_bytes: bytes,
            pixmap,
            notice,
        }),
        Ok(Err(error)) => Err(error),
        Err(join_error) => Err(join_error.to_string()),
    };
    tx.send(Msg::PreviewReady { generation, result }).ok();
}

/// Extracts one page as Markdown. Unlike the preview this needs no
/// whole-file fetch and no `spawn_blocking`: `extract_page_markdown_with`
/// runs directly over the `AsyncDocument`, which is `Send` and fetches
/// only the objects the page's text touches.
async fn extract_markdown(
    doc: AsyncDocument,
    tx: UnboundedSender<Msg>,
    generation: u64,
    page: usize,
) {
    let result = match doc.page(page) {
        Ok(page_object) => {
            let oc = doc.oc_state().await;
            pdfboss_output::extract_page_markdown_with(&doc, &page_object, oc.as_ref())
                .await
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    };
    tx.send(Msg::MarkdownReady { generation, result }).ok();
}

/// Fetches the entire file via one `read_span` over
/// `0..doc.file_len()` (the aio crate reports the length synchronously).
async fn fetch_whole_file(doc: &AsyncDocument) -> Result<Vec<u8>, String> {
    let end = doc.file_len();
    doc.read_span(Span { start: 0, end })
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_failed_true_only_when_zero_elements_and_some_errors() {
        assert!(
            pass_failed(0, 1),
            "zero elements with at least one error is total failure"
        );
        assert!(
            pass_failed(0, 5),
            "any positive error count still fails when no elements arrived"
        );
        assert!(!pass_failed(0, 0), "empty-but-clean pass is not a failure");
        assert!(
            !pass_failed(3, 2),
            "partial salvage: any real element wins over errors"
        );
        assert!(!pass_failed(3, 0), "clean pass with elements");
    }

    /// The markdown task end to end over the real async path: no
    /// whole-file fetch, no `spawn_blocking`, and the page's text comes
    /// back on the channel as a `MarkdownReady`.
    #[tokio::test]
    async fn extract_markdown_sends_the_page_text() {
        let doc = AsyncDocument::from_bytes(pdfboss_testkit::simple_doc("Hello"))
            .await
            .expect("fixture opens");
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        extract_markdown(doc, tx, 7, 0).await;
        match rx.try_recv().expect("one message") {
            Msg::MarkdownReady { generation, result } => {
                assert_eq!(generation, 7, "the request's generation rides along");
                assert!(
                    result.expect("extraction succeeds").contains("Hello"),
                    "the page's text must reach the pane"
                );
            }
            other => panic!("expected MarkdownReady, got {:?}", other),
        }
    }

    /// A page index the document does not have fails the task, not the
    /// event loop: the error travels as the message's `Err`.
    #[tokio::test]
    async fn extract_markdown_reports_a_missing_page_as_an_error() {
        let doc = AsyncDocument::from_bytes(pdfboss_testkit::simple_doc("Hello"))
            .await
            .expect("fixture opens");
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        extract_markdown(doc, tx, 1, 99).await;
        match rx.try_recv().expect("one message") {
            Msg::MarkdownReady { result, .. } => {
                assert!(result.is_err(), "page 99 does not exist");
            }
            other => panic!("expected MarkdownReady, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn fetch_whole_file_reads_exactly_file_len_bytes() {
        let data = pdfboss_testkit::simple_doc("Hello");
        let doc = AsyncDocument::from_bytes(data.clone())
            .await
            .expect("fixture opens");
        assert_eq!(doc.file_len(), data.len() as u64, "reported length");
        let fetched = fetch_whole_file(&doc).await.expect("whole-file fetch");
        assert_eq!(fetched, data, "fetch covers the entire file");
    }
}
