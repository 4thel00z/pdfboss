//! The document outline (ISO 32000-1 §12.3.3): the tree of items a viewer
//! shows as a table of contents, each with a title, an open or closed
//! state, a destination or action, and its children.

use crate::destination::{destination_value_with, Destination};
use crate::hash::FastSet;
use crate::object::{decode_text_string, Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// Maximum items read from one outline; past it the walk stops, so a
/// malformed chain cannot grow without bound.
pub const MAX_OUTLINE_ITEMS: usize = 65_536;

/// Maximum nesting followed through `/First`; deeper items are left out.
pub const MAX_OUTLINE_DEPTH: usize = 64;

/// One outline item (Table 153) with its children.
///
/// Covers ISO 32000-1 §12.3.3.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    /// `/Title`, decoded as a text string.
    pub title: String,
    /// Where activating the item goes: its `/Dest`, or the `/D` of a
    /// `/GoTo` action in `/A`. `None` for any other action or none.
    pub destination: Option<Destination>,
    /// Whether the item shows its children: a positive `/Count`.
    pub open: bool,
    /// `/C`, the DeviceRGB colour of the title; black when absent.
    pub color: [f32; 3],
    /// Bit 1 of `/F`.
    pub italic: bool,
    /// Bit 2 of `/F`.
    pub bold: bool,
    /// `/SE`, the structure element the item refers to, when indirect.
    pub structure_element: Option<ObjRef>,
    /// The item's children through `/First` and `/Next`, in order.
    pub children: Vec<OutlineItem>,
}

/// The top-level items of the catalog's `/Outlines`, each with its
/// descendants, in the order of the `/First` and `/Next` chains. Empty when
/// the document has no outline. A node visited twice, or one that is not a
/// dictionary, ends its chain; the walk stops after [`MAX_OUTLINE_ITEMS`]
/// items and does not descend past [`MAX_OUTLINE_DEPTH`].
///
/// Covers ISO 32000-1 §12.3.3.
pub async fn outline_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Vec<OutlineItem> {
    let Some(first) = outline_first(src, trailer).await else {
        return Vec::new();
    };
    // Pre-order, one (depth, item) per node: a node's children come before
    // its next sibling because /First is pushed after /Next.
    let mut flat: Vec<(usize, OutlineItem)> = Vec::new();
    let mut visited: FastSet<ObjRef> = FastSet::default();
    let mut stack: Vec<(Object, usize)> = vec![(first, 0)];
    while let Some((node, depth)) = stack.pop() {
        if flat.len() >= MAX_OUTLINE_ITEMS {
            break;
        }
        if let Some(r) = node.as_ref() {
            if !visited.insert(r) {
                continue;
            }
        }
        let Some(dict) = resolved_dict(src, &node).await else {
            continue;
        };
        if let Some(next) = dict.get("Next") {
            stack.push((next.clone(), depth));
        }
        if depth < MAX_OUTLINE_DEPTH {
            if let Some(child) = dict.get("First") {
                stack.push((child.clone(), depth + 1));
            }
        }
        flat.push((depth, read_item(src, trailer, &dict).await));
    }
    nest(flat)
}

/// The `/First` entry of the catalog's `/Outlines` dictionary.
async fn outline_first<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<Object> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let outlines = resolved_dict(src, catalog.get("Outlines")?).await?;
    outlines.get("First").cloned()
}

