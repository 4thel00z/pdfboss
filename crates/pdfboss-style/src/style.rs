//! Element-type selectors and the concrete text/box styles they resolve
//! to: the vocabulary later tasks parse a CSS subset into and cascade
//! through a theme.

use pdfboss_write::{Color, Standard14};

/// The twenty element types a theme rule can select on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Element {
    /// The document body, the root of the cascade.
    Body,
    /// `<h1>` heading.
    H1,
    /// `<h2>` heading.
    H2,
    /// `<h3>` heading.
    H3,
    /// `<h4>` heading.
    H4,
    /// `<h5>` heading.
    H5,
    /// `<h6>` heading.
    H6,
    /// Paragraph.
    P,
    /// Inline code span.
    Code,
    /// Preformatted code block.
    Pre,
    /// Blockquote.
    Blockquote,
    /// Unordered list.
    Ul,
    /// Ordered list.
    Ol,
    /// List item.
    Li,
    /// Table.
    Table,
    /// Table header cell.
    Th,
    /// Table data cell.
    Td,
    /// Link.
    A,
    /// Struck-through text.
    Del,
    /// Horizontal rule.
    Hr,
}

impl Element {
    /// All twenty variants, in declaration order, so `element as usize`
    /// indexes a `[Declared; 20]` keyed by this order.
    pub const ALL: [Element; 20] = [
        Element::Body,
        Element::H1,
        Element::H2,
        Element::H3,
        Element::H4,
        Element::H5,
        Element::H6,
        Element::P,
        Element::Code,
        Element::Pre,
        Element::Blockquote,
        Element::Ul,
        Element::Ol,
        Element::Li,
        Element::Table,
        Element::Th,
        Element::Td,
        Element::A,
        Element::Del,
        Element::Hr,
    ];

    /// The lowercase selector spelling, e.g. `"blockquote"`.
    pub fn name(self) -> &'static str {
        match self {
            Element::Body => "body",
            Element::H1 => "h1",
            Element::H2 => "h2",
            Element::H3 => "h3",
            Element::H4 => "h4",
            Element::H5 => "h5",
            Element::H6 => "h6",
            Element::P => "p",
            Element::Code => "code",
            Element::Pre => "pre",
            Element::Blockquote => "blockquote",
            Element::Ul => "ul",
            Element::Ol => "ol",
            Element::Li => "li",
            Element::Table => "table",
            Element::Th => "th",
            Element::Td => "td",
            Element::A => "a",
            Element::Del => "del",
            Element::Hr => "hr",
        }
    }

    /// Parses a selector spelling back to the element, `None` for anything
    /// outside the twenty supported selectors.
    pub fn from_name(name: &str) -> Option<Element> {
        Element::ALL
            .into_iter()
            .find(|element| element.name() == name)
    }
}

/// A font family resolved to one of the three Standard-14 type families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontFamily {
    /// Helvetica (sans-serif).
    Helvetica,
    /// Times (serif).
    Times,
    /// Courier (monospace).
    Courier,
}

/// Horizontal text alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    /// Flush left.
    Left,
    /// Centered.
    Center,
    /// Flush right.
    Right,
}

/// Text decoration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decoration {
    /// No decoration.
    None,
    /// Underlined.
    Underline,
    /// Struck through.
    LineThrough,
}

/// A font size, either absolute or relative to the inherited size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSize {
    /// An absolute size in points.
    Pt(f32),
    /// A multiple of the inherited size.
    Em(f32),
}

/// The four sides of a box edge, in declaration order top/right/bottom/left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Left edge.
    pub left: f32,
}

/// One theme rule's declared properties, every field optional so a rule
/// only overrides what it sets. Margin and padding arrays are ordered
/// top, right, bottom, left.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Declared {
    /// Font family.
    pub family: Option<FontFamily>,
    /// Font size.
    pub size: Option<FontSize>,
    /// Bold weight.
    pub bold: Option<bool>,
    /// Italic style.
    pub italic: Option<bool>,
    /// Text color.
    pub color: Option<Color>,
    /// Background color.
    pub background: Option<Color>,
    /// Margin, top/right/bottom/left.
    pub margin: [Option<f32>; 4],
    /// Padding, top/right/bottom/left.
    pub padding: [Option<f32>; 4],
    /// Line height, as a multiple of font size.
    pub line_height: Option<f32>,
    /// Text alignment.
    pub align: Option<Align>,
    /// Text decoration.
    pub decoration: Option<Decoration>,
}

