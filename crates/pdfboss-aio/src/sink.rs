//! Streams documents built with `pdfboss-write` into tokio writers: files,
//! sockets, anything implementing [`tokio::io::AsyncWrite`]. Only compiled
//! with the `write` feature.

use pdfboss_core::source::BoxFuture;
use pdfboss_write::sink::AsyncByteSink;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Presents any [`tokio::io::AsyncWrite`] as the byte sink the write
/// crate's emission streams into — hand `TokioSink(file)` to
/// [`pdfboss_write::Pdf::write_into_with`] or
/// [`pdfboss_write::Writer::finish_into_with`] and take the writer back
/// out of the returned sink's field. No flush is ever performed: flush (or
/// shut down) the recovered writer yourself before dropping it.
#[derive(Debug)]
pub struct TokioSink<W>(pub W);

impl<W: AsyncWrite + Unpin + Send> AsyncByteSink for TokioSink<W> {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, pdfboss_write::Result<()>> {
        Box::pin(async move {
            self.0.write_all(buf).await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_write::{Page, PageSize, Pdf, Standard14};
    use tokio::io::AsyncWriteExt;

    use super::TokioSink;

    fn hello_doc() -> Pdf {
        let mut page = Page::new(PageSize::A4);
        page.canvas
            .text(
                "Streamed through tokio",
                72.0,
                720.0,
                Standard14::Helvetica,
                14.0,
            )
            .expect("ASCII encodes");
        Pdf {
            pages: vec![page],
            ..Pdf::default()
        }
    }

    #[tokio::test]
    async fn tokio_sink_over_a_vec_matches_to_bytes() {
        let expected = hello_doc().to_bytes().expect("to_bytes succeeds");
        let sink = hello_doc()
            .write_into_with(TokioSink(Vec::new()))
            .await
            .expect("write_into_with succeeds");
        assert_eq!(sink.0, expected);
    }

    #[tokio::test]
    async fn tokio_sink_over_a_file_round_trips() {
        let path =
            std::env::temp_dir().join(format!("pdfboss-aio-sink-test-{}.pdf", std::process::id()));
        let file = tokio::fs::File::create(&path).await.expect("file creates");
        let mut sink = hello_doc()
            .write_into_with(TokioSink(file))
            .await
            .expect("write_into_with succeeds");
        sink.0.flush().await.expect("file flushes");
        drop(sink);
        let bytes = std::fs::read(&path).expect("file reads back");
        std::fs::remove_file(&path).ok();
        assert_eq!(bytes, hello_doc().to_bytes().expect("to_bytes succeeds"));
        let doc = pdfboss_core::Document::load(bytes).expect("written file loads");
        assert_eq!(doc.page_count(), 1);
    }
}
