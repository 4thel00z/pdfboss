//! Document requirements (ISO 32000-1 §12.10): the catalog's `/Requirements`
//! array, one dictionary per feature a reader needs to show the document as
//! intended.

use crate::object::{Dict, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// One entry of the catalog's `/Requirements` array (ISO 32000-1 §12.10.1,
/// Table 266): a feature the document needs a reader to support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// `/S`: the requirement type. `EnableJavaScripts` is the one type ISO
    /// 32000-1 defines; any other name is kept as written.
    pub kind: String,
}

/// The requirements the catalog's `/Requirements` array lists, in order:
/// empty when the catalog has no array. An entry that is no dictionary, or
/// whose `/S` is no name, is skipped. The `/RH` handlers are not read
/// (§12.10.2).
///
/// Covers ISO 32000-1 §12.10.1.
pub async fn requirements_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Vec<Requirement> {
    let Some(root) = trailer.get("Root") else {
        return Vec::new();
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return Vec::new();
    };
    let Some(entry) = catalog.get("Requirements") else {
        return Vec::new();
    };
    let Ok(Object::Array(entries)) = src.resolve(entry).await else {
        return Vec::new();
    };
    let mut requirements = Vec::new();
    for entry in &entries {
        let Some(dict) = resolved_dict(src, entry).await else {
            continue;
        };
        let Some(kind) = Entries { src, dict: &dict }.value("S").await else {
            continue;
        };
        let Some(kind) = kind.as_name() else {
            continue;
        };
        requirements.push(Requirement {
            kind: kind.0.clone(),
        });
    }
    requirements
}

#[cfg(test)]
mod tests {
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose catalog carries `catalog_extra`; object 20
    /// is an EnableJavaScripts requirement reachable by reference.
    fn doc_with(catalog_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(
            20,
            "<< /Type /Requirement /S /EnableJavaScripts \
             /RH << /Type /ReqHandler /S /JS /Script (init) >> >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// The array's dictionaries read in order, by reference or written
    /// directly, each with its `/S` type name.
    // Covers ISO 32000-1 §12.10.1.
    #[test]
    fn reads_the_requirements_in_order() {
        let doc = doc_with("/Requirements [20 0 R << /Type /Requirement /S /Custom >>]");
        let requirements = doc.requirements();
        let kinds: Vec<&str> = requirements
            .iter()
            .map(|requirement| requirement.kind.as_str())
            .collect();
        assert_eq!(kinds, ["EnableJavaScripts", "Custom"]);
    }

    /// A catalog without `/Requirements`, or whose entry is no array, has
    /// none; an entry that is no dictionary or has no `/S` name is skipped.
    // Covers ISO 32000-1 §12.10.1.
    #[test]
    fn missing_or_malformed_requirements_read_as_none() {
        assert!(doc_with("").requirements().is_empty());
        assert!(doc_with("/Requirements 20 0 R").requirements().is_empty());
        let odd =
            doc_with("/Requirements [7 (text) << /Type /Requirement >> 20 0 R]").requirements();
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].kind, "EnableJavaScripts");
    }
}
