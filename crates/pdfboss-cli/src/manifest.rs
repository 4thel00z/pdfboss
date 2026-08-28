//! `pdfboss create manifest`: a TOML document description — metadata, pages
//! and page content (text, paragraphs, images, links) — mapped onto the
//! compose vocabulary in `pdfboss_write`.

use std::path::Path;

use serde::Deserialize;

use pdfboss_core::Point;
use pdfboss_write::{
    Content, Image, ImageData, Link, LinkTarget, Metadata, Page, PageSize, Paragraph,
    ParagraphAlign, Pdf, Standard14, Text,
};

/// The manifest's top-level shape: optional document metadata plus its
/// pages, in reading order.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    meta: Option<ManifestMeta>,
    #[serde(rename = "page", default)]
    pages: Vec<ManifestPage>,
}

/// `[meta]`: maps directly onto `pdfboss_write::Metadata`'s text fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestMeta {
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    keywords: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
}

/// `[[page]]`: a page's size, orientation, and its content, in schema
/// order (text, then paragraph, then image, then link — TOML's separate
/// arrays-of-tables carry no cross-type ordering, so the schema's own
/// order is the one the manifest maps to).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPage {
    size: Option<String>,
    #[serde(default)]
    landscape: bool,
    #[serde(rename = "text", default)]
    texts: Vec<ManifestText>,
    #[serde(rename = "paragraph", default)]
    paragraphs: Vec<ManifestParagraph>,
    #[serde(rename = "image", default)]
    images: Vec<ManifestImage>,
    #[serde(rename = "link", default)]
    links: Vec<ManifestLink>,
}

/// `[[page.text]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestText {
    value: String,
    at: [f32; 2],
    font: Option<String>,
    size: Option<f32>,
}

/// `[[page.paragraph]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestParagraph {
    value: String,
    rect: [f32; 4],
    font: Option<String>,
    size: Option<f32>,
    leading: Option<f32>,
    align: Option<String>,
}

/// `[[page.image]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestImage {
    path: String,
    at: [f32; 2],
    width: Option<f32>,
    height: Option<f32>,
}

/// `[[page.link]]`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestLink {
    rect: [f32; 4],
    url: Option<String>,
    page: Option<usize>,
}

/// Builds a `Pdf` from the TOML manifest at `manifest_path`. Every error
/// this returns is prefixed with `manifest_path`, in the CLI's own
/// `"{path}: {cause}"` shape.
pub fn build(manifest_path: &Path) -> Result<Pdf, String> {
    build_inner(manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))
}

/// [`build`]'s body: every error here is a bare cause, prefixed with the
/// manifest path exactly once, at the single call site above.
fn build_inner(manifest_path: &Path) -> Result<Pdf, String> {
    let text = std::fs::read_to_string(manifest_path).map_err(|e| e.to_string())?;
    let manifest: Manifest = toml::from_str(&text).map_err(|e| e.to_string())?;
    let base_dir = manifest_path.parent().unwrap_or(Path::new("."));
    let metadata = manifest.meta.map(to_metadata);
    let mut pages = Vec::with_capacity(manifest.pages.len());
    for page in manifest.pages {
        pages.push(to_page(page, base_dir)?);
    }
    Ok(Pdf {
        metadata,
        pages,
        ..Pdf::default()
    })
}

/// `[meta]` -> `Metadata`: a direct field-for-field copy, every field
/// optional.
fn to_metadata(meta: ManifestMeta) -> Metadata {
    Metadata {
        title: meta.title,
        author: meta.author,
        subject: meta.subject,
        keywords: meta.keywords,
        creator: meta.creator,
        producer: meta.producer,
        ..Metadata::default()
    }
}

/// `[[page]]` -> `Page`: size (named or default A4, swapped under
/// `landscape`) plus content in schema order.
fn to_page(page: ManifestPage, base_dir: &Path) -> Result<Page, String> {
    let size = resolve_size(page.size.as_deref(), page.landscape)?;
    let mut content = Vec::new();
    for text in page.texts {
        content.push(Content::from(to_text(text)?));
    }
    for paragraph in page.paragraphs {
        content.push(Content::from(to_paragraph(paragraph)?));
    }
    for image in page.images {
        content.push(Content::from(to_image(image, base_dir)?));
    }
    for link in page.links {
        content.push(Content::from(to_link(link)?));
    }
    Ok(Page {
        size,
        content,
        ..Page::default()
    })
}

