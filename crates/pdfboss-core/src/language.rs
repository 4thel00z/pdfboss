//! The document's natural language (ISO 32000-1 §14.9.2): the catalog's
//! `/Lang`, the default for every piece of text a structure element or a
//! marked-content sequence does not tag with a language of its own.

use crate::object::{decode_text_string, Dict};
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// The language the catalog declares for the document's text (`/Lang`),
/// decoded as a text string, or `None` when the catalog declares none or
/// the entry is not a string. A structure element's or a marked-content
/// sequence's own `/Lang` overrides it for their text (`Placement::lang`,
/// and `lang` on the text crate's spans).
///
/// Covers ISO 32000-1 §14.9.2.
pub async fn language_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<String> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let value = src.resolve(catalog.get("Lang")?).await.ok()?;
    Some(decode_text_string(value.as_str_bytes()?))
}

#[cfg(test)]
mod tests {
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

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
        Document::load(b.build(1)).unwrap()
    }

    // Covers ISO 32000-1 §14.9.2.
    #[test]
    fn the_catalog_lang_is_the_document_language() {
        assert_eq!(
            doc("/Lang (en-US)", &[]).language().as_deref(),
            Some("en-US")
        );
        assert_eq!(
            doc("/Lang <FEFF00640065>", &[]).language().as_deref(),
            Some("de")
        );
        assert_eq!(
            doc("/Lang 4 0 R", &[(4, "(fr)")]).language().as_deref(),
            Some("fr")
        );
        assert_eq!(doc("", &[]).language(), None);
        assert_eq!(doc("/Lang 7", &[]).language(), None);
    }
}
