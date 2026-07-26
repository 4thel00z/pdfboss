//! Page rasterization for pdfboss: paths, fills, strokes, clipping, color
//! spaces, images and glyph outlines, rendered to an RGBA8 pixmap and
//! encodable as PNG.
//!
//! Glyph painting is staged behind [`GlyphPainting`] tiers: embedded
//! TrueType only, then every embedded font program (TrueType, CFF, Type1,
//! Type3), and finally `Full`, which additionally substitutes a
//! replacement face for a non-embedded simple font (see `crate::glyph` and
//! `crate::substitute` for the loader and the request/provider plumbing).
//! A substitute face comes from either a caller-supplied directory
//! ([`SubstituteSource::Dir`]) or the compiled-in OFL Croscore set
//! ([`SubstituteSource::Builtin`]), the latter gated behind this crate's
//! `substitute-fonts` Cargo feature and queryable at runtime via
//! [`builtin_fonts_available`]. Advance widths for a substituted
//! standard-14 font additionally consult Adobe Core-14 AFM tables
//! (`pdfboss_encoding::standard_14_width`) ahead of the substitute's own
//! `hmtx`, behind only the PDF's own `/Widths`.
//!
//! v1 limitations: `/Symbol` and `/ZapfDingbats` have no license-clean
//! substitute, so they stay unpainted at every tier rather than borrowing
//! an unrelated face's glyphs; a "bold" *sans* substitute request is not
//! visually distinct from regular weight (Arimo is a `[wght]` variable
//! font, rendered at its Regular instance -- only italic varies, via a
//! separate static face); and advancing *unpainted* non-embedded text at
//! `AllEmbedded` via the AFM tables is deferred to a later plan.

// The rasterizer modules are consumed by the content-stream executor; the
// `dead_code` allowances below disappear once it is wired up.
mod cff;
#[allow(dead_code)]
mod color;
mod executor;
mod glyph;
mod image;
#[allow(dead_code)]
mod path;
#[allow(dead_code)]
mod raster;
#[allow(dead_code)]
mod stroke;
#[allow(dead_code)]
mod substitute;
mod truetype;
mod type1;
mod type3;

use std::path::{Path, PathBuf};

use pdfboss_core::{Document, Error, Page, Result};

/// An RGBA8 raster image with straight (non-premultiplied) alpha, row-major
/// from the top-left.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    /// Pixel data, `width * height * 4` bytes (RGBA per pixel).
    pub data: Vec<u8>,
}

impl Pixmap {
    /// Creates a fully transparent pixmap.
    pub fn new(w: u32, h: u32) -> Pixmap {
        Pixmap {
            width: w,
            height: h,
            data: vec![0; w as usize * h as usize * 4],
        }
    }

    /// Fills every pixel with `rgba`.
    pub fn fill(&mut self, rgba: [u8; 4]) {
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
    }

    /// Encodes the pixmap as a PNG image.
    pub fn encode_png(&self) -> Result<Vec<u8>> {
        fn err(e: png::EncodingError) -> Error {
            Error::Other(format!("png encode: {e}"))
        }
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(err)?;
        writer.write_image_data(&self.data).map_err(err)?;
        writer.finish().map_err(err)?;
        Ok(out)
    }

    /// Encodes the pixmap as PNG and writes it to `path`.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.encode_png()?)?;
        Ok(())
    }
}

/// How aggressively the rasterizer turns text into filled outlines. Each tier is
/// a strict superset of the previous one; the difference is only observable once
/// the corresponding glyph loaders exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlyphPainting {
    /// Only embedded TrueType (`glyf`) outlines — the cheapest tier.
    EmbeddedTrueTypeOnly,
    /// Every embedded program: TrueType, CFF, Type1 and Type3. No bundled assets.
    #[default]
    AllEmbedded,
    /// Also substitute bundled or caller-provided faces for non-embedded fonts.
    Full,
}

impl GlyphPainting {
    /// Whether this tier paints every embedded program (CFF, Type1, Type3),
    /// not just embedded TrueType.
    pub fn paints_all_embedded(self) -> bool {
        !matches!(self, GlyphPainting::EmbeddedTrueTypeOnly)
    }
}

