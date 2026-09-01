//! The compose element vocabulary: painted content one step above raw
//! canvas operators. A [`Content`] value sits in [`crate::pdf::Page::content`]
//! and lowers onto the page's [`Canvas`] at assemble time, after any
//! operators the caller already painted there directly — elements paint
//! over manual canvas work, never under it.

use pdfboss_core::content::Op;
use pdfboss_core::Point;

use crate::canvas::Canvas;
use crate::color::Color;
use crate::error::{Error, Result};
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

/// Horizontal alignment for a [`Paragraph`].
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ParagraphAlign {
    /// Flush to the rect's left edge.
    #[default]
    Left,
    /// Centered within the rect width.
    Center,
    /// Flush to the rect's right edge.
    Right,
    /// Flush to both edges: word spacing stretches every line but the
    /// last.
    Justify,
}

/// A block of text wrapped, aligned, and (for [`ParagraphAlign::Justify`])
/// stretched to fill a rectangle.
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    /// The text to lay out. `\n` forces a line break; other whitespace runs
    /// between words collapse to a single space.
    pub text: String,
    /// The box to wrap into, `[x0, y0, x1, y1]` in page user-space units.
    pub rect: [f32; 4],
    /// Face to draw in.
    pub font: Standard14,
    /// Font size, in points.
    pub size: f32,
    /// Line-to-line advance, in points. `None` defaults to `1.2 * size`.
    pub leading: Option<f32>,
    /// Horizontal alignment within the rect.
    pub align: ParagraphAlign,
    /// Fill color.
    pub color: Color,
}

impl Default for Paragraph {
    fn default() -> Paragraph {
        Paragraph {
            text: String::new(),
            rect: [0.0, 0.0, 0.0, 0.0],
            font: Standard14::Helvetica,
            size: 11.0,
            leading: None,
            align: ParagraphAlign::Left,
            color: Color::BLACK,
        }
    }
}

/// Greedily wraps `text` to `max_width`: `\n` forces a break, other
/// whitespace runs between words collapse to single spaces, and a blank
/// source line yields an empty line so its vertical advance survives. A
/// word wider than `max_width` on its own still gets a line — no
/// hyphenation.
fn wrap_lines(text: &str, font: Standard14, size: f32, max_width: f32) -> Result<Vec<Vec<&str>>> {
    let mut lines = Vec::new();
    for source_line in text.split('\n') {
        let words: Vec<&str> = source_line.split_whitespace().collect();
        if words.is_empty() {
            lines.push(Vec::new());
            continue;
        }
        let mut current: Vec<&str> = Vec::new();
        let mut current_text = String::new();
        for word in words {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current_text} {word}")
            };
            let width = font.text_width(&candidate, size)?;
            if current.is_empty() || width <= max_width {
                current.push(word);
                current_text = candidate;
                continue;
            }
            lines.push(std::mem::take(&mut current));
            current_text = word.to_string();
            current.push(word);
        }
        lines.push(current);
    }
    Ok(lines)
}

/// The number of lines whose baseline fits between `y0` and the first
/// baseline at `y1 - size`, stepping down by `leading` per further line. A
/// small epsilon absorbs floating-point rounding at exact boundaries.
fn lines_that_fit(y0: f32, y1: f32, size: f32, leading: f32) -> usize {
    const EPSILON: f32 = 1e-3;
    let first_baseline = y1 - size;
    if first_baseline < y0 - EPSILON {
        return 0;
    }
    (((first_baseline - y0) / leading) + EPSILON).floor() as usize + 1
}

