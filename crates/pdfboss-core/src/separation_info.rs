//! Separation dictionaries (ISO 32000-1 §14.11.4): a page's `/SeparationInfo`,
//! which names the colorant a pre-separated page prints and the other pages
//! of the same separation set.

use crate::document::Page;
use crate::object::{ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// A separation dictionary (ISO 32000-1 §14.11.4, Table 364): what a page
/// that is one colour separation of a composite page prints, read as data.
#[derive(Debug, Clone, PartialEq)]
pub struct SeparationInfo {
    /// `/Pages`: the pages of the same separation set, this page among
    /// them, as the references written.
    pub pages: Vec<ObjRef>,
    /// `/DeviceColorant`: the colorant this page prints, a name or a text
    /// string.
    pub device_colorant: String,
    /// `/ColorSpace`: the Separation or DeviceN colour space array whose tint
    /// transform approximates the colorant on a display; `None` without one.
    pub color_space: Option<Vec<Object>>,
}

/// The separation dictionary of `page`: `None` when the page has no
/// `/SeparationInfo`, when the entry is no dictionary, or when it names no
/// colorant. A `/Pages` entry that is no array reads as empty, and its items
/// that are no references are skipped.
///
/// Covers ISO 32000-1 §14.11.4.
pub async fn separation_info_with<S: AsyncObjectSource>(
    src: &S,
    page: &Page,
) -> Option<SeparationInfo> {
    let dict = resolved_dict(src, page.dict().get("SeparationInfo")?).await?;
    let entries = Entries { src, dict: &dict };
    let device_colorant = match entries.value("DeviceColorant").await? {
        Object::Name(name) => name.0,
        _ => entries.text("DeviceColorant").await?,
    };
    let pages = match entries.value("Pages").await {
        Some(Object::Array(items)) => items.iter().filter_map(Object::as_ref).collect(),
        _ => Vec::new(),
    };
    let color_space = match entries.value("ColorSpace").await {
        Some(Object::Array(items)) => Some(items),
        _ => None,
    };
    Some(SeparationInfo {
        pages,
        device_colorant,
        color_space,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::object::ObjRef;
    use pdfboss_testkit::PdfBuilder;

    /// A two-page document whose first page carries `page_extra`.
    fn doc_with(page_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page_extra} >>"),
        );
        b.object(4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>");
        Document::load(b.build(1)).unwrap()
    }

    fn separation_info(page_extra: &str) -> Option<SeparationInfo> {
        let doc = doc_with(page_extra);
        doc.separation_info(&doc.page(0).unwrap())
    }

    /// The dictionary reads as the set's page references, the colorant as a
    /// name or a text string, and the approximating colour space array.
    // Covers ISO 32000-1 §14.11.4.
    #[test]
    fn reads_the_separation_dictionary() {
        let cyan = separation_info(
            "/SeparationInfo << /Pages [3 0 R 4 0 R] /DeviceColorant /Cyan \
             /ColorSpace [/Separation /Cyan /DeviceCMYK 5 0 R] >>",
        )
        .unwrap();
        assert_eq!(
            cyan.pages,
            [ObjRef { num: 3, gen: 0 }, ObjRef { num: 4, gen: 0 }]
        );
        assert_eq!(cyan.device_colorant, "Cyan");
        let color_space = cyan.color_space.unwrap();
        assert_eq!(color_space.len(), 4);
        assert_eq!(
            color_space[0].as_name().map(|n| n.0.as_str()),
            Some("Separation")
        );
        let spot =
            separation_info("/SeparationInfo << /Pages [3 0 R] /DeviceColorant (PANTONE 300 C) >>")
                .unwrap();
        assert_eq!(spot.device_colorant, "PANTONE 300 C");
        assert_eq!(spot.color_space, None);
    }

    /// A page without `/SeparationInfo`, or whose entry is no dictionary or
    /// names no colorant, has none; a `/Pages` entry that is no array reads
    /// as empty and items that are no references are skipped.
    // Covers ISO 32000-1 §14.11.4.
    #[test]
    fn missing_or_malformed_separation_info_reads_as_absent() {
        assert_eq!(separation_info(""), None);
        assert_eq!(separation_info("/SeparationInfo 5"), None);
        assert_eq!(
            separation_info("/SeparationInfo << /Pages [3 0 R] >>"),
            None
        );
        let no_pages =
            separation_info("/SeparationInfo << /DeviceColorant /Cyan /Pages 7 >>").unwrap();
        assert!(no_pages.pages.is_empty());
        let odd =
            separation_info("/SeparationInfo << /DeviceColorant /Cyan /Pages [3 0 R (x) 4 0 R] >>")
                .unwrap();
        assert_eq!(
            odd.pages,
            [ObjRef { num: 3, gen: 0 }, ObjRef { num: 4, gen: 0 }]
        );
    }
}
