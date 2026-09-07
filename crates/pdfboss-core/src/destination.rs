//! Explicit destinations (ISO 32000-1 §12.3.2.2): the page a link, outline
//! item or action shows, and how that page is fitted in the window.

use crate::object::{ObjRef, Object};
use crate::source::AsyncObjectSource;

/// The page a destination shows.
///
/// Covers ISO 32000-1 §12.3.2.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DestinationPage {
    /// A reference to a page object of this document.
    Object(ObjRef),
    /// A 0-based page number, as a remote go-to action's destination gives
    /// it, the page being in another document (§12.6.4.3).
    Number(u32),
}

/// How the page is shown, one row of Table 151. A `None` coordinate or
/// zoom keeps the viewer's current value, as a null in the array asks; a
/// zoom of 0 means the same.
///
/// Covers ISO 32000-1 §12.3.2.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    /// `/XYZ left top zoom`: the point at the window's upper-left corner
    /// and the magnification.
    Xyz {
        left: Option<f32>,
        top: Option<f32>,
        zoom: Option<f32>,
    },
    /// `/Fit`: the whole page in the window.
    Fit,
    /// `/FitH top`: the page's width across the window, `top` at its top.
    FitH { top: Option<f32> },
    /// `/FitV left`: the page's height down the window, `left` at its left.
    FitV { left: Option<f32> },
    /// `/FitR left bottom right top`: the rectangle in the window.
    FitR {
        left: f32,
        bottom: f32,
        right: f32,
        top: f32,
    },
    /// `/FitB`: the page's bounding box in the window.
    FitB,
    /// `/FitBH top`: the bounding box's width across the window.
    FitBH { top: Option<f32> },
    /// `/FitBV left`: the bounding box's height down the window.
    FitBV { left: Option<f32> },
}

/// One explicit destination: a page and how to show it.
///
/// Covers ISO 32000-1 §12.3.2.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Destination {
    pub page: DestinationPage,
    pub fit: Fit,
}

impl Destination {
    /// Parses a Table 151 array whose elements are direct objects. The
    /// page is a reference or a non-negative integer; a coordinate the
    /// array leaves out reads as null; `/FitR` needs all four numbers. Any
    /// other form is `None`.
    ///
    /// Covers ISO 32000-1 §12.3.2.2.
    pub fn parse(array: &[Object]) -> Option<Destination> {
        let page = match array.first()? {
            Object::Ref(r) => DestinationPage::Object(*r),
            Object::Int(n) => DestinationPage::Number(u32::try_from(*n).ok()?),
            _ => return None,
        };
        let fit = array.get(1)?.as_name()?.0.as_str();
        let optional = |i: usize| -> Option<Option<f32>> {
            match array.get(i) {
                None | Some(Object::Null) => Some(None),
                Some(o) => Some(Some(o.as_f64()? as f32)),
            }
        };
        let required = |i: usize| -> Option<f32> { Some(array.get(i)?.as_f64()? as f32) };
        let fit = match fit {
            "XYZ" => Fit::Xyz {
                left: optional(2)?,
                top: optional(3)?,
                zoom: optional(4)?.filter(|z| *z != 0.0),
            },
            "Fit" => Fit::Fit,
            "FitH" => Fit::FitH { top: optional(2)? },
            "FitV" => Fit::FitV { left: optional(2)? },
            "FitR" => Fit::FitR {
                left: required(2)?,
                bottom: required(3)?,
                right: required(4)?,
                top: required(5)?,
            },
            "FitB" => Fit::FitB,
            "FitBH" => Fit::FitBH { top: optional(2)? },
            "FitBV" => Fit::FitBV { left: optional(2)? },
            _ => return None,
        };
        Some(Destination { page, fit })
    }
}

