//! The compose element vocabulary: painted content one step above raw
//! canvas operators. A [`Content`] value sits in [`crate::pdf::Page::content`]
//! and lowers onto the page's [`Canvas`] at assemble time, after any
//! operators the caller already painted there directly — elements paint
//! over manual canvas work, never under it.

use pdfboss_core::Point;

use crate::canvas::Canvas;
use crate::color::Color;
use crate::error::Result;
use crate::font::Standard14;
use crate::image::ImageData;
use crate::pdf::{LinkAnnotation, LinkTarget};

/// Something that paints itself onto a [`Canvas`]. `Send` so a
/// [`Content::Custom`] value never blocks a `Pdf` from crossing a thread
/// boundary.
pub trait Draw: Send {
    /// Paints this value's content onto `canvas`.
    fn draw(&self, canvas: &mut Canvas) -> Result<()>;
}

/// One line of text at a fixed baseline origin.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    /// The characters to show.
    pub value: String,
    /// Baseline origin, in page user-space units.
    pub at: Point,
    /// Face to draw in.
    pub font: Standard14,
    /// Font size, in points.
    pub size: f32,
    /// Fill color.
    pub color: Color,
}

impl Default for Text {
    fn default() -> Text {
        Text {
            value: String::new(),
            at: Point::default(),
            font: Standard14::Helvetica,
            size: 12.0,
            color: Color::BLACK,
        }
    }
}

impl Draw for Text {
    fn draw(&self, canvas: &mut Canvas) -> Result<()> {
        canvas.set_fill(self.color);
        canvas.text(&self.value, self.at.x, self.at.y, self.font, self.size)
    }
}

/// A raster image placed at a point.
///
/// No `Default`: there is no meaningful default for `data`.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    /// The pixels to embed.
    pub data: ImageData,
    /// Placement origin (the box's bottom-left corner), in page user-space
    /// units.
    pub at: Point,
    /// Explicit width, in points. `None` derives it from `height` (by
    /// aspect) or, with `height` also `None`, from the image's pixel width
    /// at 72 dpi.
    pub width: Option<f32>,
    /// Explicit height, in points. `None` derives it from `width` (by
    /// aspect) or, with `width` also `None`, from the image's pixel height
    /// at 72 dpi.
    pub height: Option<f32>,
}

impl Image {
    /// The box this image paints into: the natural size at 72 dpi when
    /// neither dimension is given, the other dimension scaled by aspect
    /// when one is given, or the exact box when both are given.
    pub fn placed_size(&self) -> (f32, f32) {
        let natural_width = self.data.width() as f32;
        let natural_height = self.data.height() as f32;
        match (self.width, self.height) {
            (Some(width), Some(height)) => (width, height),
            (Some(width), None) => (width, width * natural_height / natural_width),
            (None, Some(height)) => (height * natural_width / natural_height, height),
            (None, None) => (natural_width, natural_height),
        }
    }
}

impl Draw for Image {
    fn draw(&self, canvas: &mut Canvas) -> Result<()> {
        let (width, height) = self.placed_size();
        let handle = canvas.add_image(self.data.clone());
        canvas.draw_image(handle, self.at.x, self.at.y, width, height);
        Ok(())
    }
}

/// A clickable rectangle, lowered into a [`LinkAnnotation`] on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// The clickable area, `[x0, y0, x1, y1]` in the page's user space.
    pub rect: [f32; 4],
    /// Where the link goes.
    pub target: LinkTarget,
}

/// One piece of composed page content.
pub enum Content {
    /// A line of text.
    Text(Text),
    /// A placed image.
    Image(Image),
    /// A clickable link area.
    Link(Link),
    /// Anything implementing [`Draw`].
    Custom(Box<dyn Draw>),
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Content::Image(image) => f.debug_tuple("Image").field(image).finish(),
            Content::Link(link) => f.debug_tuple("Link").field(link).finish(),
            Content::Custom(..) => f.write_str("Custom(..)"),
        }
    }
}

impl From<Text> for Content {
    fn from(value: Text) -> Content {
        Content::Text(value)
    }
}

impl From<Image> for Content {
    fn from(value: Image) -> Content {
        Content::Image(value)
    }
}

impl From<Link> for Content {
    fn from(value: Link) -> Content {
        Content::Link(value)
    }
}

impl Content {
    /// Wraps any [`Draw`] value as custom content.
    pub fn custom(value: impl Draw + 'static) -> Content {
        Content::Custom(Box::new(value))
    }
}

