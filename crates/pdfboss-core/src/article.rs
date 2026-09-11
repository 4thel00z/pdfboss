//! Article threads (ISO 32000-1 §12.4.3): the catalog's `/Threads` array,
//! each thread's information dictionary and its chain of beads.

use crate::document::{metadata_with, Metadata, Page};
use crate::geom::Rect;
use crate::hash::FastSet;
use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// Beads followed per thread before the walk gives up, so a malformed chain
/// that never returns to its first bead stays bounded even without a repeat.
const MAX_BEADS: usize = 100_000;

/// One bead of an article thread (ISO 32000-1 §12.4.3, Table 161): where
/// one piece of the article lies.
#[derive(Debug, Clone, PartialEq)]
pub struct Bead {
    /// The bead dictionary's own reference; the chain links by reference.
    pub object: ObjRef,
    /// `/P`: the page object the bead appears on.
    pub page: Option<ObjRef>,
    /// `/R`: the bead's rectangle on that page, normalized.
    pub rect: Option<Rect>,
}

/// An article thread (ISO 32000-1 §12.4.3, Table 160): its information
/// dictionary and its beads in chain order.
#[derive(Debug, Clone, PartialEq)]
pub struct ArticleThread {
    /// The thread dictionary's reference, `None` when it was written
    /// directly into the `/Threads` array.
    pub object: Option<ObjRef>,
    /// `/I`: the thread's title, author and the other document information
    /// entries it carries, all `None` without one.
    pub info: Metadata,
    /// The beads from `/F` along `/N`, until the chain returns to a bead
    /// already seen (the last bead points back to the first) or breaks.
    pub beads: Vec<Bead>,
}

/// The article threads the catalog's `/Threads` array declares, in array
/// order: every entry that is a dictionary with an `/F` bead reference.
/// Other entries are skipped, and a catalog without the array, or whose
/// entry is no array, has none.
///
/// Covers ISO 32000-1 §12.4.3.
pub async fn articles_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Vec<ArticleThread> {
    let mut threads = Vec::new();
    let Some(root) = trailer.get("Root") else {
        return threads;
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return threads;
    };
    let Some(entry) = catalog.get("Threads") else {
        return threads;
    };
    let Ok(array) = src.resolve(entry).await else {
        return threads;
    };
    let Some(array) = array.as_array() else {
        return threads;
    };
    for value in array {
        let Some(dict) = resolved_dict(src, value).await else {
            continue;
        };
        let Some(first) = dict.get("F").and_then(Object::as_ref) else {
            continue;
        };
        let info = match dict.get("I") {
            Some(entry) => match resolved_dict(src, entry).await {
                Some(info) => metadata_with(src, &info).await,
                None => Metadata::default(),
            },
            None => Metadata::default(),
        };
        threads.push(ArticleThread {
            object: value.as_ref(),
            info,
            beads: beads_from(src, first).await,
        });
    }
    threads
}

/// The beads on `page` in drawing order: the references its `/B` array
/// holds (Table 30), anything else in it skipped; empty without one.
///
/// Covers ISO 32000-1 §12.4.3.
pub async fn page_beads_with<S: AsyncObjectSource>(src: &S, page: &Page) -> Vec<ObjRef> {
    let Some(entry) = page.dict().get("B") else {
        return Vec::new();
    };
    let Ok(array) = src.resolve(entry).await else {
        return Vec::new();
    };
    array
        .as_array()
        .map(|items| items.iter().filter_map(Object::as_ref).collect())
        .unwrap_or_default()
}

/// Follows `/N` from `first` until a bead repeats, is missing, or the cap
/// is reached.
async fn beads_from<S: AsyncObjectSource>(src: &S, first: ObjRef) -> Vec<Bead> {
    let mut beads = Vec::new();
    let mut visited: FastSet<ObjRef> = FastSet::default();
    let mut next = Some(first);
    while let Some(object) = next {
        if beads.len() >= MAX_BEADS || !visited.insert(object) {
            break;
        }
        let Some(bead) = resolved_dict(src, &Object::Ref(object)).await else {
            break;
        };
        next = bead.get("N").and_then(Object::as_ref);
        beads.push(Bead {
            object,
            page: bead.get("P").and_then(Object::as_ref),
            rect: rectangle(src, bead.get("R")).await,
        });
    }
    beads
}

