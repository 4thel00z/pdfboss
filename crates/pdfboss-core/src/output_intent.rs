//! Output intents (ISO 32000-1 §14.11.5): the catalog's `/OutputIntents`
//! array, read as data. Rendering does not apply a destination profile.

use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// An output intent dictionary (ISO 32000-1 §14.11.5, Table 365): the colour
/// reproduction characteristics of one intended output device or production
/// condition, as a PDF/X, PDF/A or PDF/E file declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputIntent {
    /// `/S`: the subtype naming the standard the intent belongs to:
    /// `GTS_PDFX`, `GTS_PDFA1`, `ISO_PDFE1`, or a name an extension defines.
    pub subtype: String,
    /// `/OutputCondition`: the intended output condition, worded for people.
    pub output_condition: Option<String>,
    /// `/OutputConditionIdentifier`: the condition's name in a registry such
    /// as the ICC characterization data registry, `Custom`, or an
    /// application's own name. Required by the table, `None` when missing.
    pub output_condition_identifier: Option<String>,
    /// `/RegistryName`: the registry, conventionally a URI, that defines the
    /// identifier.
    pub registry_name: Option<String>,
    /// `/Info`: further human-readable information about the condition.
    pub info: Option<String>,
    /// `/DestOutputProfile`: the ICC profile stream, by reference, that maps
    /// the document's source colours to the device's colorants; `None` when
    /// absent or written directly, a stream always being indirect.
    pub destination_profile: Option<ObjRef>,
}

/// The output intents the catalog's `/OutputIntents` array declares, in
/// array order: every entry that is a dictionary carrying the required `/S`
/// subtype. Other entries are skipped, and a catalog without the array, or
/// whose entry is no array, has none. Nothing here converts colours: the
/// clause leaves the data informational, and rendering ignores it.
///
/// Covers ISO 32000-1 §14.11.5.
pub async fn output_intents_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Vec<OutputIntent> {
    let mut intents = Vec::new();
    let Some(root) = trailer.get("Root") else {
        return intents;
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return intents;
    };
    let Some(entry) = catalog.get("OutputIntents") else {
        return intents;
    };
    let Ok(array) = src.resolve(entry).await else {
        return intents;
    };
    let Some(array) = array.as_array() else {
        return intents;
    };
    for value in array {
        let Some(dict) = resolved_dict(src, value).await else {
            continue;
        };
        let Some(subtype) = dict.get_name("S") else {
            continue;
        };
        let entries = Entries { src, dict: &dict };
        intents.push(OutputIntent {
            subtype: subtype.0.clone(),
            output_condition: entries.text("OutputCondition").await,
            output_condition_identifier: entries.text("OutputConditionIdentifier").await,
            registry_name: entries.text("RegistryName").await,
            info: entries.text("Info").await,
            destination_profile: dict.get("DestOutputProfile").and_then(Object::as_ref),
        });
    }
    intents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::object::ObjRef;
    use pdfboss_testkit::PdfBuilder;

    /// A document whose catalog carries `output_intents` as its
    /// `/OutputIntents` value, plus the clause's EXAMPLE 1 as object 5.
    fn with_intents(output_intents: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R /OutputIntents {output_intents} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(
            5,
            "<< /Type /OutputIntent /S /GTS_PDFX /OutputCondition (CGATS TR 001 (SWOP)) \
             /OutputConditionIdentifier (CGATS TR 001) /RegistryName (http://www.color.org) \
             /DestOutputProfile 100 0 R >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// The clause's EXAMPLE 1 by reference and a direct PDF/A intent with a
    /// UTF-16 `/Info`, in array order.
    // Covers ISO 32000-1 §14.11.5.
    #[test]
    fn reads_every_entry_of_the_output_intent_dictionaries() {
        let doc = with_intents(
            "[ 5 0 R << /S /GTS_PDFA1 /OutputConditionIdentifier (sRGB IEC61966-2.1) \
             /Info <FEFF007300520047004200200070007200650076006900650077> >> ]",
        );
        assert_eq!(
            doc.output_intents(),
            vec![
                OutputIntent {
                    subtype: "GTS_PDFX".to_string(),
                    output_condition: Some("CGATS TR 001 (SWOP)".to_string()),
                    output_condition_identifier: Some("CGATS TR 001".to_string()),
                    registry_name: Some("http://www.color.org".to_string()),
                    info: None,
                    destination_profile: Some(ObjRef { num: 100, gen: 0 }),
                },
                OutputIntent {
                    subtype: "GTS_PDFA1".to_string(),
                    output_condition: None,
                    output_condition_identifier: Some("sRGB IEC61966-2.1".to_string()),
                    registry_name: None,
                    info: Some("sRGB preview".to_string()),
                    destination_profile: None,
                },
            ]
        );
    }

    /// An intent without the required `/S`, a non-dictionary entry and a
    /// direct `/DestOutputProfile` (the table asks for a stream, which is
    /// always indirect); a catalog whose entry is no array has no intents.
    // Covers ISO 32000-1 §14.11.5.
    #[test]
    fn malformed_intents_are_skipped_and_a_direct_profile_is_no_reference() {
        let doc = with_intents(
            "[ << /Type /OutputIntent /OutputConditionIdentifier (nameless) >> 42 \
             << /S /ISO_PDFE1 /DestOutputProfile << /N 3 >> >> ]",
        );
        let intents = doc.output_intents();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].subtype, "ISO_PDFE1");
        assert_eq!(intents[0].destination_profile, None);
        assert_eq!(with_intents("<< /S /GTS_PDFX >>").output_intents(), vec![]);
        let plain = Document::load(pdfboss_testkit::simple_doc("plain")).unwrap();
        assert_eq!(plain.output_intents(), vec![]);
    }
}
