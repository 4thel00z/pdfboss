//! Non-embedded font substitution: deriving a face request from a PDF font
//! dictionary (ISO 32000-1 Table 121, `/FontDescriptor /Flags`) and mapping
//! that request onto a replacement face, either compiled in or read from a
//! directory at render time.
//!
//! This module only defines the request/lookup plumbing; nothing in the
//! loader (`glyph.rs`) consults it yet -- that lands in later plans once the
//! `Full` tier actually substitutes.

use pdfboss_core::{Dict, Document};

/// The three families a substitute request maps a non-embedded font to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Serif,
    Sans,
    Mono,
}

/// A style request derived from a font's `/BaseFont` name and
/// `/FontDescriptor /Flags`, used to pick a substitute face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FaceRequest {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
}

/// A source of substitute face programs, keyed by [`FaceRequest`].
pub(crate) trait SubstituteProvider {
    /// Returns the face's raw font program bytes, or `None` if this provider
    /// has no face for `req` (a missing directory or file, say).
    fn face(&self, req: &FaceRequest) -> Option<Vec<u8>>;
}

/// `/FontDescriptor /Flags` bits consulted here (ISO 32000-1 Table 121).
const FLAG_FIXED_PITCH: i64 = 0x1;
const FLAG_SERIF: i64 = 0x2;
const FLAG_ITALIC: i64 = 0x40;
const FLAG_FORCE_BOLD: i64 = 0x40000;

/// Strips a subset prefix (`ABCDEF+Name` -> `Name`, ISO 32000-1 9.6.4): six
/// uppercase ASCII letters followed by `+`. Names that don't match this exact
/// shape (wrong length, non-uppercase, no `+`) pass through unchanged.
fn strip_subset_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(u8::is_ascii_uppercase) {
        &name[7..]
    } else {
        name
    }
}

