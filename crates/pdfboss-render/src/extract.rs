//! Embedded-image extraction: every image a page draws (image XObjects,
//! form recursion included), decoded at native size to RGBA pixmaps.

use std::sync::Arc;

use pdfboss_core::content::{parse_content, Op};
use pdfboss_core::{
    block_on, content_stream_data_with, page_content_with, AsyncObjectSource, Dict, Document,
    Immediate, Object, Page, Result, Stream,
};

use crate::color::IccCache;
use crate::executor::{image_alpha_mask, MAX_FORM_DEPTH};
use crate::image::{self, ImageMeta};
use crate::{Pixmap, RenderReport};

/// Decodes every image the page draws, at the image's own pixel
/// dimensions, in drawing order. An image drawn twice appears twice; an
/// XObject the content never draws does not appear at all. Stencil masks
/// (`/ImageMask true`) paint a fill color rather than carrying one of
/// their own, so they are not extracted. Optional-content visibility is
/// not consulted: an image inside a hidden `/OC` group is still embedded
/// in the file, so it still extracts. Extraction is lenient the way
/// rendering is: content that cannot be read or decoded contributes
/// nothing rather than failing the call.
pub fn extract_page_images(doc: &Document, page: &Page) -> Result<Vec<Pixmap>> {
    block_on(extract_page_images_with(Immediate(doc), page))
}

/// Extracts like [`extract_page_images`] against any object source; the
/// synchronous form is this implementation over `pdfboss_core::Immediate`.
pub async fn extract_page_images_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<Vec<Pixmap>> {
    let Ok(content) = page_content_with(&src, page).await else {
        return Ok(Vec::new());
    };
    let Ok(ops) = parse_content(&content) else {
        return Ok(Vec::new());
    };
    let chain: Vec<Arc<Dict>> = vec![Arc::new(page.resources.clone())];
    let icc = IccCache::default();
    let mut out = Vec::new();
    walk(&src, ops, chain, &icc, &mut out).await;
    Ok(out)
}

/// One operator list mid-walk: the page's, or a form's. The walk keeps
/// these on an explicit stack because form recursion is unbounded input
/// and the future must stay one flat state machine.
struct Level {
    ops: Vec<Op>,
    chain: Vec<Arc<Dict>>,
    depth: u32,
    next: usize,
}

/// Collects the images the operator list draws into `out`, following form
/// XObjects to [`MAX_FORM_DEPTH`] exactly as the executor does.
async fn walk<S: AsyncObjectSource>(
    src: &S,
    ops: Vec<Op>,
    chain: Vec<Arc<Dict>>,
    icc: &IccCache,
    out: &mut Vec<Pixmap>,
) {
    let mut stack = vec![Level {
        ops,
        chain,
        depth: 0,
        next: 0,
    }];
    while let Some(level) = stack.last_mut() {
        let Some(op) = level.ops.get(level.next) else {
            stack.pop();
            continue;
        };
        level.next += 1;
        let (name, depth, chain) = match op {
            Op::InlineImage(img) => {
                let stream = Stream {
                    dict: img.dict.clone(),
                    data: img.data.clone(),
                };
                let chain = level.chain.clone();
                collect_image(src, &stream, &chain, icc, out).await;
                continue;
            }
            Op::XObject(name) => (name.0.clone(), level.depth, level.chain.clone()),
            _ => continue,
        };
        let Some(Object::Stream(stream)) = find_res(src, &chain, "XObject", &name).await else {
            continue;
        };
        match xobject_subtype(src, &stream.dict).await.as_deref() {
            Some("Image") => collect_image(src, &stream, &chain, icc, out).await,
            Some("Form") if depth < MAX_FORM_DEPTH => {
                let Ok(data) = content_stream_data_with(src, &stream).await else {
                    continue;
                };
                let Ok(ops) = parse_content(&data) else {
                    continue;
                };
                let own_res = match stream.dict.get("Resources") {
                    Some(o) => match src.resolve(o).await {
                        Ok(Object::Dict(d)) => Some(d),
                        _ => None,
                    },
                    None => None,
                };
                let mut inner_chain = Vec::with_capacity(chain.len() + 1);
                if let Some(d) = own_res {
                    inner_chain.push(Arc::new(d));
                }
                inner_chain.extend_from_slice(&chain);
                stack.push(Level {
                    ops,
                    chain: inner_chain,
                    depth: depth + 1,
                    next: 0,
                });
            }
            _ => {}
        }
    }
}

