//! The developer extensions the catalog declares (ISO 32000-1 §7.12): the
//! `/Extensions` dictionary, one developer extensions dictionary per
//! registered developer prefix, each naming the PDF version it extends and
//! the developer's extension level.

use crate::object::Dict;
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// A developer-defined extension to the standard the document uses (ISO
/// 32000-1 §7.12.2, Table 50): one entry of the catalog's `/Extensions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeveloperExtension {
    /// The developer's registered prefix (Annex E), the entry's key.
    pub prefix: String,
    /// `/BaseVersion`: the PDF version the extension applies to, in the
    /// catalog `/Version` syntax (`1.7`), as written (§7.12.3).
    pub base_version: String,
    /// `/ExtensionLevel`: the developer's number for the extension, higher
    /// for a later extension to the same base version (§7.12.4).
    pub extension_level: i64,
}

impl DeveloperExtension {
    /// The base version as the two integers §7.12.3 reads it as, not as a
    /// real number (`1.7` is `(1, 7)`); `None` when it is not two integers
    /// around a period.
    ///
    /// Covers ISO 32000-1 §7.12.3.
    pub fn base_version_numbers(&self) -> Option<(u32, u32)> {
        let (major, minor) = self.base_version.split_once('.')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }
}

/// The developer extensions the catalog's `/Extensions` dictionary declares,
/// sorted by prefix since a dictionary keeps no order: every entry whose
/// value is a dictionary with a `/BaseVersion` name and an
/// `/ExtensionLevel` integer. The dictionary's own `/Type` entry and an
/// entry missing either value are skipped; indirect objects are followed
/// although the clause asks for direct ones.
///
/// Covers ISO 32000-1 §7.12.2, §7.12.3 and §7.12.4.
pub async fn extensions_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Vec<DeveloperExtension> {
    let mut found = Vec::new();
    let Some(root) = trailer.get("Root") else {
        return found;
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return found;
    };
    let Some(entry) = catalog.get("Extensions") else {
        return found;
    };
    let Some(extensions) = resolved_dict(src, entry).await else {
        return found;
    };
    for (prefix, value) in extensions.iter() {
        if prefix.0 == "Type" {
            continue;
        }
        let Some(dict) = resolved_dict(src, value).await else {
            continue;
        };
        let Some(base_version) = dict.get_name("BaseVersion") else {
            continue;
        };
        let Some(extension_level) = dict.get_int("ExtensionLevel") else {
            continue;
        };
        found.push(DeveloperExtension {
            prefix: prefix.0.clone(),
            base_version: base_version.0.clone(),
            extension_level,
        });
    }
    found.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    found
}

#[cfg(test)]
mod tests {
    use super::DeveloperExtension;
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

    fn extension(prefix: &str, base_version: &str, extension_level: i64) -> DeveloperExtension {
        DeveloperExtension {
            prefix: prefix.to_string(),
            base_version: base_version.to_string(),
            extension_level,
        }
    }

    /// The clause's EXAMPLE 3 with a `/Type` entry, an entry that is no
    /// dictionary and one missing its level, all skipped, and an indirect
    /// developer extensions dictionary, followed; a catalog without the
    /// dictionary has no extensions.
    // Covers ISO 32000-1 §7.12.2, §7.12.3 and §7.12.4.
    #[test]
    fn the_catalog_extensions_are_read_sorted_by_prefix() {
        let found = doc(
            "/Extensions << /Type /Extensions \
             /GLGR << /Type /DeveloperExtensions /BaseVersion /1.7 /ExtensionLevel 1002 >> \
             /ADBE << /BaseVersion /1.7 /ExtensionLevel 3 >> \
             /BAD 5 /HALF << /BaseVersion /1.7 >> /INDR 4 0 R >>",
            &[(4, "<< /BaseVersion /2.0 /ExtensionLevel 1 >>")],
        )
        .extensions();
        assert_eq!(
            found,
            vec![
                extension("ADBE", "1.7", 3),
                extension("GLGR", "1.7", 1002),
                extension("INDR", "2.0", 1),
            ]
        );
        assert_eq!(doc("", &[]).extensions(), Vec::new());
    }

    /// A base version is two integers around a period, not a real number.
    // Covers ISO 32000-1 §7.12.3.
    #[test]
    fn base_versions_are_two_integers() {
        assert_eq!(
            extension("ADBE", "1.7", 3).base_version_numbers(),
            Some((1, 7))
        );
        assert_eq!(
            extension("ADBE", "1.10", 3).base_version_numbers(),
            Some((1, 10))
        );
        assert_eq!(extension("ADBE", "1.7.1", 3).base_version_numbers(), None);
        assert_eq!(extension("ADBE", "one", 3).base_version_numbers(), None);
    }
}
