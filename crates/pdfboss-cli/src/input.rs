//! Shared input handling for the explorer subcommands (`json`, `hex`, `q`):
//! local files through the sync `Document` fast path, `http(s)` URLs through
//! the async HTTP backend on a small single-thread runtime.

use std::io::IsTerminal as _;

use futures_core::Stream as _;
use pdfboss_aio::AsyncDocument;
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::{Document, Stream};

/// Whether stdout should carry ANSI colors: only on a tty, and never when
/// `NO_COLOR` is set (any value, per the NO_COLOR convention).
pub fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// True for inputs fetched over HTTP; everything else is a local path.
pub fn is_url(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

/// One opened PDF input, local or remote.
pub enum Input {
    Local {
        doc: Document,
    },
    Remote {
        rt: tokio::runtime::Runtime,
        doc: AsyncDocument,
    },
}

impl Input {
    /// Opens `spec`: an `http(s)://` URL via the aio HTTP backend, anything
    /// else as a local file via the sync fast path.
    pub fn open(spec: &str) -> Result<Input, String> {
        if is_url(spec) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            let doc = rt
                .block_on(AsyncDocument::open_url(spec))
                .map_err(|e| e.to_string())?;
            Ok(Input::Remote { rt, doc })
        } else {
            let bytes = std::fs::read(spec).map_err(|e| format!("{spec}: {e}"))?;
            let doc = Document::load(bytes).map_err(|e| e.to_string())?;
            Ok(Input::Local { doc })
        }
    }

    /// Collects the document's elements. Unreadable elements are skipped with
    /// a warning on stderr (salvage semantics: one bad object must not kill
    /// exploration).
    pub fn collect_elements(&self, opts: ElementOpts) -> Vec<Element> {
        match self {
            Input::Local { doc, .. } => doc
                .elements(opts)
                .filter_map(|item| match item {
                    Ok(element) => Some(element),
                    Err(e) => {
                        eprintln!("pdfboss: warning: skipping unreadable element: {e}");
                        None
                    }
                })
                .collect(),
            Input::Remote { rt, doc } => rt.block_on(async {
                let mut out = Vec::new();
                let mut stream = std::pin::pin!(doc.elements(opts));
                while let Some(item) =
                    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
                {
                    match item {
                        Ok(element) => out.push(element),
                        Err(e) => {
                            eprintln!("pdfboss: warning: skipping unreadable element: {e}");
                        }
                    }
                }
                out
            }),
        }
    }

    /// Decodes a stream's data through its filter chain.
    pub fn decode_stream(&self, s: &Stream) -> Result<Vec<u8>, String> {
        match self {
            Input::Local { doc, .. } => doc.stream_data(s).map_err(|e| e.to_string()),
            Input::Remote { rt, doc } => {
                rt.block_on(doc.decode_stream(s)).map_err(|e| e.to_string())
            }
        }
    }

    /// Raw bytes for `span` (end-exclusive), for hex views.
    pub fn read_span(&self, span: Span) -> Result<Vec<u8>, String> {
        match self {
            Input::Local { doc } => {
                let bytes = doc.bytes();
                let start = usize::try_from(span.start).ok();
                let end = usize::try_from(span.end).ok();
                match (start, end) {
                    (Some(start), Some(end)) if start <= end && end <= bytes.len() => {
                        Ok(bytes[start..end].to_vec())
                    }
                    _ => Err(format!(
                        "span {}..{} lies outside the file ({} bytes)",
                        span.start,
                        span.end,
                        bytes.len()
                    )),
                }
            }
            Input::Remote { rt, doc } => {
                rt.block_on(doc.read_span(span)).map_err(|e| e.to_string())
            }
        }
    }

    /// Total length of the underlying file in bytes.
    pub fn file_len(&self) -> u64 {
        match self {
            Input::Local { doc } => doc.bytes().len() as u64,
            Input::Remote { doc, .. } => doc.file_len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        )
    }

    fn physical_opts() -> ElementOpts {
        ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        }
    }

    #[test]
    fn url_detection() {
        assert!(is_url("http://example.com/a.pdf"));
        assert!(is_url("https://example.com/a.pdf"));
        assert!(!is_url("a.pdf"));
        assert!(!is_url("/tmp/a.pdf"));
        assert!(!is_url("httpx://nope"));
    }

    #[test]
    fn local_open_collects_header_first() {
        let input = Input::open(&fixture("hello.pdf")).expect("fixture opens");
        let elements = input.collect_elements(physical_opts());
        assert!(!elements.is_empty(), "no elements from hello.pdf");
        assert!(
            matches!(elements[0], Element::Header { .. }),
            "first physical element must be the header"
        );
    }

    #[test]
    fn local_read_span_and_file_len() {
        let input = Input::open(&fixture("hello.pdf")).expect("fixture opens");
        let len = input.file_len();
        assert!(len > 0);
        let head = input
            .read_span(Span { start: 0, end: 8 })
            .expect("in bounds");
        assert_eq!(&head, b"%PDF-1.7");
        assert!(input
            .read_span(Span {
                start: 0,
                end: len + 1
            })
            .is_err());
        assert!(input.read_span(Span { start: 9, end: 8 }).is_err());
    }

    #[test]
    fn missing_local_file_reports_path() {
        // Not `.expect_err(..)`: that requires the Ok type (`Input`) to
        // implement `Debug`, which it cannot (it embeds a
        // `tokio::runtime::Runtime` and an `AsyncDocument` wrapping a `dyn
        // Backend`, neither `Debug`). Match directly instead.
        let err = match Input::open("definitely-not-here.pdf") {
            Ok(_) => panic!("expected missing file to error"),
            Err(e) => e,
        };
        assert!(
            err.contains("definitely-not-here.pdf"),
            "path missing from: {err}"
        );
    }
}