/// Decodes one drawn image XObject at native size and appends it.
async fn collect_image<S: AsyncObjectSource>(
    src: &S,
    stream: &Stream,
    chain: &[Arc<Dict>],
    icc: &IccCache,
    out: &mut Vec<Pixmap>,
) {
    let Ok(data) = src.stream_data(stream).await else {
        return;
    };
    let cs_obj = image_colorspace(src, &stream.dict, chain).await;
    let meta = ImageMeta::read_with(src, &stream.dict, cs_obj.as_ref(), icc).await;
    if meta.stencil {
        return;
    }
    let mut report = RenderReport::default();
    let smask = image_alpha_mask(src, &stream.dict, &meta, &data, icc, &mut report).await;
    if let Some(pix) = image::decode_native(&meta, &data, smask.as_ref()) {
        out.push(pix);
    }
}

/// The XObject's `/Subtype` name, resolving an indirect value the way
/// drawing does (ISO 32000-1 7.3.8.1).
async fn xobject_subtype<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<String> {
    match dict.get("Subtype") {
        Some(Object::Name(n)) => Some(n.0.clone()),
        Some(indirect @ Object::Ref(_)) => match src.resolve(indirect).await {
            Ok(o) => o.as_name().map(|n| n.0.clone()),
            Err(_) => None,
        },
        _ => None,
    }
}

/// Looks up `/category/name` in the resource chain (innermost dict first),
/// resolving references at every step. Shared with the executor, whose
/// resource semantics extraction must match exactly.
pub(crate) async fn find_res<S: AsyncObjectSource>(
    src: &S,
    chain: &[Arc<Dict>],
    category: &str,
    name: &str,
) -> Option<Object> {
    for res in chain {
        let Some(cat) = res.get(category) else {
            continue;
        };
        let Ok(Object::Dict(dict)) = src.resolve(cat).await else {
            continue;
        };
        let Some(value) = dict.get(name) else {
            continue;
        };
        if let Ok(obj) = src.resolve(value).await {
            if !obj.is_null() {
                return Some(obj);
            }
        }
    }
    None
}

