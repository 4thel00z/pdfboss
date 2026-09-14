//! Legal content attestations (ISO 32000-1 §12.8.5): the catalog's `/Legal`
//! dictionary, which counts the content a certifying signature could not vouch
//! for and carries the signer's attestation about it.

use crate::object::Dict;
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// The catalog's legal attestation dictionary (ISO 32000-1 §12.8.5, Table
/// 259): how much content of each kind that a certifying signature cannot
/// vouch for the document holds, and the signer's statement about it. Every
/// count is read as written, an absent one as 0; nothing is recounted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegalAttestation {
    /// `/JavaScriptActions`: JavaScript actions.
    pub java_script_actions: u32,
    /// `/LaunchActions`: launch actions.
    pub launch_actions: u32,
    /// `/URIActions`: URI actions.
    pub uri_actions: u32,
    /// `/MovieActions`: movie actions.
    pub movie_actions: u32,
    /// `/SoundActions`: sound actions.
    pub sound_actions: u32,
    /// `/HideAnnotationActions`: hide actions.
    pub hide_annotation_actions: u32,
    /// `/GoToRemoteActions`: remote go-to actions.
    pub go_to_remote_actions: u32,
    /// `/AlternateImages`: alternate images.
    pub alternate_images: u32,
    /// `/ExternalStreams`: streams read from outside the file.
    pub external_streams: u32,
    /// `/TrueTypeFonts`: TrueType fonts.
    pub true_type_fonts: u32,
    /// `/ExternalRefXobjects`: reference XObjects.
    pub external_ref_xobjects: u32,
    /// `/ExternalOPIdicts`: OPI dictionaries.
    pub external_opi_dicts: u32,
    /// `/NonEmbeddedFonts`: fonts without an embedded program.
    pub non_embedded_fonts: u32,
    /// `/DevDepGS_OP`: graphics state parameter dictionaries setting overprint.
    pub dev_dep_gs_op: u32,
    /// `/DevDepGS_HT`: graphics state parameter dictionaries with a halftone.
    pub dev_dep_gs_ht: u32,
    /// `/DevDepGS_TR`: graphics state parameter dictionaries with a transfer
    /// function.
    pub dev_dep_gs_tr: u32,
    /// `/DevDepGS_UCR`: graphics state parameter dictionaries with undercolour
    /// removal.
    pub dev_dep_gs_ucr: u32,
    /// `/DevDepGS_BG`: graphics state parameter dictionaries with black
    /// generation.
    pub dev_dep_gs_bg: u32,
    /// `/DevDepGS_FL`: graphics state parameter dictionaries with a flatness
    /// tolerance.
    pub dev_dep_gs_fl: u32,
    /// `/Annotations`: annotations.
    pub annotations: u32,
    /// `/OptionalContent`: optional content groups.
    pub optional_content: u32,
    /// `/Attestation`: the signer's statement about the counted content.
    pub attestation: Option<String>,
}

