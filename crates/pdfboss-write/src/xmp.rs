//! XMP metadata packet generation. The packet is derived from the same
//! `Metadata` value written to the `/Info` dictionary, so the two never
//! drift apart. No `xmpMM:InstanceID`, no `xmpMM:DocumentID`, no generated
//! timestamps: every value traces back to a caller-supplied field, keeping
//! the packet as reproducible as the rest of the writer.

use crate::pdf::Metadata;

/// The fixed packet-wrapper id from the XMP specification — not a per-file
/// document or instance identifier, the same constant appears in every
/// XMP packet ever written.
const PACKET_ID: &str = "W5M0MpCehiHzreSzNTczkc9d";

/// Builds a minimal RDF/XMP packet from `meta`. Each `dc:`/`pdf:`/`xmp:`
/// element is present only when its `Metadata` field is `Some`.
pub(crate) fn packet(meta: &Metadata) -> Vec<u8> {
    let mut elements = String::new();
    if let Some(title) = &meta.title {
        elements.push_str(&alt_element("dc:title", title));
    }
    if let Some(author) = &meta.author {
        elements.push_str(&seq_element("dc:creator", author));
    }
    if let Some(subject) = &meta.subject {
        elements.push_str(&alt_element("dc:description", subject));
    }
    if let Some(keywords) = &meta.keywords {
        elements.push_str(&leaf_element("pdf:Keywords", keywords));
    }
    if let Some(producer) = &meta.producer {
        elements.push_str(&leaf_element("pdf:Producer", producer));
    }
    if let Some(creator) = &meta.creator {
        elements.push_str(&leaf_element("xmp:CreatorTool", creator));
    }
    if let Some(date) = meta.creation_date {
        elements.push_str(&leaf_element("xmp:CreateDate", &date.to_iso8601()));
    }
    if let Some(date) = meta.modification_date {
        elements.push_str(&leaf_element("xmp:ModifyDate", &date.to_iso8601()));
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"{PACKET_ID}\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
         <rdf:Description rdf:about=\"\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n\
         {elements}</rdf:Description>\n\
         </rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>"
    )
    .into_bytes()
}

/// One leaf element: `<tag>escaped-value</tag>`.
fn leaf_element(tag: &str, value: &str) -> String {
    format!("<{tag}>{}</{tag}>\n", escape(value))
}

/// An `rdf:Alt` element with a single `x-default` language alternative —
/// the RDF shape XMP expects for free-text fields like title and
/// description.
fn alt_element(tag: &str, value: &str) -> String {
    format!(
        "<{tag}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{tag}>\n",
        escape(value)
    )
}

/// An `rdf:Seq` element with a single entry — the RDF shape XMP expects
/// for ordered lists like `dc:creator`.
fn seq_element(tag: &str, value: &str) -> String {
    format!(
        "<{tag}><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></{tag}>\n",
        escape(value)
    )
}

/// Escapes the five XML special characters in `value`.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::Date;

    fn full_metadata() -> Metadata {
        Metadata {
            title: Some("Report".to_string()),
            author: Some("Jane Doe".to_string()),
            subject: Some("Quarterly numbers".to_string()),
            keywords: Some("finance, quarterly".to_string()),
            creator: Some("pdfboss".to_string()),
            producer: Some("pdfboss-write".to_string()),
            creation_date: Some(Date {
                year: 2026,
                month: 8,
                day: 27,
                hour: 12,
                minute: 30,
                second: 15,
                utc_offset_minutes: 0,
            }),
            modification_date: Some(Date {
                year: 2026,
                month: 8,
                day: 28,
                hour: 9,
                minute: 0,
                second: 0,
                utc_offset_minutes: 120,
            }),
        }
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("packet is valid UTF-8")
    }

    #[test]
    fn full_metadata_maps_every_field() {
        let xml = text(packet(&full_metadata()));
        assert!(xml.contains(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Report</rdf:li></rdf:Alt></dc:title>"
        ));
        assert!(
            xml.contains("<dc:creator><rdf:Seq><rdf:li>Jane Doe</rdf:li></rdf:Seq></dc:creator>")
        );
        assert!(xml.contains(
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">Quarterly numbers</rdf:li></rdf:Alt></dc:description>"
        ));
        assert!(xml.contains("<pdf:Keywords>finance, quarterly</pdf:Keywords>"));
        assert!(xml.contains("<pdf:Producer>pdfboss-write</pdf:Producer>"));
        assert!(xml.contains("<xmp:CreatorTool>pdfboss</xmp:CreatorTool>"));
        assert!(xml.contains("<xmp:CreateDate>2026-08-27T12:30:15Z</xmp:CreateDate>"));
        assert!(xml.contains("<xmp:ModifyDate>2026-08-28T09:00:00+02:00</xmp:ModifyDate>"));
    }

    #[test]
    fn absent_fields_write_no_element() {
        let xml = text(packet(&Metadata::default()));
        assert!(!xml.contains("dc:title"));
        assert!(!xml.contains("dc:creator"));
        assert!(!xml.contains("dc:description"));
        assert!(!xml.contains("pdf:Keywords"));
        assert!(!xml.contains("pdf:Producer"));
        assert!(!xml.contains("xmp:CreatorTool"));
        assert!(!xml.contains("xmp:CreateDate"));
        assert!(!xml.contains("xmp:ModifyDate"));
    }

    #[test]
    fn never_contains_instance_or_document_ids() {
        let xml = text(packet(&full_metadata()));
        assert!(!xml.contains("InstanceID"));
        assert!(!xml.contains("DocumentID"));
        assert!(!xml.contains("xmpMM"));
    }

    #[test]
    fn special_characters_are_escaped() {
        let meta = Metadata {
            title: Some("<&>\"'".to_string()),
            ..Metadata::default()
        };
        let xml = text(packet(&meta));
        assert!(xml.contains("&lt;&amp;&gt;&quot;&apos;"));
        assert!(!xml.contains("<&>\"'"));
    }

    #[test]
    fn same_metadata_produces_identical_packets() {
        assert_eq!(packet(&full_metadata()), packet(&full_metadata()));
    }
}