/// The image's `/ColorSpace` value with any resource-name indirection
/// resolved through the chain, shared with the executor so extraction and
/// drawing resolve color identically.
pub(crate) async fn image_colorspace<S: AsyncObjectSource>(
    src: &S,
    dict: &Dict,
    chain: &[Arc<Dict>],
) -> Option<Object> {
    let resolved = src.resolve(dict.get("ColorSpace")?).await.ok()?;
    if let Object::Name(n) = &resolved {
        let device = matches!(
            n.0.as_str(),
            "DeviceGray" | "DeviceRGB" | "DeviceCMYK" | "G" | "RGB" | "CMYK"
        );
        if !device {
            if let Some(from_res) = find_res(src, chain, "ColorSpace", &n.0).await {
                return Some(from_res);
            }
        }
    }
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_testkit::PdfBuilder;

    /// One page with the given `/Resources` body and raw content operators;
    /// `add` contributes the image objects (numbers 5 and up).
    fn image_doc(resources: &str, content: &str, add: impl FnOnce(&mut PdfBuilder)) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources {resources} /Contents 4 0 R >>"
            ),
        );
        b.stream(4, "", content.as_bytes());
        add(&mut b);
        b.build(1)
    }

    fn extract(bytes: Vec<u8>) -> Vec<Pixmap> {
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        extract_page_images(&doc, &page).expect("extract")
    }

    fn px(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * pix.width + x) * 4) as usize;
        pix.data[off..off + 4].try_into().expect("pixel")
    }

    const RGB_2X2: [u8; 12] = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0];
    const RGB_2X2_DICT: &str = "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                                /ColorSpace /DeviceRGB /BitsPerComponent 8";

    #[test]
    fn extracts_an_image_drawn_inside_a_form_xobject() {
        let bytes = image_doc("<< /XObject << /Fm1 5 0 R >> >>", "q /Fm1 Do Q", |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                     /Resources << /XObject << /Im1 6 0 R >> >>",
                b"q 50 0 0 50 0 0 cm /Im1 Do Q",
            );
            b.stream(6, RGB_2X2_DICT, &RGB_2X2);
        });
        let images = extract(bytes);
        assert_eq!(images.len(), 1, "the form's image is reached");
        assert_eq!((images[0].width, images[0].height), (2, 2));
    }

    #[test]
    fn a_form_drawing_itself_terminates() {
        let bytes = image_doc(
            "<< /XObject << /Fm1 5 0 R /Im1 6 0 R >> >>",
            "q /Fm1 Do Q /Im1 Do Q",
            |b| {
                // No own /Resources: the form sees the page's, so /Fm1
                // resolves back to itself.
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
                    b"q /Fm1 Do Q",
                );
                b.stream(6, RGB_2X2_DICT, &RGB_2X2);
            },
        );
        let images = extract(bytes);
        assert_eq!(images.len(), 1, "recursion bounded, page image still out");
    }

    #[test]
    fn an_image_drawn_twice_is_extracted_twice() {
        let bytes = image_doc(
            "<< /XObject << /Im1 5 0 R >> >>",
            "q 50 0 0 50 0 0 cm /Im1 Do Q q 50 0 0 50 50 50 cm /Im1 Do Q",
            |b| {
                b.stream(5, RGB_2X2_DICT, &RGB_2X2);
            },
        );
        assert_eq!(extract(bytes).len(), 2, "one entry per drawing");
    }

    #[test]
    fn an_undrawn_resource_is_not_extracted() {
        let bytes = image_doc("<< /XObject << /Im1 5 0 R >> >>", "q Q", |b| {
            b.stream(5, RGB_2X2_DICT, &RGB_2X2);
        });
        assert_eq!(extract(bytes).len(), 0, "nothing was drawn");
    }

    #[test]
    fn a_stencil_mask_is_not_extracted() {
        let bytes = image_doc(
            "<< /XObject << /Im1 5 0 R >> >>",
            "q 1 0 0 rg 50 0 0 50 10 10 cm /Im1 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                     /ImageMask true /BitsPerComponent 1",
                    &[0b10000000, 0b01000000],
                );
            },
        );
        assert_eq!(
            extract(bytes).len(),
            0,
            "stencils paint fill color, not pixels"
        );
    }

    #[test]
    fn extracts_an_inline_image() {
        let mut content = Vec::new();
        content.extend_from_slice(b"q 50 0 0 50 10 10 cm BI /W 2 /H 2 /CS /RGB /BPC 8 ID ");
        content.extend_from_slice(&RGB_2X2);
        content.extend_from_slice(b" EI Q");
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << >> /Contents 4 0 R >>",
        );
        b.stream(4, "", &content);
        let images = extract(b.build(1));
        assert_eq!(images.len(), 1, "the inline image is extracted");
        let img = &images[0];
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(px(img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(img, 1, 1), [255, 255, 0, 255]);
    }

    #[test]
    fn smask_becomes_the_extracted_alpha_channel() {
        let bytes = image_doc(
            "<< /XObject << /Im1 5 0 R >> >>",
            "q 50 0 0 50 10 10 cm /Im1 Do Q",
            |b| {
                b.stream(5, &format!("{RGB_2X2_DICT} /SMask 6 0 R"), &RGB_2X2);
                b.stream(
                    6,
                    "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                     /ColorSpace /DeviceGray /BitsPerComponent 8",
                    &[0, 85, 170, 255],
                );
            },
        );
        let images = extract(bytes);
        assert_eq!(images.len(), 1);
        let img = &images[0];
        assert_eq!(px(img, 0, 0), [255, 0, 0, 0], "fully masked out");
        assert_eq!(px(img, 1, 0), [0, 255, 0, 85]);
        assert_eq!(px(img, 0, 1), [0, 0, 255, 170]);
        assert_eq!(px(img, 1, 1), [255, 255, 0, 255], "fully opaque");
    }

    #[test]
    fn extracts_a_drawn_image_xobject_at_native_size() {
        let bytes = image_doc(
            "<< /XObject << /Im1 5 0 R >> >>",
            "q 50 0 0 50 10 10 cm /Im1 Do Q",
            |b| {
                b.stream(5, RGB_2X2_DICT, &RGB_2X2);
            },
        );
        let images = extract(bytes);
        assert_eq!(images.len(), 1, "one drawn image");
        let img = &images[0];
        assert_eq!((img.width, img.height), (2, 2), "native size");
        assert_eq!(px(img, 0, 0), [255, 0, 0, 255]);
        assert_eq!(px(img, 1, 0), [0, 255, 0, 255]);
        assert_eq!(px(img, 0, 1), [0, 0, 255, 255]);
        assert_eq!(px(img, 1, 1), [255, 255, 0, 255]);
    }
}