/// One item's own entries (Table 153), children left empty.
async fn read_item<S: AsyncObjectSource>(src: &S, trailer: &Dict, dict: &Dict) -> OutlineItem {
    let title = match dict.get("Title") {
        Some(t) => src
            .resolve(t)
            .await
            .ok()
            .and_then(|t| t.as_str_bytes().map(decode_text_string))
            .unwrap_or_default(),
        None => String::new(),
    };
    let destination = match dict.get("Dest") {
        Some(dest) => destination_value_with(src, trailer, dest).await,
        None => action_destination(src, trailer, dict).await,
    };
    let count = match dict.get("Count") {
        Some(c) => src.resolve(c).await.ok().and_then(|c| c.as_int()),
        None => None,
    };
    let color = match dict.get("C") {
        Some(c) => src.resolve(c).await.ok().and_then(|c| rgb(&c)),
        None => None,
    };
    let flags = match dict.get("F") {
        Some(f) => src.resolve(f).await.ok().and_then(|f| f.as_int()),
        None => None,
    }
    .unwrap_or(0);
    OutlineItem {
        title,
        destination,
        open: count.is_some_and(|c| c > 0),
        color: color.unwrap_or([0.0; 3]),
        italic: flags & 1 != 0,
        bold: flags & 2 != 0,
        structure_element: dict.get("SE").and_then(Object::as_ref),
        children: Vec::new(),
    }
}

/// The `/D` of a `/GoTo` action in the item's `/A`, as a destination.
///
/// Covers ISO 32000-1 §12.6.4.2.
async fn action_destination<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    dict: &Dict,
) -> Option<Destination> {
    let action = resolved_dict(src, dict.get("A")?).await?;
    let kind = src.resolve(action.get("S")?).await.ok()?;
    if kind.as_name()?.0 != "GoTo" {
        return None;
    }
    destination_value_with(src, trailer, action.get("D")?).await
}

/// An array of three numbers in the range 0 to 1 as a colour.
fn rgb(o: &Object) -> Option<[f32; 3]> {
    let items = o.as_array()?;
    if items.len() != 3 {
        return None;
    }
    let mut out = [0.0; 3];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = item.as_f64()?.clamp(0.0, 1.0) as f32;
    }
    Some(out)
}

/// Rebuilds the tree from a pre-order list of `(depth, item)`.
fn nest(flat: Vec<(usize, OutlineItem)>) -> Vec<OutlineItem> {
    let mut roots = Vec::new();
    let mut open: Vec<(usize, OutlineItem)> = Vec::new();
    for (depth, item) in flat {
        while open.last().is_some_and(|(d, _)| *d >= depth) {
            let (_, done) = open.pop().expect("checked by the loop condition");
            attach(done, &mut open, &mut roots);
        }
        open.push((depth, item));
    }
    while let Some((_, done)) = open.pop() {
        attach(done, &mut open, &mut roots);
    }
    roots
}

