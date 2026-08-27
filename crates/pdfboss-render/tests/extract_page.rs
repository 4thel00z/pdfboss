//! End-to-end coverage of embedded-image extraction over the passthrough
//! codec paths (JPEG 2000 here; the plain-sample paths are unit-tested in
//! the extract module). The fixture embeds one JP2 file in an image
//! XObject, so a decode that silently drops it is a count of zero, not an
//! error.

use pdfboss_core::Document;
use pdfboss_render::extract_page_images;

#[test]
fn extracts_the_jpx_image_at_native_size() {
    let bytes = include_bytes!("fixtures/pdf-rgb-53.pdf");
    let doc = Document::load(bytes.to_vec()).expect("the fixture PDF opens");
    let page = doc.page(0).expect("the fixture has one page");
    let images = extract_page_images(&doc, &page).expect("extract");
    assert_eq!(images.len(), 1, "the JPX image is extracted");
    let img = &images[0];
    assert_eq!((img.width, img.height), (130, 83), "native codestream size");
    let chromatic = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| {
            let (r, g, b) = (i32::from(px[0]), i32::from(px[1]), i32::from(px[2]));
            (r - g).abs() > 16 || (g - b).abs() > 16 || (r - b).abs() > 16
        })
        .count();
    assert!(
        chromatic > 100,
        "an RGB codestream decodes to colored pixels, got {chromatic}"
    );
}