/// A page's `size`/`landscape` pair resolved to a `PageSize`: an absent
/// name defaults to A4, an unnamed size errors naming the size and the
/// valid list.
fn resolve_size(name: Option<&str>, landscape: bool) -> Result<PageSize, String> {
    let size = match name {
        None => PageSize::default(),
        Some(name) => PageSize::by_name(name).ok_or_else(|| {
            format!("unknown page size {name:?}: valid sizes are a3, a4, a5, letter, legal")
        })?,
    };
    if !landscape {
        return Ok(size);
    }
    Ok(size.landscape())
}

/// A font name resolved to a `Standard14`: absent defaults to Helvetica, an
/// unknown name errors naming the font and the valid list of PostScript
/// base names.
fn to_font(name: Option<&str>) -> Result<Standard14, String> {
    let Some(name) = name else {
        return Ok(Standard14::Helvetica);
    };
    Standard14::from_base_font(name).ok_or_else(|| {
        let valid: Vec<&str> = Standard14::ALL
            .iter()
            .map(|font| font.base_font())
            .collect();
        format!(
            "unknown font {name:?}: valid fonts are {}",
            valid.join(", ")
        )
    })
}

/// `[[page.text]]` -> `Text`.
fn to_text(item: ManifestText) -> Result<Text, String> {
    let font = to_font(item.font.as_deref())?;
    let mut text = Text {
        value: item.value,
        at: Point::new(item.at[0], item.at[1]),
        font,
        ..Text::default()
    };
    if let Some(size) = item.size {
        text.size = size;
    }
    Ok(text)
}

/// `align` resolved to a `ParagraphAlign`: absent defaults to left, an
/// unknown value errors naming the value and the valid list.
fn to_align(name: Option<&str>) -> Result<ParagraphAlign, String> {
    match name {
        None | Some("left") => Ok(ParagraphAlign::Left),
        Some("center") => Ok(ParagraphAlign::Center),
        Some("right") => Ok(ParagraphAlign::Right),
        Some("justify") => Ok(ParagraphAlign::Justify),
        Some(other) => Err(format!(
            "unknown paragraph align {other:?}: valid values are left, center, right, justify"
        )),
    }
}

/// `[[page.paragraph]]` -> `Paragraph`.
fn to_paragraph(item: ManifestParagraph) -> Result<Paragraph, String> {
    let font = to_font(item.font.as_deref())?;
    let align = to_align(item.align.as_deref())?;
    let mut paragraph = Paragraph {
        text: item.value,
        rect: item.rect,
        font,
        align,
        ..Paragraph::default()
    };
    if let Some(size) = item.size {
        paragraph.size = size;
    }
    if let Some(leading) = item.leading {
        paragraph.leading = Some(leading);
    }
    Ok(paragraph)
}

/// `[[page.image]]` -> `Image`: `path` resolved relative to the manifest's
/// directory and decoded by content (PNG or JPEG), never by extension.
fn to_image(item: ManifestImage, base_dir: &Path) -> Result<Image, String> {
    let path = base_dir.join(&item.path);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let data = ImageData::decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Image {
        data,
        at: Point::new(item.at[0], item.at[1]),
        width: item.width,
        height: item.height,
    })
}

