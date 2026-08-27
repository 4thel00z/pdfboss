//! Theme: cascade of CSS rules into concrete styles by element type.

use pdfboss_write::Color;

use crate::parse::{parse_sheet, StyleError};
use crate::style::{Declared, Edges, Element, TextStyle};

pub(crate) const DEFAULT_CSS: &str = "\
body { font-family: helvetica; font-size: 11pt; line-height: 1.4; color: #000; margin: 72pt; text-align: left; }\n\
h1 { font-size: 2em; font-weight: bold; margin-top: 18pt; margin-bottom: 9pt; }\n\
h2 { font-size: 1.6em; font-weight: bold; margin-top: 14pt; margin-bottom: 7pt; }\n\
h3 { font-size: 1.3em; font-weight: bold; margin-top: 12pt; margin-bottom: 6pt; }\n\
h4 { font-size: 1.15em; font-weight: bold; margin-top: 11pt; margin-bottom: 6pt; }\n\
h5 { font-size: 1em; font-weight: bold; margin-top: 11pt; margin-bottom: 6pt; }\n\
h6 { font-size: 0.9em; font-weight: bold; margin-top: 11pt; margin-bottom: 6pt; }\n\
p { margin-bottom: 8pt; }\n\
code { font-family: courier; background-color: #f0f0f0; }\n\
pre { font-family: courier; font-size: 0.9em; background-color: #f0f0f0; margin-top: 8pt; margin-bottom: 8pt; padding: 8pt; }\n\
blockquote { margin-left: 24pt; margin-top: 8pt; margin-bottom: 8pt; color: #555; font-style: italic; }\n\
ul { margin-top: 4pt; margin-bottom: 8pt; }\n\
ol { margin-top: 4pt; margin-bottom: 8pt; }\n\
li { margin-bottom: 2pt; }\n\
table { margin-top: 8pt; margin-bottom: 8pt; }\n\
th { font-weight: bold; background-color: #e8e8e8; padding: 4pt; }\n\
td { padding: 4pt; }\n\
a { color: #0645ad; text-decoration: underline; }\n\
del { text-decoration: line-through; }\n\
hr { margin-top: 12pt; margin-bottom: 12pt; color: #999; }\n";

/// A theme: cascade of CSS rules resolved into concrete styles by element.
#[derive(Debug)]
pub struct Theme {
    decls: Vec<Declared>,
}

impl Theme {
    /// The default theme: built-in CSS styles.
    pub fn default_theme() -> Theme {
        let rules = parse_sheet(DEFAULT_CSS).expect("the built-in default theme parses");
        Theme::from_rules(Theme::empty(), rules)
    }

    /// Parse a user stylesheet and overlay it onto the default theme.
    /// Later rules override earlier rules for the same element.
    pub fn parse(css: &str) -> Result<Theme, StyleError> {
        Ok(Theme::from_rules(Theme::default_theme(), parse_sheet(css)?))
    }

    /// Create an empty theme with no declarations.
    fn empty() -> Theme {
        Theme {
            decls: vec![Declared::default(); Element::ALL.len()],
        }
    }

    /// Fold rules into a theme, with later rules overriding earlier ones.
    fn from_rules(mut theme: Theme, rules: Vec<crate::parse::Rule>) -> Theme {
        for rule in rules {
            for element in &rule.elements {
                theme.decls[*element as usize].merge(&rule.declared);
            }
        }
        theme
    }

    /// The declared properties for an element, as merged from all matching
    /// rules.
    pub fn declared(&self, e: Element) -> &Declared {
        &self.decls[e as usize]
    }

    /// The base text style for the theme: hard fallback overlaid by body
    /// element declarations.
    pub fn base(&self) -> TextStyle {
        TextStyle::base().apply(self.declared(Element::Body))
    }

    /// The margin edges for an element. Unset sides default to 0.0.
    pub fn margin(&self, e: Element) -> Edges {
        let d = self.declared(e);
        Edges {
            top: d.margin[0].unwrap_or(0.0),
            right: d.margin[1].unwrap_or(0.0),
            bottom: d.margin[2].unwrap_or(0.0),
            left: d.margin[3].unwrap_or(0.0),
        }
    }

    /// The padding edges for an element. Unset sides default to 0.0.
    pub fn padding(&self, e: Element) -> Edges {
        let d = self.declared(e);
        Edges {
            top: d.padding[0].unwrap_or(0.0),
            right: d.padding[1].unwrap_or(0.0),
            bottom: d.padding[2].unwrap_or(0.0),
            left: d.padding[3].unwrap_or(0.0),
        }
    }

    /// The background color for an element, if set.
    pub fn background(&self, e: Element) -> Option<Color> {
        self.declared(e).background
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_parses_and_covers_body() {
        let theme = Theme::default_theme();
        let base = theme.base();
        assert_eq!(base.size, 11.0);
        assert_eq!(base.family, crate::style::FontFamily::Helvetica);
        assert_eq!(theme.margin(Element::Body).left, 72.0);
    }

    #[test]
    fn user_theme_overlays_defaults() {
        let theme = Theme::parse("h1 { color: #c00; }").unwrap();
        let h1 = theme.base().apply(theme.declared(Element::H1));
        assert_eq!(h1.color, Color::Rgb(0.8, 0.0, 0.0));
        assert!(
            h1.bold,
            "default h1 bold survives an overlay that only sets color"
        );
        assert_eq!(h1.size, 22.0, "default h1 2em of body 11pt survives");
    }

    #[test]
    fn later_rule_wins() {
        let theme = Theme::parse("p { color: #111; }\np { color: #222; }").unwrap();
        let p = theme.base().apply(theme.declared(Element::P));
        let expected = 0x22 as f32 / 255.0;
        assert_eq!(p.color, Color::Rgb(expected, expected, expected));
    }

    #[test]
    fn parse_error_location_passes_through() {
        let e = Theme::parse("h1 { font-size: 12px; }").unwrap_err();
        assert_eq!(e.line, 1);
    }
}