/// `Tw` (word spacing) is persistent text state that `BeginText` does not
/// clear, so a stretch an earlier line left active must be reset before the
/// first following line that does not itself stretch — not only once, at
/// the very end.
impl Draw for Paragraph {
    fn draw(&self, canvas: &mut Canvas) -> Result<()> {
        canvas.set_fill(self.color);
        let [x0, y0, x1, y1] = self.rect;
        let width = x1 - x0;
        let leading = self.leading.unwrap_or(1.2 * self.size);
        let lines = wrap_lines(&self.text, self.font, self.size, width)?;
        let fits = lines_that_fit(y0, y1, self.size, leading);
        if lines.len() > fits {
            return Err(Error::Other(format!(
                "paragraph overflows its rect: {fits} lines fit, {} needed",
                lines.len()
            )));
        }
        let last_visible = lines.iter().rposition(|words| !words.is_empty());
        let mut stretch_active = false;
        for (index, words) in lines.iter().enumerate() {
            if words.is_empty() {
                continue;
            }
            let baseline = y1 - self.size - index as f32 * leading;
            let line_text = words.join(" ");
            let line_width = self.font.text_width(&line_text, self.size)?;
            let is_final = Some(index) == last_visible;
            let stretch = match self.align {
                ParagraphAlign::Justify if !is_final && words.len() >= 2 => {
                    Some((width - line_width) / (words.len() as f32 - 1.0))
                }
                _ => None,
            };
            let x = match self.align {
                ParagraphAlign::Right => x1 - line_width,
                ParagraphAlign::Center => x0 + (width - line_width) / 2.0,
                ParagraphAlign::Left | ParagraphAlign::Justify => x0,
            };
            if stretch.is_none() && stretch_active {
                canvas.op(Op::SetWordSpacing(0.0));
                stretch_active = false;
            }
            if let Some(spacing) = stretch {
                canvas.op(Op::SetWordSpacing(spacing));
                stretch_active = true;
            }
            canvas.text(&line_text, x, baseline, self.font, self.size)?;
        }
        if stretch_active {
            canvas.op(Op::SetWordSpacing(0.0));
        }
        Ok(())
    }
}

