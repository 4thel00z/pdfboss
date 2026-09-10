//! Thumbnail images (ISO 32000-1 §12.3.4): a page's `/Thumb` image XObject,
//! read as a stream with the entries the clause makes significant.

use crate::document::Page;
use crate::object::{Object, Stream};
use crate::source::AsyncObjectSource;

/// A page's thumbnail image (ISO 32000-1 §12.3.4): the `/Thumb` image
/// XObject, with the entries the clause makes significant read out and the
/// rest of Table 89 left in the stream's dictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    /// `/Width` in samples.
    pub width: u32,
    /// `/Height` in samples.
    pub height: u32,
    /// `/BitsPerComponent`; `None` when absent.
    pub bits_per_component: Option<u32>,
    /// `/ColorSpace` as written: the clause allows DeviceGray, DeviceRGB or
    /// an Indexed space over one of them.
    pub color_space: Option<Object>,
    /// `/Decode`: the sample decode array as numbers, `None` when absent.
    pub decode: Option<Vec<f64>>,
    /// The image XObject itself: its dictionary and encoded data.
    pub stream: Stream,
}

/// The thumbnail of `page`: the image stream its `/Thumb` entry names,
/// `None` when absent or not a stream. Width and height are required by
/// Table 89; a stream lacking either is no thumbnail.
///
/// Covers ISO 32000-1 §12.3.4.
pub async fn thumbnail_with<S: AsyncObjectSource>(src: &S, page: &Page) -> Option<Thumbnail> {
    let Object::Stream(stream) = src.resolve(page.dict().get("Thumb")?).await.ok()? else {
        return None;
    };
    let dict = &stream.dict;
    let count = |key: &str| u32::try_from(dict.get_int(key)?).ok();
    Some(Thumbnail {
        width: count("Width")?,
        height: count("Height")?,
        bits_per_component: count("BitsPerComponent"),
        color_space: dict.get("ColorSpace").cloned(),
        decode: dict
            .get_array("Decode")
            .map(|values| values.iter().filter_map(Object::as_f64).collect()),
        stream,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::object::Name;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose page carries `page_extra`; object 7 is a
    /// two-pixel DeviceRGB image stream and object 8 an Indexed one.
    fn doc_with(page_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page_extra} >>"),
        );
        b.stream(
            7,
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Decode [0 1 0 1 0 1] /Interpolate true",
            &[255, 0, 0, 0, 0, 255],
        );
        b.stream(
            8,
            "/Width 1 /Height 1 /ColorSpace [/Indexed /DeviceRGB 0 <FF0000>] /BitsPerComponent 8",
            &[0],
        );
        Document::load(b.build(1)).unwrap()
    }

    /// The stream and the five significant entries are read; `/Interpolate`
    /// and the rest of Table 89 stay in the stream's dictionary only.
    // Covers ISO 32000-1 §12.3.4.
    #[test]
    fn reads_the_pages_thumbnail_and_its_significant_entries() {
        let doc = doc_with("/Thumb 7 0 R");
        let page = doc.page(0).unwrap();
        let thumbnail = doc.thumbnail(&page).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (2, 1));
        assert_eq!(thumbnail.bits_per_component, Some(8));
        assert_eq!(
            thumbnail.color_space,
            Some(Object::Name(Name("DeviceRGB".to_string())))
        );
        assert_eq!(thumbnail.decode, Some(vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0]));
        assert_eq!(thumbnail.stream.data, vec![255, 0, 0, 0, 0, 255]);
        assert!(thumbnail.stream.dict.get("Interpolate").is_some());
    }

    /// An Indexed colour space is kept as written and a missing `/Decode`
    /// is `None`; a page without `/Thumb`, or whose `/Thumb` is not a
    /// stream, has no thumbnail.
    // Covers ISO 32000-1 §12.3.4.
    #[test]
    fn indexed_thumbnails_keep_their_space_and_non_streams_are_none() {
        let doc = doc_with("/Thumb 8 0 R");
        let thumbnail = doc.thumbnail(&doc.page(0).unwrap()).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (1, 1));
        assert!(matches!(thumbnail.color_space, Some(Object::Array(_))));
        assert_eq!(thumbnail.decode, None);
        for page_extra in [
            "",
            "/Thumb 5",
            "/Thumb << /Width 1 /Height 1 >>",
            "/Thumb 3 0 R",
        ] {
            let doc = doc_with(page_extra);
            assert_eq!(doc.thumbnail(&doc.page(0).unwrap()), None, "{page_extra}");
        }
    }
}
