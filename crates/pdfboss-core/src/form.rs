//! The document's interactive form (ISO 32000-1 §12.7): the catalog's
//! `/AcroForm` dictionary, which names the root fields and carries the
//! defaults their widgets are drawn with.

use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// The document-level signature flags (`/SigFlags`, ISO 32000-1 §12.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignatureFlags {
    /// Bit 1, `SignaturesExist`: the document has at least one signature
    /// field.
    pub signatures_exist: bool,
    /// Bit 2, `AppendOnly`: the document's signatures would be invalidated
    /// by anything but an incremental update.
    pub append_only: bool,
}

impl SignatureFlags {
    /// The flags of a `/SigFlags` word; the reserved bits are ignored.
    ///
    /// Covers ISO 32000-1 §12.7.2.
    pub fn from_bits(bits: i64) -> SignatureFlags {
        SignatureFlags {
            signatures_exist: bits & 1 != 0,
            append_only: bits & 2 != 0,
        }
    }
}

/// The quadding of variable text (`/Q`, §12.7.3.3): how the text of a field
/// is justified within its widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quadding {
    /// 0: left-justified.
    Left,
    /// 1: centred.
    Centered,
    /// 2: right-justified.
    Right,
}

impl Quadding {
    /// The quadding a `/Q` value names, `None` for anything but 0, 1 and 2.
    pub fn from_int(value: i64) -> Option<Quadding> {
        match value {
            0 => Some(Quadding::Left),
            1 => Some(Quadding::Centered),
            2 => Some(Quadding::Right),
            _ => None,
        }
    }
}

/// The interactive form dictionary (ISO 32000-1 §12.7.2, Table 218).
#[derive(Debug, Clone, PartialEq)]
pub struct InteractiveForm {
    /// `/Fields`: the root fields, those with no parent, as the references
    /// the array holds.
    pub fields: Vec<ObjRef>,
    /// `/NeedAppearances`: whether a reader should build the widgets'
    /// appearance streams itself; false by default.
    pub need_appearances: bool,
    /// `/SigFlags`, no flag set by default.
    pub signature_flags: SignatureFlags,
    /// `/CO`: the fields with calculation actions, in the order their values
    /// are recalculated.
    pub calculation_order: Vec<ObjRef>,
    /// `/DR`: the default resources for the fields' appearance streams.
    pub default_resources: Option<Dict>,
    /// `/DA`: the document-wide default appearance string for variable
    /// text, a content fragment such as `/Helv 0 Tf 0 g`, as written.
    pub default_appearance: Option<String>,
    /// `/Q`: the document-wide default quadding for variable text; `None`
    /// when absent or not 0, 1 or 2.
    pub quadding: Option<Quadding>,
    /// `/XFA`: whether the form carries an XFA resource, as a stream or an
    /// array of packets (§12.7.8).
    pub xfa: bool,
}

/// The catalog's `/AcroForm` dictionary (ISO 32000-1 §12.7.2), `None` when
/// the catalog has none. Indirect values are followed; an entry of the
/// wrong type reads as absent.
///
/// Covers ISO 32000-1 §12.7.2.
pub async fn interactive_form_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<InteractiveForm> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let dict = resolved_dict(src, catalog.get("AcroForm")?).await?;
    let entries = Entries { src, dict: &dict };
    let fields = references(entries.value("Fields").await.as_ref());
    let calculation_order = references(entries.value("CO").await.as_ref());
    let need_appearances = entries.flag("NeedAppearances").await.unwrap_or(false);
    let signature_flags = entries
        .value("SigFlags")
        .await
        .and_then(|bits| bits.as_int())
        .map_or_else(SignatureFlags::default, SignatureFlags::from_bits);
    let default_resources = match dict.get("DR") {
        Some(entry) => resolved_dict(src, entry).await,
        None => None,
    };
    let default_appearance = entries
        .value("DA")
        .await
        .and_then(|da| Some(String::from_utf8_lossy(da.as_str_bytes()?).into_owned()));
    let quadding = entries
        .value("Q")
        .await
        .and_then(|q| q.as_int())
        .and_then(Quadding::from_int);
    let xfa = matches!(
        entries.value("XFA").await,
        Some(Object::Stream(_)) | Some(Object::Array(_))
    );
    Some(InteractiveForm {
        fields,
        need_appearances,
        signature_flags,
        calculation_order,
        default_resources,
        default_appearance,
        quadding,
        xfa,
    })
}

/// The references an array holds, anything else in it skipped; empty for a
/// value that is no array.
fn references(value: Option<&Object>) -> Vec<ObjRef> {
    value
        .and_then(Object::as_array)
        .map(|items| items.iter().filter_map(Object::as_ref).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{InteractiveForm, Quadding, SignatureFlags};
    use crate::object::ObjRef;
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

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    /// Every entry of Table 218 set, the dictionary itself and its flags
    /// indirect; a direct dictionary in `/Fields` is not a reference and is
    /// skipped.
    // Covers ISO 32000-1 §12.7.2.
    #[test]
    fn the_interactive_form_dictionary_is_read() {
        let found = doc(
            "/AcroForm 4 0 R",
            &[
                (
                    4,
                    "<< /Fields [5 0 R << /T (inline) >> 6 0 R] /NeedAppearances true \
                     /SigFlags 7 0 R /CO [6 0 R] /DR << /Font << /Helv 8 0 R >> >> \
                     /DA (/Helv 0 Tf 0 g) /Q 1 /XFA [(preamble) 9 0 R] >>",
                ),
                (5, "<< /T (a) /FT /Tx >>"),
                (6, "<< /T (b) /FT /Tx >>"),
                (7, "3"),
                (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            ],
        )
        .interactive_form()
        .unwrap();
        assert_eq!(found.fields, vec![r(5), r(6)]);
        assert!(found.need_appearances);
        assert_eq!(
            found.signature_flags,
            SignatureFlags {
                signatures_exist: true,
                append_only: true,
            }
        );
        assert_eq!(found.calculation_order, vec![r(6)]);
        assert!(found
            .default_resources
            .as_ref()
            .and_then(|dr| dr.get_dict("Font"))
            .is_some());
        assert_eq!(found.default_appearance.as_deref(), Some("/Helv 0 Tf 0 g"));
        assert_eq!(found.quadding, Some(Quadding::Centered));
        assert!(found.xfa);
    }

    /// A catalog without the dictionary has no form; an empty dictionary
    /// holds the table's defaults; a quadding the table does not define and
    /// a `/Fields` that is no array read as absent.
    // Covers ISO 32000-1 §12.7.2.
    #[test]
    fn missing_and_malformed_form_entries_take_the_defaults() {
        assert_eq!(doc("", &[]).interactive_form(), None);
        assert_eq!(doc("/AcroForm 5", &[]).interactive_form(), None);
        assert_eq!(
            doc("/AcroForm << >>", &[]).interactive_form(),
            Some(InteractiveForm {
                fields: Vec::new(),
                need_appearances: false,
                signature_flags: SignatureFlags::default(),
                calculation_order: Vec::new(),
                default_resources: None,
                default_appearance: None,
                quadding: None,
                xfa: false,
            })
        );
        let odd = doc(
            "/AcroForm << /Fields 5 0 R /Q 7 /SigFlags 4 >>",
            &[(5, "(x)")],
        )
        .interactive_form()
        .unwrap();
        assert_eq!(odd.fields, Vec::new());
        assert_eq!(odd.quadding, None);
        assert_eq!(odd.signature_flags, SignatureFlags::default());
    }
}