/// Resolves `object` to a destination array and parses it, resolving
/// indirect numbers on the way; the page reference itself stays a
/// reference.
///
/// Covers ISO 32000-1 §12.3.2.2.
pub async fn destination_with<S: AsyncObjectSource>(
    src: &S,
    object: &Object,
) -> Option<Destination> {
    let Ok(Object::Array(items)) = src.resolve(object).await else {
        return None;
    };
    let mut direct = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        // The page reference stays a reference; every later element is a
        // name, a number or null, so resolving it loses nothing.
        let resolved = match item {
            Object::Ref(_) if i > 0 => src.resolve(item).await.ok()?,
            other => other.clone(),
        };
        direct.push(resolved);
    }
    Destination::parse(&direct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;
    use crate::{block_on, Document, Immediate};
    use pdfboss_testkit::PdfBuilder;

    fn page() -> Object {
        Object::Ref(ObjRef { num: 3, gen: 0 })
    }

    fn name(n: &str) -> Object {
        Object::Name(Name(n.into()))
    }

    fn parse(items: Vec<Object>) -> Option<Destination> {
        Destination::parse(&items)
    }

    // Covers ISO 32000-1 §12.3.2.2.
    #[test]
    fn every_table_151_form_parses() {
        let page_ref = DestinationPage::Object(ObjRef { num: 3, gen: 0 });
        let cases = [
            (
                vec![
                    page(),
                    name("XYZ"),
                    Object::Int(10),
                    Object::Real(700.5),
                    Object::Real(1.5),
                ],
                Fit::Xyz {
                    left: Some(10.0),
                    top: Some(700.5),
                    zoom: Some(1.5),
                },
            ),
            (vec![page(), name("Fit")], Fit::Fit),
            (
                vec![page(), name("FitH"), Object::Int(700)],
                Fit::FitH { top: Some(700.0) },
            ),
            (
                vec![page(), name("FitV"), Object::Int(72)],
                Fit::FitV { left: Some(72.0) },
            ),
            (
                vec![
                    page(),
                    name("FitR"),
                    Object::Int(10),
                    Object::Int(20),
                    Object::Int(300),
                    Object::Int(400),
                ],
                Fit::FitR {
                    left: 10.0,
                    bottom: 20.0,
                    right: 300.0,
                    top: 400.0,
                },
            ),
            (vec![page(), name("FitB")], Fit::FitB),
            (
                vec![page(), name("FitBH"), Object::Int(650)],
                Fit::FitBH { top: Some(650.0) },
            ),
            (
                vec![page(), name("FitBV"), Object::Int(36)],
                Fit::FitBV { left: Some(36.0) },
            ),
        ];
        for (array, fit) in cases {
            assert_eq!(
                parse(array.clone()),
                Some(Destination {
                    page: page_ref,
                    fit
                }),
                "{array:?}"
            );
        }
    }

    // Covers ISO 32000-1 §12.3.2.2.
    #[test]
    fn null_zero_zoom_and_missing_parameters_keep_the_current_value() {
        let xyz = |left, top, zoom| {
            Some(Destination {
                page: DestinationPage::Object(ObjRef { num: 3, gen: 0 }),
                fit: Fit::Xyz { left, top, zoom },
            })
        };
        assert_eq!(
            parse(vec![
                page(),
                name("XYZ"),
                Object::Null,
                Object::Null,
                Object::Null
            ]),
            xyz(None, None, None)
        );
        assert_eq!(
            parse(vec![
                page(),
                name("XYZ"),
                Object::Int(0),
                Object::Int(0),
                Object::Int(0)
            ]),
            xyz(Some(0.0), Some(0.0), None),
            "zoom 0 is null, a coordinate 0 is not"
        );
        // Files leave trailing parameters out; they read as null.
        assert_eq!(
            parse(vec![page(), name("XYZ"), Object::Int(5)]),
            xyz(Some(5.0), None, None)
        );
        assert_eq!(
            parse(vec![page(), name("FitH")]),
            Some(Destination {
                page: DestinationPage::Object(ObjRef { num: 3, gen: 0 }),
                fit: Fit::FitH { top: None },
            })
        );
    }

    // Covers ISO 32000-1 §12.3.2.2 and §12.6.4.3.
    #[test]
    fn remote_destinations_carry_a_page_number() {
        assert_eq!(
            parse(vec![Object::Int(4), name("Fit")]),
            Some(Destination {
                page: DestinationPage::Number(4),
                fit: Fit::Fit,
            })
        );
        assert_eq!(parse(vec![Object::Int(-1), name("Fit")]), None);
    }

    // Covers ISO 32000-1 §12.3.2.2.
    #[test]
    fn malformed_destinations_are_rejected() {
        assert_eq!(parse(vec![]), None, "empty");
        assert_eq!(parse(vec![page()]), None, "no fit");
        assert_eq!(parse(vec![page(), name("FitAll")]), None, "unknown fit");
        assert_eq!(
            parse(vec![name("Fit"), page()]),
            None,
            "page and fit swapped"
        );
        assert_eq!(
            parse(vec![Object::String(b"3".to_vec()), name("Fit")]),
            None,
            "page as a string"
        );
        assert_eq!(
            parse(vec![
                page(),
                name("FitR"),
                Object::Int(1),
                Object::Int(2),
                Object::Int(3)
            ]),
            None,
            "FitR with three numbers"
        );
        assert_eq!(
            parse(vec![
                page(),
                name("XYZ"),
                name("left"),
                Object::Int(1),
                Object::Int(1)
            ]),
            None,
            "a coordinate that is not a number or null"
        );
    }

    // Covers ISO 32000-1 §12.3.2.2.
    #[test]
    fn indirect_arrays_and_numbers_resolve() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.object(10, "[3 0 R /XYZ 11 0 R null 12 0 R]");
        b.object(11, "42");
        b.object(12, "2.0");
        b.object(13, "(not an array)");
        let doc = Document::load(b.build(1)).unwrap();
        let indirect = Object::Ref(ObjRef { num: 10, gen: 0 });
        assert_eq!(
            block_on(destination_with(&Immediate(&doc), &indirect)),
            Some(Destination {
                page: DestinationPage::Object(ObjRef { num: 3, gen: 0 }),
                fit: Fit::Xyz {
                    left: Some(42.0),
                    top: None,
                    zoom: Some(2.0),
                },
            })
        );
        let direct = Object::Array(vec![page(), name("Fit")]);
        assert_eq!(
            block_on(destination_with(&Immediate(&doc), &direct)).map(|d| d.fit),
            Some(Fit::Fit)
        );
        let string = Object::Ref(ObjRef { num: 13, gen: 0 });
        assert_eq!(block_on(destination_with(&Immediate(&doc), &string)), None);
    }
}