/// One piece of composed page content.
pub enum Content {
    /// A line of text.
    Text(Text),
    /// A placed image.
    Image(Image),
    /// A clickable link area.
    Link(Link),
    /// A wrapped, aligned block of text.
    Paragraph(Paragraph),
    /// Anything implementing [`Draw`].
    Custom(Box<dyn Draw>),
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Content::Image(image) => f.debug_tuple("Image").field(image).finish(),
            Content::Link(link) => f.debug_tuple("Link").field(link).finish(),
            Content::Paragraph(paragraph) => f.debug_tuple("Paragraph").field(paragraph).finish(),
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

impl From<Paragraph> for Content {
    fn from(value: Paragraph) -> Content {
        Content::Paragraph(value)
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
            Content::Paragraph(paragraph) => paragraph.draw(canvas)?,
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
    fn paragraph_default_is_helvetica_11_left_and_none_leading() {
        let paragraph = Paragraph::default();
        assert_eq!(paragraph.text, "");
        assert_eq!(paragraph.font, Standard14::Helvetica);
        assert_eq!(paragraph.size, 11.0);
        assert_eq!(paragraph.leading, None);
        assert_eq!(paragraph.align, ParagraphAlign::Left);
    }

    #[test]
    fn content_from_paragraph_debug_prints_paragraph() {
        let content = Content::from(Paragraph::default());
        assert_eq!(
            format!("{content:?}"),
            format!("Paragraph({:?})", Paragraph::default())
        );
    }

    #[test]
    fn paragraph_sets_its_own_fill() {
        let mut canvas = Canvas::new();
        let paragraph = Paragraph {
            text: "hi".into(),
            rect: [0.0, 0.0, 100.0, 100.0],
            font: Standard14::Helvetica,
            size: 10.0,
            color: Color::Rgb(0.0, 0.5, 0.0),
            ..Paragraph::default()
        };
        paragraph.draw(&mut canvas).unwrap();
        assert!(matches!(canvas.ops()[0], Op::SetFillRGB(0.0, 0.5, 0.0)));
    }

    #[test]
    fn paragraph_wraps_at_word_boundaries_courier_metrics() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "aaaaaaaaa bbbbbbbbbb cccccccccc".into(),
            rect: [0.0, 0.0, 120.0, 100.0],
            font: Standard14::Courier,
            size: 10.0,
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::SetFillGray(0.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 90.0),
                Op::ShowText(b"aaaaaaaaa bbbbbbbbbb".to_vec()),
                Op::EndText,
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 78.0),
                Op::ShowText(b"cccccccccc".to_vec()),
                Op::EndText,
            ]
        );
    }

    #[test]
    fn paragraph_overflow_reports_lines_fit_and_needed() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "aaaaaaaaa bbbbbbbbbb cccccccccc".into(),
            rect: [0.0, 80.0, 120.0, 95.0],
            font: Standard14::Courier,
            size: 10.0,
            ..Paragraph::default()
        };
        let err = lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap_err();
        match err {
            Error::Other(msg) => {
                assert_eq!(msg, "paragraph overflows its rect: 1 lines fit, 2 needed")
            }
            other => panic!("expected Error::Other, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_justify_stretches_non_final_lines_and_resets_once() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "aaaaaaa bbbbbbb ddddd".into(),
            rect: [0.0, 0.0, 120.0, 100.0],
            font: Standard14::Courier,
            size: 10.0,
            align: ParagraphAlign::Justify,
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::SetFillGray(0.0),
                Op::SetWordSpacing(30.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 90.0),
                Op::ShowText(b"aaaaaaa bbbbbbb".to_vec()),
                Op::EndText,
                Op::SetWordSpacing(0.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 78.0),
                Op::ShowText(b"ddddd".to_vec()),
                Op::EndText,
            ],
            "the reset must land before the final line's BeginText, not after it \
             (Tw is persistent text state that BeginText does not clear)"
        );

        let bytes = crate::content::serialize_ops(canvas.ops());
        let parsed = parse_content(&bytes).unwrap();
        let stretched: Vec<f32> = parsed
            .iter()
            .filter_map(|op| match op {
                Op::SetWordSpacing(value) if *value > 0.0 => Some(*value),
                _ => None,
            })
            .collect();
        assert_eq!(stretched, [30.0]);
        assert!(parsed.contains(&Op::SetWordSpacing(0.0)));
    }

    #[test]
    fn paragraph_justify_final_line_with_multiple_words_gets_no_leftover_spacing() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "aaaaaaa bbbbbbb ccccc dd".into(),
            rect: [0.0, 0.0, 120.0, 100.0],
            font: Standard14::Courier,
            size: 10.0,
            align: ParagraphAlign::Justify,
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        let expected = [
            Op::SetFillGray(0.0),
            Op::SetWordSpacing(30.0),
            Op::BeginText,
            Op::SetFont(Name("F1".into()), 10.0),
            Op::TextMove(0.0, 90.0),
            Op::ShowText(b"aaaaaaa bbbbbbb".to_vec()),
            Op::EndText,
            Op::SetWordSpacing(0.0),
            Op::BeginText,
            Op::SetFont(Name("F1".into()), 10.0),
            Op::TextMove(0.0, 78.0),
            Op::ShowText(b"ccccc dd".to_vec()),
            Op::EndText,
        ];
        assert_eq!(
            canvas.ops(),
            expected,
            "the final line has 2 words (a space glyph) — a leftover non-zero \
             Tw here would visibly over-stretch it, which is exactly the bug"
        );

        let bytes = crate::content::serialize_ops(canvas.ops());
        let parsed = parse_content(&bytes).unwrap();
        let reset_index = parsed
            .iter()
            .position(|op| *op == Op::SetWordSpacing(0.0))
            .expect("a zero reset must be present");
        let final_show_index = parsed
            .iter()
            .position(|op| matches!(op, Op::ShowText(s) if s == b"ccccc dd"))
            .expect("final line's ShowText must be present");
        assert!(
            reset_index < final_show_index,
            "reset (index {reset_index}) must precede the final line's ShowText \
             (index {final_show_index}): {parsed:?}"
        );
        let stray_nonzero_between = parsed[reset_index..final_show_index]
            .iter()
            .any(|op| matches!(op, Op::SetWordSpacing(value) if *value != 0.0));
        assert!(
            !stray_nonzero_between,
            "no non-zero word spacing may sit between the reset and the final \
             line's ShowText: {:?}",
            &parsed[reset_index..final_show_index]
        );
    }

    #[test]
    fn paragraph_trailing_blank_line_does_not_become_the_justify_final_line() {
        let text = "aaaaaaa bbbbbbb ccccc dd\n";

        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: text.into(),
            rect: [0.0, 0.0, 120.0, 100.0],
            font: Standard14::Courier,
            size: 10.0,
            align: ParagraphAlign::Justify,
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::SetFillGray(0.0),
                Op::SetWordSpacing(30.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 90.0),
                Op::ShowText(b"aaaaaaa bbbbbbb".to_vec()),
                Op::EndText,
                Op::SetWordSpacing(0.0),
                Op::BeginText,
                Op::SetFont(Name("F1".into()), 10.0),
                Op::TextMove(0.0, 78.0),
                Op::ShowText(b"ccccc dd".to_vec()),
                Op::EndText,
            ],
            "the trailing blank line (from the trailing \\n) must not steal the \
             'final line never stretches' exemption from \"ccccc dd\""
        );

        let tight_rect_without_trailing_newline = Paragraph {
            text: "aaaaaaa bbbbbbb ccccc dd".into(),
            rect: [0.0, 0.0, 120.0, 24.0],
            font: Standard14::Courier,
            size: 10.0,
            align: ParagraphAlign::Justify,
            ..Paragraph::default()
        };
        let mut fits_canvas = Canvas::new();
        lower(
            vec![tight_rect_without_trailing_newline.into()],
            &mut fits_canvas,
            &mut links,
        )
        .expect("two visible lines fit in a rect sized for exactly two lines");

        let tight_rect_with_trailing_newline = Paragraph {
            text: text.into(),
            rect: [0.0, 0.0, 120.0, 24.0],
            font: Standard14::Courier,
            size: 10.0,
            align: ParagraphAlign::Justify,
            ..Paragraph::default()
        };
        let mut overflow_canvas = Canvas::new();
        let err = lower(
            vec![tight_rect_with_trailing_newline.into()],
            &mut overflow_canvas,
            &mut links,
        )
        .unwrap_err();
        match err {
            Error::Other(msg) => assert_eq!(
                msg, "paragraph overflows its rect: 2 lines fit, 3 needed",
                "the trailing blank line must still count toward vertical advance"
            ),
            other => panic!("expected Error::Other, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_center_and_right_align_offset_by_rect_width() {
        let mut links = Vec::new();
        let base = Paragraph {
            text: "aaaaaaaaa".into(),
            rect: [0.0, 0.0, 120.0, 50.0],
            font: Standard14::Courier,
            size: 10.0,
            ..Paragraph::default()
        };

        let mut center_canvas = Canvas::new();
        lower(
            vec![Paragraph {
                align: ParagraphAlign::Center,
                ..base.clone()
            }
            .into()],
            &mut center_canvas,
            &mut links,
        )
        .unwrap();
        assert_eq!(center_canvas.ops()[3], Op::TextMove(33.0, 40.0));

        let mut right_canvas = Canvas::new();
        lower(
            vec![Paragraph {
                align: ParagraphAlign::Right,
                ..base
            }
            .into()],
            &mut right_canvas,
            &mut links,
        )
        .unwrap();
        assert_eq!(right_canvas.ops()[3], Op::TextMove(66.0, 40.0));
    }

    #[test]
    fn paragraph_blank_line_keeps_advance_without_drawing() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "a\n\nb".into(),
            rect: [0.0, 0.0, 100.0, 100.0],
            font: Standard14::Helvetica,
            size: 10.0,
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        let moves: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::TextMove(..)))
            .collect();
        assert_eq!(
            moves,
            [&Op::TextMove(0.0, 90.0), &Op::TextMove(0.0, 66.0)],
            "blank line should still consume a leading slot"
        );
        let shows: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::ShowText(..)))
            .collect();
        assert_eq!(
            shows,
            [&Op::ShowText(b"a".to_vec()), &Op::ShowText(b"b".to_vec()),]
        );
    }

    #[test]
    fn paragraph_leading_override_changes_line_advance() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "aaaaaaaaaa bbbbbbbbbb".into(),
            rect: [0.0, 0.0, 60.0, 100.0],
            font: Standard14::Courier,
            size: 10.0,
            leading: Some(20.0),
            ..Paragraph::default()
        };
        lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap();
        let moves: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::TextMove(..)))
            .collect();
        assert_eq!(moves, [&Op::TextMove(0.0, 90.0), &Op::TextMove(0.0, 70.0)]);
    }

    #[test]
    fn paragraph_propagates_unencodable_character_error_untouched() {
        let mut canvas = Canvas::new();
        let mut links = Vec::new();
        let paragraph = Paragraph {
            text: "\u{2318}".into(),
            rect: [0.0, 0.0, 100.0, 100.0],
            font: Standard14::Helvetica,
            size: 10.0,
            ..Paragraph::default()
        };
        let err = lower(vec![paragraph.into()], &mut canvas, &mut links).unwrap_err();
        assert!(matches!(
            err,
            Error::Unencodable {
                ch: '\u{2318}',
                font: "Helvetica"
            }
        ));
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