/// Where non-embedded glyph substitution (the `Full` [`GlyphPainting`] tier)
/// draws replacement faces from. The default, `None`, substitutes nothing --
/// `Full` behaves exactly like `AllEmbedded` until a caller opts in.
#[derive(Clone, Debug, Default)]
pub enum SubstituteSource {
    /// No substitution: non-embedded fonts stay unpainted.
    #[default]
    None,
    /// Compiled-in faces: the OFL Croscore set (Arimo/Tinos/Cousine,
    /// metric-compatible with Helvetica/Times/Courier) bundled via
    /// `include_bytes!` behind the `substitute-fonts` Cargo feature -- see
    /// [`builtin_fonts_available`] and `crate::substitute::BuiltinProvider`.
    /// Built without that feature, there are no compiled-in faces to hand
    /// out: `Builtin` degrades to no provider at all, so `Full` behaves
    /// exactly like `AllEmbedded` for non-embedded fonts, the same as
    /// `SubstituteSource::None`.
    Builtin,
    /// Faces read from a directory at render time (e.g. an installed
    /// `pdfboss-fonts` package), one file per style -- see
    /// `substitute::face_filename`.
    Dir(PathBuf),
}

/// Options controlling a single page render.
#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    /// Which font programs the rasterizer will paint.
    pub glyph_painting: GlyphPainting,
    /// Where `Full`-tier substitution draws replacement faces from. Ignored
    /// at every other tier.
    pub substitutes: SubstituteSource,
}

/// Whether this binary was built with the `substitute-fonts` feature, i.e.
/// whether `SubstituteSource::Builtin` has compiled-in faces to hand out.
/// Callers (e.g. the CLI) use this to give an actionable message when `Full`
/// is requested with no `--font-dir` and no compiled-in set, rather than
/// silently rendering as if `Full` had never been asked for.
pub fn builtin_fonts_available() -> bool {
    cfg!(feature = "substitute-fonts")
}

/// Renders a page at `scale` onto a white background. The pixel size is
/// `ceil(crop_w * scale) x ceil(crop_h * scale)` (after `/Rotate`), and the
/// base transform maps the crop box to device space with a y-flip and the
/// page rotation applied.
///
/// Rendering is lenient: content pdfboss cannot read is skipped rather than
/// failing the render, so a page can come back blank without an error. Use
/// [`render_page_reporting`] to find out what was dropped.
pub fn render_page(doc: &Document, page: &Page, scale: f32) -> Result<Pixmap> {
    render_page_with_options(doc, page, scale, &RenderOptions::default())
}

/// Renders a page like [`render_page`], honoring `opts` (currently the glyph
/// painting tier). See [`render_page`] for the geometry contract and for
/// what leniency means for the pixels you get back.
pub fn render_page_with_options(
    doc: &Document,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<Pixmap> {
    executor::render_page_reporting(doc, page, scale, opts).map(|(pix, _)| pix)
}

/// Renders a page like [`render_page_with_options`], additionally returning
/// a [`RenderReport`] describing any content that had to be dropped or
/// approximated. Use this when a silently blank page would be misleading.
pub fn render_page_reporting(
    doc: &Document,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<(Pixmap, RenderReport)> {
    executor::render_page_reporting(doc, page, scale, opts)
}

/// Upper bound on the distinct entries a [`RenderReport`] keeps. Repeats of
/// the same kind and reason only raise an existing entry's count, so this
/// bounds the report's memory for any page: a stream drawing the same
/// undecodable image a million times costs one entry, and a stream inventing
/// endlessly *different* failures stops growing the list here and counts the
/// rest in [`RenderReport::unlisted`].
const MAX_SKIPPED: usize = 64;

/// Which piece of page content a render could not reproduce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SkippedKind {
    /// The page's own content stream: nothing on the page was drawn.
    PageContents,
    /// An image XObject or an inline image.
    Image,
    /// A form XObject, and with it everything nested inside it.
    Form,
    /// A `Do` whose XObject resource is missing, is not a stream, or has no
    /// subtype this renderer knows how to draw.
    XObject,
    /// A shading (`sh`), which this renderer does not paint.
    Shading,
    /// A pattern fill or stroke, painted as flat mid-gray instead of the
    /// pattern's own content.
    Pattern,
    /// A mask that was ignored, so content the author masked out painted
    /// solid: an image `/SMask` or `/Mask`, or an `/ExtGState` `/SMask`.
    SoftMask,
    /// A blend mode other than `Normal`, painted as `Normal`.
    BlendMode,
    /// An annotation appearance stream: annotations are not painted.
    Annotation,
}

impl SkippedKind {
    /// The noun this kind reads as in [`RenderReport::summary`] and
    /// [`RenderReport::warnings`], pluralized for `n`.
    fn noun(self, n: u64) -> &'static str {
        let one = n == 1;
        match self {
            SkippedKind::PageContents if one => "content stream",
            SkippedKind::PageContents => "content streams",
            SkippedKind::Image if one => "image",
            SkippedKind::Image => "images",
            SkippedKind::Form if one => "form XObject",
            SkippedKind::Form => "form XObjects",
            SkippedKind::XObject if one => "XObject",
            SkippedKind::XObject => "XObjects",
            SkippedKind::Shading if one => "shading",
            SkippedKind::Shading => "shadings",
            SkippedKind::Pattern if one => "pattern",
            SkippedKind::Pattern => "patterns",
            SkippedKind::SoftMask if one => "mask",
            SkippedKind::SoftMask => "masks",
            SkippedKind::BlendMode if one => "blend mode",
            SkippedKind::BlendMode => "blend modes",
            SkippedKind::Annotation if one => "annotation",
            SkippedKind::Annotation => "annotations",
        }
    }
}