/// Puts a finished item under the innermost open ancestor, or at the top.
fn attach(done: OutlineItem, open: &mut [(usize, OutlineItem)], roots: &mut Vec<OutlineItem>) {
    match open.last_mut() {
        Some((_, parent)) => parent.children.push(done),
        None => roots.push(done),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::{DestinationPage, Fit};
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    /// One page with `catalog_extra` in the catalog and `objects` from 10 up.
    fn doc(catalog_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).expect("load")
    }

    fn titles(items: &[OutlineItem]) -> Vec<String> {
        items.iter().map(|i| i.title.clone()).collect()
    }

    fn page(fit: Fit) -> Option<Destination> {
        Some(Destination {
            page: DestinationPage::Object(ObjRef { num: 3, gen: 0 }),
            fit,
        })
    }

    // Covers ISO 32000-1 §12.3.3 and §12.3.2.3.
    #[test]
    fn outline_items_nest_by_first_and_next() {
        // Chapter 1 (open, two children) and Chapter 2 (closed, one child),
        // then an appendix with a colour, both style flags and a named
        // destination through the catalog /Dests dictionary; the second
        // child reaches its page through a /GoTo action.
        let doc = doc(
            "/Outlines 10 0 R /Dests << /App [3 0 R /FitB] >>",
            &[
                (
                    10,
                    "<< /Type /Outlines /First 11 0 R /Last 16 0 R /Count 4 >>",
                ),
                (
                    11,
                    "<< /Title (Chapter 1) /Parent 10 0 R /Next 14 0 R /First 12 0 R /Last 13 0 R \
                     /Count 2 /Dest [3 0 R /XYZ 0 792 0] >>",
                ),
                (
                    12,
                    "<< /Title <FEFF00A70031> /Parent 11 0 R /Next 13 0 R /Dest [3 0 R /Fit] >>",
                ),
                (
                    13,
                    "<< /Title (1.2) /Parent 11 0 R /Prev 12 0 R \
                     /A << /S /GoTo /D [3 0 R /FitH 400] >> >>",
                ),
                (
                    14,
                    "<< /Title (Chapter 2) /Parent 10 0 R /Prev 11 0 R /Next 16 0 R \
                     /First 15 0 R /Last 15 0 R /Count -1 /SE 20 0 R >>",
                ),
                (
                    15,
                    "<< /Title (2.1) /Parent 14 0 R /A << /S /URI /URI (http://x) >> >>",
                ),
                (
                    16,
                    "<< /Title (Appendix) /Parent 10 0 R /Prev 14 0 R /Dest /App \
                     /C [1 0 0.5] /F 3 >>",
                ),
                (20, "<< /Type /StructElem /S /Sect >>"),
            ],
        );
        let items = doc.outline();
        assert_eq!(titles(&items), ["Chapter 1", "Chapter 2", "Appendix"]);

        let chapter1 = &items[0];
        assert!(chapter1.open);
        assert_eq!(
            chapter1.destination,
            page(Fit::Xyz {
                left: Some(0.0),
                top: Some(792.0),
                zoom: None
            })
        );
        assert_eq!(titles(&chapter1.children), ["§1", "1.2"]);
        assert_eq!(chapter1.children[0].destination, page(Fit::Fit));
        assert_eq!(
            chapter1.children[1].destination,
            page(Fit::FitH { top: Some(400.0) }),
            "a /GoTo action's /D counts as the destination"
        );
        assert!(chapter1.children.iter().all(|c| c.children.is_empty()));

        let chapter2 = &items[1];
        assert!(!chapter2.open, "a negative /Count is closed");
        assert_eq!(chapter2.destination, None);
        assert_eq!(chapter2.structure_element, Some(ObjRef { num: 20, gen: 0 }));
        assert_eq!(titles(&chapter2.children), ["2.1"]);
        assert_eq!(
            chapter2.children[0].destination, None,
            "a URI action is not a destination"
        );

        let appendix = &items[2];
        assert_eq!(appendix.destination, page(Fit::FitB));
        assert_eq!(appendix.color, [1.0, 0.0, 0.5]);
        assert!(appendix.italic && appendix.bold);
        assert!(!chapter1.italic && !chapter1.bold);
        assert_eq!(chapter1.color, [0.0, 0.0, 0.0]);
    }

    // Covers ISO 32000-1 §12.3.3.
    #[test]
    fn cycles_and_missing_outlines_end_quietly() {
        assert!(doc("", &[]).outline().is_empty(), "no /Outlines");
        assert!(
            doc(
                "/Outlines 10 0 R",
                &[(10, "<< /Type /Outlines /Count 0 >>")]
            )
            .outline()
            .is_empty(),
            "an outline without /First"
        );
        // B's /Next points back at A, C's /First is a string, and D is
        // reached from C: every item once, in chain order.
        let doc = doc(
            "/Outlines 10 0 R",
            &[
                (10, "<< /Type /Outlines /First 11 0 R /Last 13 0 R >>"),
                (11, "<< /Title (A) /Parent 10 0 R /Next 12 0 R >>"),
                (
                    12,
                    "<< /Title (B) /Parent 10 0 R /Next 11 0 R /First 13 0 R >>",
                ),
                (
                    13,
                    "<< /Title (C) /Parent 12 0 R /First (oops) /Next 14 0 R >>",
                ),
                (14, "<< /Title (D) /Parent 12 0 R >>"),
            ],
        );
        let items = doc.outline();
        assert_eq!(titles(&items), ["A", "B"]);
        assert_eq!(titles(&items[1].children), ["C", "D"]);
        assert!(items[1].children[0].children.is_empty());
    }
}
