//! Permissions (ISO 32000-1 §12.8.4): the catalog's `/Perms` dictionary,
//! one signature dictionary per permission handler, read as data.

use crate::form::Signature;
use crate::object::Dict;
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// The permission handlers the catalog's `/Perms` dictionary names (ISO
/// 32000-1 §12.8.4, Table 258), each a signature dictionary read as data.
/// Whether the signatures verify, and the permissions they carry in their
/// transform parameters, are not evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PermissionHandlers {
    /// `/DocMDP`: the certifying signature whose DocMDP transform parameters
    /// say what changes the document allows (§12.8.2.2).
    pub doc_mdp: Option<Signature>,
    /// `/UR3`: the usage rights signature that grants a conforming reader
    /// features it does not enable by default (§12.8.2.3).
    pub usage_rights: Option<Signature>,
}

/// The permission handlers of the catalog's `/Perms` dictionary: `None`
/// when the catalog has no `/Perms` dictionary; a handler entry that is no
/// dictionary reads as absent. The table asks for `/DocMDP` by reference;
/// a direct dictionary is accepted as well.
///
/// Covers ISO 32000-1 §12.8.4.
pub async fn permission_handlers_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<PermissionHandlers> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let perms = resolved_dict(src, catalog.get("Perms")?).await?;
    let mut handlers = PermissionHandlers::default();
    if let Some(entry) = perms.get("DocMDP") {
        handlers.doc_mdp = resolved_dict(src, entry)
            .await
            .map(|dict| Signature::from_dict(&dict));
    }
    if let Some(entry) = perms.get("UR3") {
        handlers.usage_rights = resolved_dict(src, entry)
            .await
            .map(|dict| Signature::from_dict(&dict));
    }
    Some(handlers)
}

#[cfg(test)]
mod tests {
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose catalog carries `catalog_extra`; object 15
    /// is a DocMDP signature dictionary reachable by reference.
    fn doc_with(catalog_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(
            15,
            "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /ByteRange [0 10 20 30] /Contents <0102> /Name (Certifier) \
             /Reference [ << /TransformMethod /DocMDP /TransformParams << /P 1 >> >> ] >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// `/DocMDP` by reference and `/UR3` written directly both read as
    /// Table 252 signature dictionaries.
    // Covers ISO 32000-1 §12.8.4.
    #[test]
    fn reads_the_doc_mdp_and_usage_rights_handlers() {
        let doc = doc_with(
            "/Perms << /DocMDP 15 0 R /UR3 << /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.sha1 \
             /ByteRange [0 1 2 3] /Contents <ff> /Reason (Usage rights) >> >>",
        );
        let handlers = doc.permission_handlers().unwrap();
        let doc_mdp = handlers.doc_mdp.unwrap();
        assert_eq!(doc_mdp.filter.as_deref(), Some("Adobe.PPKLite"));
        assert_eq!(doc_mdp.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
        assert_eq!(doc_mdp.byte_range, vec![(0, 10), (20, 30)]);
        assert_eq!(doc_mdp.contents, vec![1, 2]);
        assert_eq!(doc_mdp.name.as_deref(), Some("Certifier"));
        let usage_rights = handlers.usage_rights.unwrap();
        assert_eq!(usage_rights.sub_filter.as_deref(), Some("adbe.pkcs7.sha1"));
        assert_eq!(usage_rights.reason.as_deref(), Some("Usage rights"));
        assert_eq!(usage_rights.name, None);
    }

    /// A catalog without `/Perms`, or whose `/Perms` is no dictionary, has
    /// no handlers; an empty dictionary has the record with neither handler;
    /// a handler entry that is no dictionary reads as absent.
    // Covers ISO 32000-1 §12.8.4.
    #[test]
    fn missing_or_malformed_permissions_read_as_absent() {
        assert_eq!(doc_with("").permission_handlers(), None);
        assert_eq!(doc_with("/Perms 7").permission_handlers(), None);
        let empty = doc_with("/Perms << >>").permission_handlers().unwrap();
        assert_eq!(empty.doc_mdp, None);
        assert_eq!(empty.usage_rights, None);
        let odd = doc_with("/Perms << /DocMDP 42 /UR3 [1 2] >>")
            .permission_handlers()
            .unwrap();
        assert_eq!(odd.doc_mdp, None);
        assert_eq!(odd.usage_rights, None);
    }
}