impl FaceRequest {
    /// Derives a request from `/BaseFont` (subset prefix stripped, matched
    /// case-insensitively) and the font's `FontDescriptor /Flags`. `None` for
    /// Symbol/ZapfDingbats, which have no license-clean substitute in v1.
    pub(crate) fn from_font_dict(doc: &Document, font: &Dict) -> Option<FaceRequest> {
        let base = font
            .get_name("BaseFont")
            .map(|n| n.0.as_str())
            .unwrap_or("");
        let stripped = strip_subset_prefix(base);
        let lower = stripped.to_lowercase();
        if lower == "symbol" || lower == "zapfdingbats" {
            return None;
        }

        let flags = font
            .get("FontDescriptor")
            .and_then(|o| doc.resolve(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .and_then(|fd| fd.get_int("Flags"))
            .unwrap_or(0);

        let family = if flags & FLAG_FIXED_PITCH != 0
            || lower.contains("courier")
            || lower.contains("mono")
            || lower.contains("consol")
        {
            Family::Mono
        } else if flags & FLAG_SERIF != 0
            || lower.contains("times")
            || lower.contains("georgia")
            || lower.contains("serif")
            || lower.contains("roman")
            || lower.contains("minion")
        {
            Family::Serif
        } else {
            Family::Sans
        };

        let bold = flags & FLAG_FORCE_BOLD != 0 || lower.contains("bold");
        let italic =
            flags & FLAG_ITALIC != 0 || lower.contains("italic") || lower.contains("oblique");

        Some(FaceRequest {
            family,
            bold,
            italic,
        })
    }
}

/// The bundled-substitute filename for `req`: Arimo (sans), Tinos (serif) or
/// Cousine (mono) -- the metric-compatible Liberation-family faces used as
/// the standard substitute set. Sans only varies by italic (weight rides the
/// variable-font axis, `[wght]`); serif and mono pick one of the four static
/// styles.
pub(crate) fn face_filename(req: &FaceRequest) -> &'static str {
    match req.family {
        Family::Sans => {
            if req.italic {
                "Arimo-Italic[wght].ttf"
            } else {
                "Arimo[wght].ttf"
            }
        }
        Family::Serif => match (req.bold, req.italic) {
            (false, false) => "Tinos-Regular.ttf",
            (true, false) => "Tinos-Bold.ttf",
            (false, true) => "Tinos-Italic.ttf",
            (true, true) => "Tinos-BoldItalic.ttf",
        },
        Family::Mono => match (req.bold, req.italic) {
            (false, false) => "Cousine-Regular.ttf",
            (true, false) => "Cousine-Bold.ttf",
            (false, true) => "Cousine-Italic.ttf",
            (true, true) => "Cousine-BoldItalic.ttf",
        },
    }
}

/// Reads substitute faces from a runtime directory (e.g. an installed
/// `pdfboss-fonts` package), one file per [`face_filename`].
pub(crate) struct DirProvider {
    pub dir: std::path::PathBuf,
}

impl SubstituteProvider for DirProvider {
    fn face(&self, req: &FaceRequest) -> Option<Vec<u8>> {
        std::fs::read(self.dir.join(face_filename(req))).ok()
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_core::{Document, ObjRef, Object};
    use pdfboss_testkit::PdfBuilder;

    use super::*;

    /// Builds a minimal one-page PDF with a simple font (object 5, the given
    /// `/BaseFont`) whose `FontDescriptor` (object 6) carries `flags`, and
    /// returns `FaceRequest::from_font_dict`'s result for that font dict.
    fn req_for(base: &str, flags: i64) -> Option<FaceRequest> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"");
        b.object(
            5,
            &format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /{base} \
                 /FontDescriptor 6 0 R >>"
            ),
        );
        b.object(
            6,
            &format!("<< /Type /FontDescriptor /FontName /{base} /Flags {flags} >>"),
        );
        let bytes = b.build(1);
        let doc = Document::load(bytes).expect("load");
        let font = doc
            .resolve(&Object::Ref(ObjRef { num: 5, gen: 0 }))
            .expect("resolve font")
            .as_dict()
            .cloned()
            .expect("font is a dict");
        FaceRequest::from_font_dict(&doc, &font)
    }

    #[test]
    fn times_bold_is_serif_and_bold() {
        let req = req_for("Times-Bold", 0).expect("some request");
        assert_eq!(req.family, Family::Serif);
        assert!(req.bold);
        assert!(!req.italic);
    }

    #[test]
    fn courier_is_mono() {
        let req = req_for("Courier", 0).expect("some request");
        assert_eq!(req.family, Family::Mono);
        assert!(!req.bold);
        assert!(!req.italic);
    }

    #[test]
    fn helvetica_oblique_is_sans_and_italic() {
        let req = req_for("Helvetica-Oblique", 0).expect("some request");
        assert_eq!(req.family, Family::Sans);
        assert!(!req.bold);
        assert!(req.italic);
    }

    #[test]
    fn subset_prefix_stripped_before_matching_and_serif_flag_wins() {
        // "Garamond" matches no name keyword; only the /Flags Serif bit (0x2)
        // makes this Serif. The ABCDEF+ subset prefix must not leak into the
        // match (e.g. by defeating the lowercase name check).
        let req = req_for("ABCDEF+Garamond", 0x2).expect("some request");
        assert_eq!(req.family, Family::Serif);
    }

    #[test]
    fn symbol_and_zapfdingbats_have_no_substitute() {
        assert_eq!(req_for("Symbol", 0), None);
        assert_eq!(req_for("ZapfDingbats", 0), None);
    }

    #[test]
    fn force_bold_flag_without_name_hint_is_bold() {
        let req = req_for("SomeFace", 0x40000).expect("some request");
        assert!(req.bold);
    }

    #[test]
    fn italic_flag_without_name_hint_is_italic() {
        let req = req_for("SomeFace", 0x40).expect("some request");
        assert!(req.italic);
    }

    #[test]
    fn fixed_pitch_flag_without_name_hint_is_mono() {
        let req = req_for("SomeFace", 0x1).expect("some request");
        assert_eq!(req.family, Family::Mono);
    }

    fn req(family: Family, bold: bool, italic: bool) -> FaceRequest {
        FaceRequest {
            family,
            bold,
            italic,
        }
    }

    #[test]
    fn face_filename_sans_ignores_bold_varies_by_italic() {
        assert_eq!(
            face_filename(&req(Family::Sans, false, false)),
            "Arimo[wght].ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Sans, true, false)),
            "Arimo[wght].ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Sans, false, true)),
            "Arimo-Italic[wght].ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Sans, true, true)),
            "Arimo-Italic[wght].ttf"
        );
    }

    #[test]
    fn face_filename_serif_every_style() {
        assert_eq!(
            face_filename(&req(Family::Serif, false, false)),
            "Tinos-Regular.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Serif, true, false)),
            "Tinos-Bold.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Serif, false, true)),
            "Tinos-Italic.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Serif, true, true)),
            "Tinos-BoldItalic.ttf"
        );
    }

    #[test]
    fn face_filename_mono_every_style() {
        assert_eq!(
            face_filename(&req(Family::Mono, false, false)),
            "Cousine-Regular.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Mono, true, false)),
            "Cousine-Bold.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Mono, false, true)),
            "Cousine-Italic.ttf"
        );
        assert_eq!(
            face_filename(&req(Family::Mono, true, true)),
            "Cousine-BoldItalic.ttf"
        );
    }

    #[test]
    fn dir_provider_reads_matching_file() {
        let dir = std::env::temp_dir().join(format!(
            "pdfboss-substitute-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let r = req(Family::Serif, true, false);
        let filename = face_filename(&r);
        std::fs::write(dir.join(filename), b"fake face bytes").expect("write fixture face");

        let provider = DirProvider { dir: dir.clone() };
        assert_eq!(provider.face(&r), Some(b"fake face bytes".to_vec()));

        // A style with no file on disk yields None, not a panic.
        let missing = req(Family::Mono, false, true);
        assert_eq!(provider.face(&missing), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dir_provider_missing_dir_yields_none() {
        let provider = DirProvider {
            dir: std::path::PathBuf::from("/no/such/pdfboss-substitute-dir"),
        };
        assert_eq!(provider.face(&req(Family::Sans, false, false)), None);
    }
}