/// Paints `content` onto `canvas`, in order, after any operators already
/// there; link elements are appended to `links` instead of painted.
pub(crate) fn lower(
    content: Vec<Content>,
    canvas: &mut Canvas,
    links: &mut Vec<LinkAnnotation>,
) -> Result<()> {
    for item in content {
        match item {
            Content::Text(text) => text.draw(canvas)?,
            Content::Image(image) => image.draw(canvas)?,
            Content::Link(link) => links.push(LinkAnnotation {
                rect: link.rect,
                target: link.target,
            }),
            Content::Custom(drawable) => drawable.draw(canvas)?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pdfboss_core::content::{parse_content, Op};
    use pdfboss_core::{Document, Name};
    use pdfboss_output::extract_text;

    use super::*;
    use crate::pdf::{Page, PageSize};
    use crate::Pdf;

    #[test]
    fn elements_lower_in_sequence_order_after_canvas_ops() {
        let mut page = Page::new(PageSize::A4);
        page.canvas
            .text("under", 72.0, 100.0, Standard14::Helvetica, 10.0)
            .unwrap();
        page.content.push(Content::from(Text {
            value: "over".into(),
            at: Point::new(72.0, 700.0),
            size: 24.0,
            ..Text::default()
        }));
        let ops_before = page.canvas.ops().len();
        let bytes = Pdf {
            pages: vec![page],
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap();
        let doc = Document::load(bytes).unwrap();
        let loaded = doc.page(0).unwrap();
        let text = extract_text(&doc, &loaded).unwrap();
        assert!(text.contains("under") && text.contains("over"));
        assert!(ops_before > 0);

        let stream = loaded.content(&doc).unwrap();
        let ops = parse_content(&stream).unwrap();
        let index_of = |needle: &[u8]| {
            ops.iter()
                .position(|op| matches!(op, Op::ShowText(s) if s == needle))
                .unwrap_or_else(|| {
                    panic!(
                        "no ShowText carrying {:?} in {:?}",
                        String::from_utf8_lossy(needle),
                        ops
                    )
                })
        };
        let under_index = index_of(b"under");
        let over_index = index_of(b"over");
        assert!(
            under_index < over_index,
            "expected \"under\" ({under_index}) before \"over\" ({over_index})"
        );
    }

    #[test]
    fn custom_draw_paints_through_the_canvas() {
        struct Letterhead;
        impl Draw for Letterhead {
            fn draw(&self, canvas: &mut Canvas) -> Result<()> {
                canvas.set_line_width(0.5);
                canvas.move_to(72.0, 806.0);
                canvas.line_to(523.0, 806.0);
                canvas.stroke();
                canvas.text("ACME GmbH", 72.0, 812.0, Standard14::Helvetica, 8.0)
            }
        }
        let custom = Content::custom(Letterhead);
        assert_eq!(format!("{custom:?}"), "Custom(..)");
        let mut page = Page::new(PageSize::A4);
        page.content.push(custom);
        let bytes = Pdf {
            pages: vec![page],
            ..Pdf::default()
        }
        .to_bytes()
        .unwrap();
        let doc = Document::load(bytes).unwrap();
        let loaded = doc.page(0).unwrap();
        let text = extract_text(&doc, &loaded).unwrap();
        assert!(text.contains("ACME GmbH"));
    }

    #[test]
    fn image_placed_size_is_natural_at_72dpi_when_both_none() {
        let image = Image {
            data: ImageData::gray8(16, 8, vec![0u8; 128]).unwrap(),
            at: Point::new(10.0, 10.0),
            width: None,
            height: None,
        };
        assert_eq!(image.placed_size(), (16.0, 8.0));
    }

    #[test]
    fn image_element_scales_by_aspect_when_one_dimension_is_given() {
        let image = Image {
            data: ImageData::gray8(16, 8, vec![0u8; 128]).unwrap(),
            at: Point::new(10.0, 10.0),
            width: Some(32.0),
            height: None,
        };
        assert_eq!(image.placed_size(), (32.0, 16.0));
    }

    #[test]
    fn image_placed_size_scales_by_aspect_when_only_height_is_given() {
        let image = Image {
            data: ImageData::gray8(16, 8, vec![0u8; 128]).unwrap(),
            at: Point::new(10.0, 10.0),
            width: None,
            height: Some(4.0),
        };
        assert_eq!(image.placed_size(), (8.0, 4.0));
    }

    #[test]
    fn image_placed_size_is_exact_when_both_dimensions_are_given() {
        let image = Image {
            data: ImageData::gray8(16, 8, vec![0u8; 128]).unwrap(),
            at: Point::new(10.0, 10.0),
            width: Some(50.0),
            height: Some(90.0),
        };
        assert_eq!(image.placed_size(), (50.0, 90.0));
    }

    #[test]
    fn text_draw_sets_fill_then_shows_text() {
        let mut canvas = Canvas::new();
        let text = Text {
            value: "hi".into(),
            at: Point::new(1.0, 2.0),
            font: Standard14::Helvetica,
            size: 10.0,
            color: Color::Rgb(1.0, 0.0, 0.0),
        };
        text.draw(&mut canvas).unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::SetFillRGB(1.0, 0.0, 0.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(1.0, 2.0),
                Op::ShowText(b"hi".to_vec()),
                Op::EndText,
            ]
        );
    }

    #[test]
    fn image_draw_uses_placed_size() {
        use pdfboss_core::Matrix;

        let mut canvas = Canvas::new();
        let image = Image {
            data: ImageData::gray8(2, 2, vec![0u8; 4]).unwrap(),
            at: Point::new(5.0, 6.0),
            width: Some(40.0),
            height: None,
        };
        image.draw(&mut canvas).unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::Save,
                Op::Concat(Matrix {
                    a: 40.0,
                    b: 0.0,
                    c: 0.0,
                    d: 40.0,
                    e: 5.0,
                    f: 6.0,
                }),
                Op::XObject(Name("Im1".into())),
                Op::Restore,
            ]
        );
    }
}
