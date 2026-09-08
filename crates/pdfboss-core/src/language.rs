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

/// A language identifier as `/Lang` entries spell it (ISO 32000-1
/// §14.9.2.2, the RFC 3066 form): a primary language subtag, then subtags
/// separated by hyphens, the second one a country code when it has two
/// letters. Case carries no meaning in a tag; the language comes out in
/// lower case and the country in upper case, as they are usually written,
/// and the other subtags as written.
///
/// Covers ISO 32000-1 §14.9.2.2.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LanguageTag {
    /// The primary subtag: an ISO 639 language code, or `i` for a registered
    /// and `x` for a private-use tag.
    pub language: String,
    /// The ISO 3166 country code, when the second subtag has two letters.
    pub country: Option<String>,
    /// The subtags after the language and the country, as written.
    pub subtags: Vec<String>,
}

impl LanguageTag {
    /// Longest subtag RFC 3066 allows.
    const MAX_SUBTAG: usize = 8;

    /// Parses `tag`, surrounding whitespace ignored; `None` when it is not a
    /// well-formed tag: an empty subtag, a subtag longer than eight
    /// characters, a character outside ASCII letters (and digits after the
    /// first subtag), or a separator other than the hyphen.
    pub fn parse(tag: &str) -> Option<LanguageTag> {
        let mut subtags = tag.trim().split('-');
        let language = subtags.next()?;
        if !Self::is_subtag(language, false) {
            return None;
        }
        let mut rest: Vec<&str> = subtags.collect();
        if !rest.iter().all(|subtag| Self::is_subtag(subtag, true)) {
            return None;
        }
        let country = match rest.first() {
            Some(second)
                if second.len() == 2 && second.bytes().all(|b| b.is_ascii_alphabetic()) =>
            {
                Some(rest.remove(0).to_ascii_uppercase())
            }
            _ => None,
        };
        Some(LanguageTag {
            language: language.to_ascii_lowercase(),
            country,
            subtags: rest.into_iter().map(str::to_string).collect(),
        })
    }

    /// One to eight ASCII letters, digits allowed too after the first subtag.
    fn is_subtag(subtag: &str, digits: bool) -> bool {
        !subtag.is_empty()
            && subtag.len() <= Self::MAX_SUBTAG
            && subtag
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || (digits && b.is_ascii_digit()))
    }
}

#[cfg(test)]
mod tests {
    use super::LanguageTag;
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

    // Covers ISO 32000-1 §14.9.2.2.
    #[test]
    fn language_tags_split_into_language_and_country() {
        let tag = LanguageTag::parse("en-US").unwrap();
        assert_eq!(tag.language, "en");
        assert_eq!(tag.country.as_deref(), Some("US"));
        assert!(tag.subtags.is_empty());
        // Case is not significant; the usual spelling comes out.
        let tag = LanguageTag::parse(" EN-us ").unwrap();
        assert_eq!(
            (tag.language.as_str(), tag.country.as_deref()),
            ("en", Some("US"))
        );
        let tag = LanguageTag::parse("de").unwrap();
        assert_eq!((tag.language.as_str(), tag.country), ("de", None));
        // A second subtag of another length is not a country; later subtags
        // stay as written.
        let tag = LanguageTag::parse("zh-Hant-TW").unwrap();
        assert_eq!(tag.language, "zh");
        assert_eq!(tag.country, None);
        assert_eq!(tag.subtags, ["Hant", "TW"]);
        // The registered and private-use prefixes are primary subtags too.
        let tag = LanguageTag::parse("x-pdfboss").unwrap();
        assert_eq!(
            (tag.language.as_str(), tag.subtags.as_slice()),
            ("x", ["pdfboss".to_string()].as_slice())
        );
        assert_eq!(LanguageTag::parse("i-klingon").unwrap().language, "i");
        for bad in [
            "",
            "-en",
            "en-",
            "en--US",
            "en_US",
            "toolongsubtag",
            "en-1234567890",
            "en US",
            "éé",
        ] {
            assert!(LanguageTag::parse(bad).is_none(), "{bad:?}");
        }
        assert_eq!(
            doc("/Lang (en-GB)", &[])
                .language_tag()
                .unwrap()
                .country
                .as_deref(),
            Some("GB")
        );
        assert!(doc("/Lang (not a tag)", &[]).language_tag().is_none());
    }
}