/// A rectangle (§7.9.5): four numbers, each possibly indirect, normalized.
async fn rectangle<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<Rect> {
    let array = src.resolve(value?).await.ok()?;
    let items = array.as_array()?;
    if items.len() != 4 {
        return None;
    }
    let mut coords = [0.0f32; 4];
    for (slot, item) in coords.iter_mut().zip(items) {
        let number = src.resolve(item).await.ok()?.as_f64()?;
        if !number.is_finite() {
            return None;
        }
        *slot = number as f32;
    }
    Some(Rect::new(coords[0], coords[1], coords[2], coords[3]).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::geom::Rect;
    use pdfboss_testkit::PdfBuilder;

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    /// The clause's EXAMPLE 2: one thread of three beads on page 8, plus a
    /// thread without `/F`; `catalog_extra` replaces the `/Threads` entry.
    fn doc_with(catalog_extra: &str, beads: impl FnOnce(&mut PdfBuilder)) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [8 0 R] /Count 1 >>");
        b.object(
            8,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [23 0 R 24 0 R] >>",
        );
        b.object(
            22,
            "<< /F 23 0 R /I << /Title (Man Bites Dog) /Author (Ann) >> >>",
        );
        b.object(30, "<< /I << /Title (No beads) >> >>");
        beads(&mut b);
        Document::load(b.build(1)).unwrap()
    }

    fn example_beads(b: &mut PdfBuilder) {
        b.object(
            23,
            "<< /T 22 0 R /N 24 0 R /V 25 0 R /P 8 0 R /R [158 247 318 905] >>",
        );
        b.object(
            24,
            "<< /N 25 0 R /V 23 0 R /P 8 0 R /R [322 246 486 904] >>",
        );
        b.object(25, "<< /N 23 0 R /V 24 0 R /P 8 0 R /R [1 2 3 4] >>");
    }

    /// Each thread's beads come in chain order from `/F` until the chain
    /// returns to the first bead; the information dictionary reads like the
    /// document information dictionary; a thread without `/F` is skipped;
    /// the page's `/B` array lists the beads on it.
    // Covers ISO 32000-1 §12.4.3.
    #[test]
    fn walks_each_threads_beads_in_order_and_reads_its_information() {
        let doc = doc_with("/Threads [22 0 R 30 0 R]", example_beads);
        let threads = doc.articles();
        assert_eq!(threads.len(), 1);
        let thread = &threads[0];
        assert_eq!(thread.object, Some(r(22)));
        assert_eq!(thread.info.title.as_deref(), Some("Man Bites Dog"));
        assert_eq!(thread.info.author.as_deref(), Some("Ann"));
        let objects: Vec<ObjRef> = thread.beads.iter().map(|bead| bead.object).collect();
        assert_eq!(objects, vec![r(23), r(24), r(25)]);
        assert_eq!(thread.beads[0].page, Some(r(8)));
        assert_eq!(
            thread.beads[0].rect,
            Some(Rect::new(158.0, 247.0, 318.0, 905.0))
        );
        assert_eq!(thread.beads[2].rect, Some(Rect::new(1.0, 2.0, 3.0, 4.0)));
        let page = doc.page(0).unwrap();
        assert_eq!(doc.page_beads(&page), vec![r(23), r(24)]);
    }

    /// A chain stops at a bead that is missing or already visited without
    /// looping; a catalog without `/Threads`, or with one that is no array,
    /// has no articles.
    // Covers ISO 32000-1 §12.4.3.
    #[test]
    fn a_broken_chain_stops_and_a_document_without_threads_has_none() {
        let doc = doc_with("/Threads [22 0 R 26 0 R]", |b| {
            b.object(
                23,
                "<< /T 22 0 R /N 24 0 R /V 25 0 R /P 8 0 R /R [0 0 1 1] >>",
            );
            b.object(24, "<< /N 99 0 R /V 23 0 R /P 8 0 R >>");
            b.object(26, "<< /F 27 0 R >>");
            b.object(27, "<< /T 26 0 R /N 28 0 R /V 28 0 R /P 8 0 R >>");
            b.object(28, "<< /N 27 0 R /V 27 0 R /P 8 0 R >>");
        });
        let threads = doc.articles();
        assert_eq!(threads.len(), 2);
        let first: Vec<ObjRef> = threads[0].beads.iter().map(|bead| bead.object).collect();
        assert_eq!(first, vec![r(23), r(24)]);
        assert_eq!(threads[0].beads[1].rect, None);
        let second: Vec<ObjRef> = threads[1].beads.iter().map(|bead| bead.object).collect();
        assert_eq!(second, vec![r(27), r(28)]);
        assert_eq!(threads[1].info, Metadata::default());
        assert_eq!(doc_with("", example_beads).articles(), vec![]);
        assert_eq!(
            doc_with("/Threads 22 0 R", example_beads).articles(),
            vec![]
        );
    }
}