/// Why a piece of page content was dropped or approximated during
/// rasterization.
///
/// Rendering is lenient: content pdfboss cannot read is skipped so the rest
/// of the page still rasterizes. This enum records *why*, so callers can
/// tell an intentionally blank page from a page whose content pdfboss could
/// not read.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// The stream's `/Filter` chain names a filter pdfboss does not decode.
    UnsupportedFilter(String),
    /// Reading the stream failed, carrying the underlying message: a filter
    /// that ran but gave up (corrupt data, size limit, ...), or a syntax
    /// error in a content stream.
    DecodeFailed(String),
    /// Filters applied cleanly but the bytes could not be interpreted (bad
    /// image dimensions, unparsable content stream, unsupported JPEG, ...).
    Undecodable,
    /// The stream held fewer samples than the image's dimensions and bit
    /// depth demand; the missing region painted as zero samples.
    Truncated,
    /// A resource the operator names is absent, or is not the kind of object
    /// the operator needs.
    Missing,
    /// pdfboss understands the construct but does not paint it yet, so it
    /// was omitted or approximated.
    Unsupported,
    /// A nesting or size guard stopped the render at this point.
    LimitExceeded,
}

impl std::fmt::Display for SkipReason {
    /// The reason as the clause after the colon of a warning line, e.g.
    /// `unsupported filter /JBIG2Decode`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::UnsupportedFilter(name) => write!(f, "unsupported filter /{name}"),
            SkipReason::DecodeFailed(msg) => f.write_str(msg),
            SkipReason::Undecodable => f.write_str("the data could not be interpreted"),
            SkipReason::Truncated => f.write_str("sample data ended early; the rest painted blank"),
            SkipReason::Missing => f.write_str("the resource is missing"),
            SkipReason::Unsupported => f.write_str("not supported yet"),
            SkipReason::LimitExceeded => f.write_str("a nesting limit stopped the render here"),
        }
    }
}

/// One kind of content dropped for one reason, with how often it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkippedContent {
    /// What was dropped.
    pub kind: SkippedKind,
    /// Why it was dropped.
    pub reason: SkipReason,
    /// How many times this exact kind/reason pair came up in the render.
    pub count: u64,
}

/// What a page render could not reproduce faithfully: content dropped
/// outright (an undecodable image, an unreadable form) and content painted
/// as an approximation (a pattern fill as flat gray). Empty means every
/// construct the render encountered was painted as the page describes it.
///
/// Two things are deliberately *not* reported, because they are configured
/// behavior rather than a failure: text left unpainted by the requested
/// [`GlyphPainting`] tier, and content clipped or transformed off the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderReport {
    /// Distinct drops in the order first encountered, at most 64 entries
    /// (see `count` for repeats and [`RenderReport::unlisted`] for the
    /// overflow).
    pub skipped: Vec<SkippedContent>,
    /// Drops that arrived after `skipped` reached its 64-entry cap and so
    /// are counted but not described.
    pub unlisted: u64,
}

impl RenderReport {
    /// Whether the page rasterized with nothing dropped or approximated.
    pub fn is_empty(&self) -> bool {
        self.skipped.is_empty() && self.unlisted == 0
    }

    /// A one-line human summary counting drops per kind, or `None` when
    /// nothing was dropped: `"2 images, 1 shading skipped"`.
    pub fn summary(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut totals: Vec<(SkippedKind, u64)> = Vec::new();
        for item in &self.skipped {
            match totals.iter_mut().find(|(kind, _)| *kind == item.kind) {
                Some((_, n)) => *n = n.saturating_add(item.count),
                None => totals.push((item.kind, item.count)),
            }
        }
        let mut parts: Vec<String> = totals
            .iter()
            .map(|(kind, n)| format!("{n} {}", kind.noun(*n)))
            .collect();
        if self.unlisted > 0 {
            parts.push(format!("{} more", self.unlisted));
        }
        Some(format!("{} skipped", parts.join(", ")))
    }

