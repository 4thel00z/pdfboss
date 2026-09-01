//! Async incremental-append parity with `pdfboss-write`'s synchronous
//! `Update`. An [`AsyncDocument`] already retains its newest cross-
//! reference section from opening, so [`overlay_base`] derives an
//! `OverlayBase` from it directly, without any extra fetch beyond what
//! opening already did. [`append_overlay`] builds the overlay's appended
//! section first, then streams the base bytes and the section through a
//! sink, so a section that fails to build leaves the sink untouched and
//! the whole base is never held in memory at once.

use pdfboss_core::XrefKind;
use pdfboss_write::{AsyncByteSink, Error, Overlay, OverlayBase, Result, XrefStyle};

use crate::document::AsyncDocument;

/// Chunk size for streaming the base document through a sink.
const CHUNK: u64 = 64 * 1024;

/// Reads `doc`'s merged trailer and the newest cross-reference section
/// retained from opening, the async twin of
/// [`pdfboss_write::OverlayBase::from_document`]: refuses an encrypted
/// base or one missing `/Root`. `prev` and `kind` come from the newest
/// section itself (a hybrid base's newest section, per `startxref`, is its
/// classic table, even though the merged trailer carries `/Type /XRef`
/// inherited from the table's `/XRefStm`); `size`, `info` and `id` come
/// from the merged trailer. A successfully opened `AsyncDocument` has
/// always parsed at least one xref section during its open-time chain
/// walk (an empty result there is `InvalidXref` before opening finishes),
/// so the section lookup below cannot fail.
pub async fn overlay_base(doc: &AsyncDocument) -> Result<OverlayBase> {
    let (trailer, _) = doc.merged_trailer();
    if trailer.get("Encrypt").is_some_and(|o| !o.is_null()) {
        return Err(Error::EncryptedBase);
    }
    let root = trailer.get_ref("Root").ok_or(Error::MissingRoot)?;
    let newest = doc
        .sections()
        .first()
        .expect("a successfully opened document has walked at least one section");
    let kind = match newest.kind {
        XrefKind::Table => XrefStyle::Table,
        XrefKind::Stream => XrefStyle::Stream,
    };
    let highest = doc
        .xref_entries()
        .iter()
        .map(|(num, _)| *num)
        .max()
        .unwrap_or(0);
    let declared = trailer.get_int("Size").unwrap_or(0).max(0) as u32;
    Ok(OverlayBase {
        prev: newest.span.start,
        kind,
        size: declared.max(highest + 1),
        root,
        info: trailer.get_ref("Info"),
        id: trailer.get("ID").cloned(),
    })
}

/// Builds `overlay`'s appended section, then streams `doc`'s bytes through
/// `sink` in 64 KiB chunks, then a pad `\n` when the base does not already
/// end on a line terminator, then the section itself: the async twin of
/// [`pdfboss_write::Update::append_into`]. The section is built, and any
/// error from an empty overlay or a change that cannot serialize is
/// returned, before a single byte reaches `sink`. Returns the sink.
pub async fn append_overlay<S: AsyncByteSink>(
    doc: &AsyncDocument,
    overlay: &Overlay,
    mut sink: S,
) -> Result<S> {
    if overlay.is_empty() {
        return Err(Error::EmptyUpdate);
    }
    let fetcher = doc.fetcher();
    let pad = if fetcher.len == 0 {
        true
    } else {
        let tail = fetcher
            .read_range(fetcher.len - 1, fetcher.len)
            .await
            .map_err(aio_error)?;
        !matches!(tail.first(), Some(b'\n') | Some(b'\r'))
    };
    let section = overlay.section(fetcher.len + u64::from(pad))?;
    let mut offset = 0u64;
    while offset < fetcher.len {
        let end = fetcher.len.min(offset + CHUNK);
        let chunk = fetcher.read_range(offset, end).await.map_err(aio_error)?;
        sink.write_all(&chunk).await?;
        offset = end;
    }
    if pad {
        sink.write_all(b"\n").await?;
    }
    sink.write_all(&section).await?;
    Ok(sink)
}

/// Wraps an aio-side fetch failure as a write-side error: `append_overlay`
/// speaks `pdfboss_write::Result` throughout, so a backend failure needs a
/// home in that error type too.
fn aio_error(error: crate::error::Error) -> Error {
    Error::Other(error.to_string())
}