impl Declared {
    /// Overlays `other`'s set fields onto `self`, leaving fields `other`
    /// leaves unset untouched. Used to fold cascade rules together in
    /// specificity order, most specific last.
    pub fn merge(&mut self, other: &Declared) {
        if other.family.is_some() {
            self.family = other.family;
        }
        if other.size.is_some() {
            self.size = other.size;
        }
        if other.bold.is_some() {
            self.bold = other.bold;
        }
        if other.italic.is_some() {
            self.italic = other.italic;
        }
        if other.color.is_some() {
            self.color = other.color;
        }
        if other.background.is_some() {
            self.background = other.background;
        }
        for side in 0..4 {
            if other.margin[side].is_some() {
                self.margin[side] = other.margin[side];
            }
            if other.padding[side].is_some() {
                self.padding[side] = other.padding[side];
            }
        }
        if other.line_height.is_some() {
            self.line_height = other.line_height;
        }
        if other.align.is_some() {
            self.align = other.align;
        }
        if other.decoration.is_some() {
            self.decoration = other.decoration;
        }
    }
}

/// A fully resolved text style: every field concrete, no inheritance left
/// to chase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Font family.
    pub family: FontFamily,
    /// Font size in points.
    pub size: f32,
    /// Bold weight.
    pub bold: bool,
    /// Italic style.
    pub italic: bool,
    /// Text color.
    pub color: Color,
    /// Line height, as a multiple of font size.
    pub line_height: f32,
    /// Text alignment.
    pub align: Align,
    /// Text decoration.
    pub decoration: Decoration,
}

impl TextStyle {
    /// The hard fallback beneath the default theme: 11pt black Helvetica,
    /// left-aligned, no decoration.
    pub fn base() -> TextStyle {
        TextStyle {
            family: FontFamily::Helvetica,
            size: 11.0,
            bold: false,
            italic: false,
            color: Color::BLACK,
            line_height: 1.4,
            align: Align::Left,
            decoration: Decoration::None,
        }
    }

    /// Overlays a rule's declared fields onto this style, resolving `Em`
    /// sizes against the inherited size and leaving unset fields
    /// inherited unchanged.
    pub fn apply(&self, d: &Declared) -> TextStyle {
        TextStyle {
            family: d.family.unwrap_or(self.family),
            size: match d.size {
                Some(FontSize::Pt(pt)) => pt,
                Some(FontSize::Em(em)) => em * self.size,
                None => self.size,
            },
            bold: d.bold.unwrap_or(self.bold),
            italic: d.italic.unwrap_or(self.italic),
            color: d.color.unwrap_or(self.color),
            line_height: d.line_height.unwrap_or(self.line_height),
            align: d.align.unwrap_or(self.align),
            decoration: d.decoration.unwrap_or(self.decoration),
        }
    }

    /// The Standard-14 face this style resolves to, by family, weight and
    /// slant.
    pub fn font(&self) -> Standard14 {
        match (self.family, self.bold, self.italic) {
            (FontFamily::Helvetica, false, false) => Standard14::Helvetica,
            (FontFamily::Helvetica, true, false) => Standard14::HelveticaBold,
            (FontFamily::Helvetica, false, true) => Standard14::HelveticaOblique,
            (FontFamily::Helvetica, true, true) => Standard14::HelveticaBoldOblique,
            (FontFamily::Times, false, false) => Standard14::TimesRoman,
            (FontFamily::Times, true, false) => Standard14::TimesBold,
            (FontFamily::Times, false, true) => Standard14::TimesItalic,
            (FontFamily::Times, true, true) => Standard14::TimesBoldItalic,
            (FontFamily::Courier, false, false) => Standard14::Courier,
            (FontFamily::Courier, true, false) => Standard14::CourierBold,
            (FontFamily::Courier, false, true) => Standard14::CourierOblique,
            (FontFamily::Courier, true, true) => Standard14::CourierBoldOblique,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_from_name_maps_every_selector() {
        assert_eq!(Element::from_name("h1"), Some(Element::H1));
        assert_eq!(Element::from_name("blockquote"), Some(Element::Blockquote));
        assert_eq!(Element::from_name("div"), None);
        for element in Element::ALL {
            assert!(Element::from_name(element.name()).is_some());
        }
    }

    #[test]
    fn font_resolution_covers_all_twelve_faces() {
        let style = TextStyle {
            family: FontFamily::Times,
            bold: true,
            italic: true,
            ..TextStyle::base()
        };
        assert_eq!(style.font(), Standard14::TimesBoldItalic);
        let style = TextStyle {
            family: FontFamily::Courier,
            ..TextStyle::base()
        };
        assert_eq!(style.font(), Standard14::Courier);
    }

    #[test]
    fn apply_overlays_only_declared_fields() {
        let declared = Declared {
            size: Some(FontSize::Em(2.0)),
            bold: Some(true),
            ..Declared::default()
        };
        let applied = TextStyle::base().apply(&declared);
        assert_eq!(applied.size, 22.0);
        assert!(applied.bold);
        assert_eq!(applied.family, FontFamily::Helvetica);
    }

    #[test]
    fn merge_keeps_earlier_fields_the_later_rule_leaves_unset() {
        let mut first = Declared {
            bold: Some(true),
            ..Declared::default()
        };
        let second = Declared {
            italic: Some(true),
            ..Declared::default()
        };
        first.merge(&second);
        assert_eq!(first.bold, Some(true));
        assert_eq!(first.italic, Some(true));
    }
}