    /// One human-readable line per distinct drop, for callers that warn
    /// about them: `"1 image skipped: unsupported filter /JBIG2Decode"`.
    pub fn warnings(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .skipped
            .iter()
            .map(|item| {
                format!(
                    "{} {} skipped: {}",
                    item.count,
                    item.kind.noun(item.count),
                    item.reason
                )
            })
            .collect();
        if self.unlisted > 0 {
            out.push(format!(
                "{} further drops not described (report limit reached)",
                self.unlisted
            ));
        }
        out
    }

    /// Records one drop, merging it into an existing entry when the same
    /// kind and reason already happened. Beyond [`MAX_SKIPPED`] distinct
    /// entries the drop is only counted, so a page drawing endlessly varied
    /// broken content cannot grow this report without bound.
    pub(crate) fn record(&mut self, kind: SkippedKind, reason: SkipReason) {
        if let Some(item) = self
            .skipped
            .iter_mut()
            .find(|item| item.kind == kind && item.reason == reason)
        {
            item.count = item.count.saturating_add(1);
            return;
        }
        if self.skipped.len() >= MAX_SKIPPED {
            self.unlisted = self.unlisted.saturating_add(1);
            return;
        }
        self.skipped.push(SkippedContent {
            kind,
            reason,
            count: 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pixmap_is_transparent() {
        let pix = Pixmap::new(3, 2);
        assert_eq!(pix.width, 3);
        assert_eq!(pix.height, 2);
        assert_eq!(pix.data.len(), 24);
        assert!(pix.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn fill_sets_every_pixel() {
        let mut pix = Pixmap::new(2, 2);
        pix.fill([1, 2, 3, 4]);
        assert_eq!(pix.data, [1, 2, 3, 4].repeat(4));
    }

    #[test]
    fn png_round_trip_preserves_pixels() {
        let mut pix = Pixmap::new(3, 2);
        for (i, b) in pix.data.iter_mut().enumerate() {
            *b = (i * 11 % 256) as u8;
        }
        let bytes = pix.encode_png().expect("encode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

        let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
        let mut reader = decoder.read_info().expect("read_info");
        let mut buf = vec![0u8; reader.output_buffer_size().expect("size")];
        let info = reader.next_frame(&mut buf).expect("frame");
        assert_eq!(info.width, 3);
        assert_eq!(info.height, 2);
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(&buf[..info.buffer_size()], &pix.data[..]);
    }

    #[test]
    fn report_merges_repeats_and_counts_per_kind() {
        let mut report = RenderReport::default();
        assert!(report.is_empty());
        report.record(SkippedKind::Image, SkipReason::Undecodable);
        report.record(SkippedKind::Image, SkipReason::Undecodable);
        report.record(
            SkippedKind::Image,
            SkipReason::UnsupportedFilter("JBIG2Decode".to_string()),
        );
        report.record(SkippedKind::Shading, SkipReason::Unsupported);

        assert!(!report.is_empty());
        assert_eq!(report.skipped.len(), 3, "same kind and reason merge");
        assert_eq!(report.skipped[0].count, 2);
        // The summary counts per kind, so the two image reasons add up.
        assert_eq!(
            report.summary().as_deref(),
            Some("3 images, 1 shading skipped"),
        );
        assert_eq!(
            report.warnings(),
            vec![
                "2 images skipped: the data could not be interpreted".to_string(),
                "1 image skipped: unsupported filter /JBIG2Decode".to_string(),
                "1 shading skipped: not supported yet".to_string(),
            ],
        );
    }

    #[test]
    fn report_stops_listing_at_the_cap_but_keeps_counting() {
        let mut report = RenderReport::default();
        for i in 0..MAX_SKIPPED + 5 {
            report.record(SkippedKind::Image, SkipReason::DecodeFailed(i.to_string()));
        }
        assert_eq!(report.skipped.len(), MAX_SKIPPED);
        assert_eq!(report.unlisted, 5);
        assert_eq!(
            report.summary().as_deref(),
            Some("64 images, 5 more skipped"),
        );
    }

    #[test]
    fn save_png_writes_decodable_file() {
        let mut pix = Pixmap::new(4, 4);
        pix.fill([10, 20, 30, 255]);
        let dir = std::env::temp_dir().join("pdfboss-render-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pix.png");
        pix.save_png(&path).expect("save");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(&path).ok();
    }
}
