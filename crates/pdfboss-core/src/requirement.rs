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
    /// `/RH`: the handlers a reader that does not meet the requirement
    /// runs, one dictionary or an array of them, in order (§12.10.2).
    pub handlers: Vec<RequirementHandler>,
}

/// One requirement handler (ISO 32000-1 §12.10.2, Table 267), read as data:
/// pdfboss runs no JavaScript, so the handler is reported, not invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementHandler {
    /// `/S`: `JS` for a document-level JavaScript, `NoOp` for doing nothing;
    /// any other name is kept as written.
    pub kind: String,
    /// `/Script`: the name of the document-level JavaScript a `JS` handler
    /// runs, as the catalog's `/Names` `/JavaScript` tree lists it.
    pub script: Option<String>,
}

/// The requirements the catalog's `/Requirements` array lists, in order:
/// empty when the catalog has no array. An entry that is no dictionary, or
/// whose `/S` is no name, is skipped.
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
            handlers: handlers_with(src, &dict).await,
        });
    }
    requirements
}

/// The `/RH` handlers of one requirement dictionary, in order: empty when
/// the entry is missing or neither a dictionary nor an array. A handler
/// that is no dictionary, or whose `/S` is no name, is skipped.
///
/// Covers ISO 32000-1 §12.10.2.
async fn handlers_with<S: AsyncObjectSource>(
    src: &S,
    requirement: &Dict,
) -> Vec<RequirementHandler> {
    let Some(entry) = requirement.get("RH") else {
        return Vec::new();
    };
    let entries = match src.resolve(entry).await {
        Ok(Object::Array(entries)) => entries,
        Ok(handler @ Object::Dict(_)) => vec![handler],
        _ => return Vec::new(),
    };
    let mut handlers = Vec::new();
    for entry in &entries {
        let Some(dict) = resolved_dict(src, entry).await else {
            continue;
        };
        let entries = Entries { src, dict: &dict };
        let Some(kind) = entries.value("S").await else {
            continue;
        };
        let Some(kind) = kind.as_name() else {
            continue;
        };
        handlers.push(RequirementHandler {
            kind: kind.0.clone(),
            script: entries.text("Script").await,
        });
    }
    handlers
}

#[cfg(test)]
mod tests {
    use super::RequirementHandler;
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose catalog carries `catalog_extra`; object 20
    /// is an EnableJavaScripts requirement reachable by reference, object 21
    /// a JavaScript requirement handler.
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
        b.object(21, "<< /Type /ReqHandler /S /JS /Script (fallback) >>");
        Document::load(b.build(1)).unwrap()
    }

    /// `/RH` as one handler dictionary or as an array of them reads in
    /// order: the `/S` kind and, for `/JS`, the `/Script` name of the
    /// document-level JavaScript that handles the requirement.
    // Covers ISO 32000-1 §12.10.2.
    #[test]
    fn reads_the_requirement_handlers() {
        let doc = doc_with(
            "/Requirements [20 0 R << /S /Custom /RH [ << /Type /ReqHandler /S /NoOp >> 21 0 R ] >>]",
        );
        let requirements = doc.requirements();
        assert_eq!(
            requirements[0].handlers,
            [RequirementHandler {
                kind: "JS".into(),
                script: Some("init".into()),
            }]
        );
        assert_eq!(
            requirements[1].handlers,
            [
                RequirementHandler {
                    kind: "NoOp".into(),
                    script: None,
                },
                RequirementHandler {
                    kind: "JS".into(),
                    script: Some("fallback".into()),
                },
            ]
        );
    }

    /// A requirement without `/RH`, or whose `/RH` is neither a dictionary
    /// nor an array, has no handlers; a handler that is no dictionary or has
    /// no `/S` name is skipped.
    // Covers ISO 32000-1 §12.10.2.
    #[test]
    fn missing_or_malformed_handlers_read_as_none() {
        let doc = doc_with(
            "/Requirements [ << /S /A >> << /S /B /RH 7 >> \
             << /S /C /RH [ 7 (text) << /Type /ReqHandler >> << /S /NoOp >> ] >> ]",
        );
        let counts: Vec<usize> = doc
            .requirements()
            .iter()
            .map(|requirement| requirement.handlers.len())
            .collect();
        assert_eq!(counts, [0, 0, 1]);
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