/// `[[page.link]]` -> `Link`: exactly one of `url`/`page` must be given.
fn to_link(item: ManifestLink) -> Result<Link, String> {
    let target = match (item.url, item.page) {
        (Some(url), None) => LinkTarget::Uri(url),
        (None, Some(page)) => LinkTarget::Page(page),
        _ => return Err("link must have exactly one of url or page".to_string()),
    };
    Ok(Link {
        rect: item.rect,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q3_MANIFEST: &str = r#"
[meta]
title  = "Q3 Report"
author = "Mo"

[[page]]
size = "a4"

  [[page.text]]
  value = "Q3 Report"
  at    = [72, 770]
  font  = "Helvetica-Bold"
  size  = 28

  [[page.paragraph]]
  value   = "Body copy for the quarter."
  rect    = [72, 380, 523, 720]
  size    = 11
  leading = 15
  align   = "left"

  [[page.image]]
  path  = "chart.png"
  at    = [72, 96]
  width = 200

  [[page.link]]
  rect = [72, 88, 523, 380]
  url  = "https://example.com/q3"

[[page]]
"#;

    fn tiny_png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 3, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[10u8; 18]).unwrap();
        writer.finish().unwrap();
        bytes
    }

    fn scratch_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pdfboss-manifest-test-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn full_manifest_maps_element_counts_and_values() {
        let dir = scratch_dir("full");
        std::fs::write(dir.join("chart.png"), tiny_png_bytes()).unwrap();
        let manifest_path = dir.join("q3.toml");
        std::fs::write(&manifest_path, Q3_MANIFEST).unwrap();

        let pdf = build(&manifest_path).unwrap();
        assert_eq!(pdf.pages.len(), 2);
        let metadata = pdf.metadata.as_ref().unwrap();
        assert_eq!(metadata.title.as_deref(), Some("Q3 Report"));
        assert_eq!(metadata.author.as_deref(), Some("Mo"));

        let first = &pdf.pages[0];
        assert_eq!(first.size, PageSize::A4);
        assert_eq!(first.content.len(), 4);

        match &first.content[0] {
            Content::Text(text) => {
                assert_eq!(text.value, "Q3 Report");
                assert_eq!(text.at, Point::new(72.0, 770.0));
                assert_eq!(text.font, Standard14::HelveticaBold);
                assert_eq!(text.size, 28.0);
            }
            other => panic!("expected Text, got {other:?}"),
        }
        match &first.content[1] {
            Content::Paragraph(paragraph) => {
                assert_eq!(paragraph.text, "Body copy for the quarter.");
                assert_eq!(paragraph.rect, [72.0, 380.0, 523.0, 720.0]);
                assert_eq!(paragraph.size, 11.0);
                assert_eq!(paragraph.leading, Some(15.0));
                assert_eq!(paragraph.align, ParagraphAlign::Left);
            }
            other => panic!("expected Paragraph, got {other:?}"),
        }
        match &first.content[2] {
            Content::Image(image) => {
                assert_eq!(image.at, Point::new(72.0, 96.0));
                assert_eq!(image.width, Some(200.0));
                assert_eq!(image.height, None);
                assert_eq!((image.data.width(), image.data.height()), (3, 2));
            }
            other => panic!("expected Image, got {other:?}"),
        }
        match &first.content[3] {
            Content::Link(link) => {
                assert_eq!(link.rect, [72.0, 88.0, 523.0, 380.0]);
                assert_eq!(
                    link.target,
                    LinkTarget::Uri("https://example.com/q3".to_string())
                );
            }
            other => panic!("expected Link, got {other:?}"),
        }

        let second = &pdf.pages[1];
        assert_eq!(second.size, PageSize::A4);
        assert!(second.content.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_are_prefixed_with_the_manifest_path() {
        let dir = scratch_dir("missing");
        let manifest_path = dir.join("nope.toml");
        let err = build(&manifest_path).unwrap_err();
        assert!(
            err.starts_with(&manifest_path.display().to_string()),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = scratch_dir("unknown-key");
        let manifest_path = dir.join("bad.toml");
        std::fs::write(&manifest_path, "[meta]\ntitle = \"x\"\nbogus = 1\n").unwrap();
        let err = build(&manifest_path).unwrap_err();
        assert!(
            err.starts_with(&manifest_path.display().to_string()),
            "{err}"
        );
        assert!(err.contains("bogus"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_font_names_the_font_and_valid_list() {
        let dir = scratch_dir("unknown-font");
        let manifest_path = dir.join("bad-font.toml");
        std::fs::write(
            &manifest_path,
            "[[page]]\n  [[page.text]]\n  value = \"hi\"\n  at = [0, 0]\n  font = \"Arial\"\n",
        )
        .unwrap();
        let err = build(&manifest_path).unwrap_err();
        assert!(err.contains("Arial"), "{err}");
        assert!(err.contains("Helvetica"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn link_with_both_url_and_page_is_an_error() {
        let dir = scratch_dir("link-both");
        let manifest_path = dir.join("bad-link.toml");
        std::fs::write(
            &manifest_path,
            "[[page]]\n  [[page.link]]\n  rect = [0, 0, 1, 1]\n  url = \"https://x\"\n  page = 0\n",
        )
        .unwrap();
        let err = build(&manifest_path).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn link_with_neither_url_nor_page_is_an_error() {
        let dir = scratch_dir("link-neither");
        let manifest_path = dir.join("bad-link2.toml");
        std::fs::write(
            &manifest_path,
            "[[page]]\n  [[page.link]]\n  rect = [0, 0, 1, 1]\n",
        )
        .unwrap();
        let err = build(&manifest_path).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn per_page_landscape_swaps_dimensions() {
        let dir = scratch_dir("landscape");
        let manifest_path = dir.join("landscape.toml");
        std::fs::write(
            &manifest_path,
            "[[page]]\nsize = \"letter\"\nlandscape = true\n",
        )
        .unwrap();
        let pdf = build(&manifest_path).unwrap();
        assert_eq!(pdf.pages[0].size.dimensions(), (792.0, 612.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_size_defaults_to_a4() {
        let dir = scratch_dir("default-size");
        let manifest_path = dir.join("no-size.toml");
        std::fs::write(&manifest_path, "[[page]]\n").unwrap();
        let pdf = build(&manifest_path).unwrap();
        assert_eq!(pdf.pages[0].size, PageSize::A4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