/// The catalog's `/Legal` dictionary: `None` without one or when the entry
/// is no dictionary. A count that is no non-negative integer reads as 0 and
/// an attestation that is no string as absent.
///
/// Covers ISO 32000-1 §12.8.5.
pub async fn legal_attestation_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<LegalAttestation> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let dict = resolved_dict(src, catalog.get("Legal")?).await?;
    let entries = Entries { src, dict: &dict };
    let mut legal = LegalAttestation {
        attestation: entries.text("Attestation").await,
        ..LegalAttestation::default()
    };
    for (key, count) in [
        ("JavaScriptActions", &mut legal.java_script_actions),
        ("LaunchActions", &mut legal.launch_actions),
        ("URIActions", &mut legal.uri_actions),
        ("MovieActions", &mut legal.movie_actions),
        ("SoundActions", &mut legal.sound_actions),
        ("HideAnnotationActions", &mut legal.hide_annotation_actions),
        ("GoToRemoteActions", &mut legal.go_to_remote_actions),
        ("AlternateImages", &mut legal.alternate_images),
        ("ExternalStreams", &mut legal.external_streams),
        ("TrueTypeFonts", &mut legal.true_type_fonts),
        ("ExternalRefXobjects", &mut legal.external_ref_xobjects),
        ("ExternalOPIdicts", &mut legal.external_opi_dicts),
        ("NonEmbeddedFonts", &mut legal.non_embedded_fonts),
        ("DevDepGS_OP", &mut legal.dev_dep_gs_op),
        ("DevDepGS_HT", &mut legal.dev_dep_gs_ht),
        ("DevDepGS_TR", &mut legal.dev_dep_gs_tr),
        ("DevDepGS_UCR", &mut legal.dev_dep_gs_ucr),
        ("DevDepGS_BG", &mut legal.dev_dep_gs_bg),
        ("DevDepGS_FL", &mut legal.dev_dep_gs_fl),
        ("Annotations", &mut legal.annotations),
        ("OptionalContent", &mut legal.optional_content),
    ] {
        *count = entries
            .value(key)
            .await
            .and_then(|value| value.as_int())
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
    }
    Some(legal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A document whose catalog carries `catalog_extra`.
    fn doc_with(catalog_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        Document::load(b.build(1)).unwrap()
    }

    /// Every Table 259 count reads into its field, an absent count as 0,
    /// and the attestation as text.
    // Covers ISO 32000-1 §12.8.5.
    #[test]
    fn reads_the_legal_attestation_counts() {
        let legal = doc_with(
            "/Legal << /JavaScriptActions 2 /LaunchActions 1 /URIActions 3 /MovieActions 4 \
             /SoundActions 5 /HideAnnotationActions 6 /GoToRemoteActions 7 /AlternateImages 8 \
             /ExternalStreams 9 /TrueTypeFonts 10 /ExternalRefXobjects 11 /ExternalOPIdicts 12 \
             /NonEmbeddedFonts 13 /DevDepGS_OP 14 /DevDepGS_HT 15 /DevDepGS_TR 16 /DevDepGS_UCR 17 \
             /DevDepGS_BG 18 /DevDepGS_FL 19 /Annotations 20 /OptionalContent 21 \
             /Attestation (Reviewed by counsel) >>",
        )
        .legal_attestation()
        .unwrap();
        assert_eq!(
            legal,
            LegalAttestation {
                java_script_actions: 2,
                launch_actions: 1,
                uri_actions: 3,
                movie_actions: 4,
                sound_actions: 5,
                hide_annotation_actions: 6,
                go_to_remote_actions: 7,
                alternate_images: 8,
                external_streams: 9,
                true_type_fonts: 10,
                external_ref_xobjects: 11,
                external_opi_dicts: 12,
                non_embedded_fonts: 13,
                dev_dep_gs_op: 14,
                dev_dep_gs_ht: 15,
                dev_dep_gs_tr: 16,
                dev_dep_gs_ucr: 17,
                dev_dep_gs_bg: 18,
                dev_dep_gs_fl: 19,
                annotations: 20,
                optional_content: 21,
                attestation: Some("Reviewed by counsel".into()),
            }
        );
        let sparse = doc_with("/Legal << /URIActions 3 >>")
            .legal_attestation()
            .unwrap();
        assert_eq!(sparse.uri_actions, 3);
        assert_eq!(sparse.non_embedded_fonts, 0);
        assert_eq!(sparse.attestation, None);
    }

    /// A catalog without `/Legal`, or whose entry is no dictionary, has no
    /// attestation; an empty dictionary reads as all zero; a count that is
    /// no non-negative integer reads as 0 and an attestation that is no
    /// string as absent.
    // Covers ISO 32000-1 §12.8.5.
    #[test]
    fn missing_or_malformed_legal_reads_as_absent() {
        assert_eq!(doc_with("").legal_attestation(), None);
        assert_eq!(doc_with("/Legal 5").legal_attestation(), None);
        assert_eq!(
            doc_with("/Legal << >>").legal_attestation(),
            Some(LegalAttestation::default())
        );
        let odd = doc_with("/Legal << /URIActions (x) /Annotations -3 /Attestation 7 >>")
            .legal_attestation()
            .unwrap();
        assert_eq!(odd.uri_actions, 0);
        assert_eq!(odd.annotations, 0);
        assert_eq!(odd.attestation, None);
    }
}
