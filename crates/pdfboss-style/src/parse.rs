//! Strict CSS-subset parser: element-type selectors and the fixed
//! property set from `style.rs`, driven directly off `cssparser` tokens
//! so every error carries a 1-indexed source location.
//!
//! cssparser's own errors carry neither a location nor the offending
//! token, so every failure is raised here as a `Failure` at the location
//! read from the parser just before the failing token was consumed.

use std::fmt;

use cssparser::{ParseError, ParseErrorKind, Parser, SourceLocation, Token};
use pdfboss_write::Color;

use crate::style::{Align, Declared, Decoration, Element, FontFamily, FontSize};

/// A located parse failure. `line` and `column` are 1-indexed.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleError {
    /// Line the failing token starts on, counted from 1.
    pub line: u32,
    /// Column the failing token starts on, counted from 1.
    pub column: u32,
    /// What went wrong, naming the offending selector, property or value.
    pub message: String,
}

impl fmt::Display for StyleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}:{}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for StyleError {}

/// One `selector, selector { declarations }` rule.
#[derive(Clone, Debug)]
pub(crate) struct Rule {
    pub elements: Vec<Element>,
    pub declared: Declared,
}

/// A failure at the location of the token that caused it.
#[derive(Clone, Debug, PartialEq)]
struct Failure {
    location: SourceLocation,
    message: String,
}

impl Failure {
    fn new(location: SourceLocation, message: impl Into<String>) -> Self {
        Failure {
            location,
            message: message.into(),
        }
    }
}

/// Parses a stylesheet into its rules, in source order. Comments and
/// whitespace are ignored; anything outside the supported selector,
/// property, value and unit vocabulary is a located error.
pub(crate) fn parse_sheet(css: &str) -> Result<Vec<Rule>, StyleError> {
    let mut parser = Parser::new(css);
    let mut rules = Vec::new();
    loop {
        match next_rule(&mut parser) {
            Ok(Some(rule)) => rules.push(rule),
            Ok(None) => return Ok(rules),
            Err(e) => return Err(style_error(e)),
        }
    }
}

fn style_error(e: Failure) -> StyleError {
    StyleError {
        line: e.location.line + 1,
        column: e.location.column,
        message: e.message,
    }
}

/// Where the next token starts, past any whitespace and comments.
fn token_location(parser: &mut Parser<'_>) -> SourceLocation {
    parser.skip_whitespace();
    parser.current_source_location()
}

/// The next token together with the location it starts at. Running out
/// of input is a failure at that location.
fn next_token<'i>(parser: &mut Parser<'i>) -> Result<(SourceLocation, Token<'i>), Failure> {
    let location = token_location(parser);
    let token = parser
        .next()
        .map_err(|e| Failure::new(location, e.kind.to_string()))?
        .clone();
    Ok((location, token))
}

/// Runs `parse` over the block or function whose opening token was just
/// consumed. cssparser reports leftover input and the nesting limit
/// without a location, so those are placed at `location`, the block's
/// opening token.
fn nested<'i, T>(
    parser: &mut Parser<'i>,
    location: SourceLocation,
    parse: impl FnOnce(&mut Parser<'i>) -> Result<T, Failure>,
) -> Result<T, Failure> {
    parser
        .parse_nested_block(|block| parse(block).map_err(ParseError::custom))
        .map_err(|e| match e.kind {
            ParseErrorKind::Custom(failure) => failure,
            ParseErrorKind::Basic(kind) => Failure::new(location, kind.to_string()),
        })
}

fn next_rule(parser: &mut Parser<'_>) -> Result<Option<Rule>, Failure> {
    let mut elements = Vec::new();
    loop {
        let location = token_location(parser);
        let token = match parser.next() {
            Ok(token) => token.clone(),
            Err(_) => {
                if elements.is_empty() {
                    return Ok(None);
                }
                return Err(Failure::new(location, "selector list without a { } block"));
            }
        };
        match token {
            Token::Ident(name) => {
                let element = Element::from_name(&name.to_ascii_lowercase()).ok_or_else(|| {
                    Failure::new(
                        location,
                        format!(
                            "unsupported selector {name:?}: only element type selectors are supported"
                        ),
                    )
                })?;
                elements.push(element);
            }
            Token::Comma => {}
            Token::CurlyBracketBlock => {
                if elements.is_empty() {
                    return Err(Failure::new(location, "rule has no selector"));
                }
                let declared = nested(parser, location, declarations)?;
                return Ok(Some(Rule { elements, declared }));
            }
            other => {
                return Err(Failure::new(
                    location,
                    format!(
                        "unsupported selector token {other:?}: only element type selectors are supported"
                    ),
                ));
            }
        }
    }
}

fn declarations(parser: &mut Parser<'_>) -> Result<Declared, Failure> {
    let mut declared = Declared::default();
    loop {
        let location = token_location(parser);
        let token = match parser.next() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(declared),
        };
        let name = match token {
            Token::Semicolon => continue,
            Token::Ident(name) => name.to_ascii_lowercase(),
            other => {
                return Err(Failure::new(
                    location,
                    format!("expected a property name, found {other:?}"),
                ))
            }
        };
        let colon = token_location(parser);
        parser
            .expect_colon()
            .map_err(|_| Failure::new(colon, format!("expected ':' after property {name:?}")))?;
        declaration(parser, &name, &mut declared, location)?;
    }
}

fn declaration(
    parser: &mut Parser<'_>,
    name: &str,
    declared: &mut Declared,
    location: SourceLocation,
) -> Result<(), Failure> {
    match name {
        "font-family" => declared.family = Some(font_family(parser)?),
        "font-size" => declared.size = Some(font_size(parser)?),
        "font-weight" => declared.bold = Some(font_weight(parser)?),
        "font-style" => declared.italic = Some(font_style(parser)?),
        "color" => declared.color = Some(color(parser)?),
        "background-color" => declared.background = Some(color(parser)?),
        "margin" => declared.margin = edges(parser)?,
        "padding" => declared.padding = edges(parser)?,
        "margin-top" => declared.margin[0] = Some(length(parser)?),
        "margin-right" => declared.margin[1] = Some(length(parser)?),
        "margin-bottom" => declared.margin[2] = Some(length(parser)?),
        "margin-left" => declared.margin[3] = Some(length(parser)?),
        "padding-top" => declared.padding[0] = Some(length(parser)?),
        "padding-right" => declared.padding[1] = Some(length(parser)?),
        "padding-bottom" => declared.padding[2] = Some(length(parser)?),
        "padding-left" => declared.padding[3] = Some(length(parser)?),
        "line-height" => declared.line_height = Some(line_height(parser)?),
        "text-align" => declared.align = Some(text_align(parser)?),
        "text-decoration" => declared.decoration = Some(text_decoration(parser)?),
        other => {
            return Err(Failure::new(
                location,
                format!("unsupported property {other:?}"),
            ))
        }
    }
    finish(parser)
}

fn finish(parser: &mut Parser<'_>) -> Result<(), Failure> {
    let location = token_location(parser);
    match parser.next() {
        Err(_) => Ok(()),
        Ok(&Token::Semicolon) => Ok(()),
        Ok(other) => Err(Failure::new(
            location,
            format!("unexpected {} after the value", render_token(other)),
        )),
    }
}

/// Renders a token the way it appeared in the source, for use in error
/// messages that must name the offending value.
fn render_token(token: &Token) -> String {
    match token {
        Token::Ident(name) | Token::AtKeyword(name) => name.to_string(),
        Token::Hash(name) | Token::IDHash(name) => format!("#{name}"),
        Token::QuotedString(value) => format!("\"{value}\""),
        Token::UnquotedUrl(value) => format!("url({value})"),
        Token::Delim(delim) => delim.to_string(),
        Token::Number { value, .. } => value.to_string(),
        Token::Percentage { unit_value, .. } => format!("{}%", unit_value * 100.0),
        Token::Dimension { value, unit, .. } => format!("{value}{unit}"),
        Token::Function(name) => format!("{name}("),
        Token::Colon => ":".to_string(),
        Token::Semicolon => ";".to_string(),
        Token::Comma => ",".to_string(),
        other => format!("{other:?}"),
    }
}

fn length(parser: &mut Parser<'_>) -> Result<f32, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("pt") => Ok(*value),
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("mm") => {
            Ok(72.0 / 25.4 * value)
        }
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("cm") => {
            Ok(72.0 / 2.54 * value)
        }
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("in") => Ok(72.0 * value),
        Token::Number { value, .. } if *value == 0.0 => Ok(0.0),
        _ => Err(Failure::new(
            location,
            format!(
                "length takes pt, mm, cm or in, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn font_size(parser: &mut Parser<'_>) -> Result<FontSize, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("pt") => {
            Ok(FontSize::Pt(*value))
        }
        Token::Dimension { value, unit, .. } if unit.eq_ignore_ascii_case("em") => {
            Ok(FontSize::Em(*value))
        }
        _ => Err(Failure::new(
            location,
            format!("font-size takes pt or em, found {}", render_token(&token)),
        )),
    }
}

fn edges(parser: &mut Parser<'_>) -> Result<[Option<f32>; 4], Failure> {
    let mut values: Vec<f32> = Vec::new();
    while values.len() < 4 {
        match parser.try_parse(length) {
            Ok(value) => values.push(value),
            Err(_) => break,
        }
    }
    let edges = match values.as_slice() {
        [a] => [*a, *a, *a, *a],
        [v, h] => [*v, *h, *v, *h],
        [t, h, b] => [*t, *h, *b, *h],
        [t, r, b, l] => [*t, *r, *b, *l],
        _ => return Err(shorthand_error(parser)),
    };
    Ok(edges.map(Some))
}

fn shorthand_error(parser: &mut Parser<'_>) -> Failure {
    let location = token_location(parser);
    match parser.next() {
        Ok(token) => Failure::new(
            location,
            format!(
                "margin/padding shorthand takes 1 to 4 lengths, found {}",
                render_token(token)
            ),
        ),
        Err(_) => Failure::new(
            location,
            "margin/padding shorthand takes 1 to 4 lengths, found nothing",
        ),
    }
}

fn color(parser: &mut Parser<'_>) -> Result<Color, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Hash(hex) | Token::IDHash(hex) => hex_color(location, hex),
        Token::Function(name) if name.eq_ignore_ascii_case("rgb") => {
            nested(parser, location, rgb_components)
        }
        Token::Ident(name) => named_color(location, name),
        _ => Err(Failure::new(
            location,
            format!(
                "color takes a hex value, rgb() or a named color, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn hex_color(location: SourceLocation, hex: &str) -> Result<Color, Failure> {
    let expanded = match hex.len() {
        3 => hex.chars().flat_map(|digit| [digit, digit]).collect(),
        6 => hex.to_string(),
        _ => {
            return Err(Failure::new(
                location,
                format!("hex colors take 3 or 6 digits, found #{hex}"),
            ))
        }
    };
    let channel = |slice: &str| -> Result<f32, Failure> {
        u8::from_str_radix(slice, 16)
            .map(|byte| byte as f32 / 255.0)
            .map_err(|_| Failure::new(location, format!("invalid hex color, found #{hex}")))
    };
    Ok(Color::Rgb(
        channel(&expanded[0..2])?,
        channel(&expanded[2..4])?,
        channel(&expanded[4..6])?,
    ))
}

fn named_color(location: SourceLocation, name: &str) -> Result<Color, Failure> {
    let rgb = match name.to_ascii_lowercase().as_str() {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "navy" => (0, 0, 128),
        "teal" => (0, 128, 128),
        "purple" => (128, 0, 128),
        "orange" => (255, 165, 0),
        "yellow" => (255, 255, 0),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "maroon" => (128, 0, 0),
        "aqua" | "cyan" => (0, 255, 255),
        "fuchsia" | "magenta" => (255, 0, 255),
        "lime" => (0, 255, 0),
        "olive" => (128, 128, 0),
        _ => return Err(Failure::new(location, format!("unknown color name {name}"))),
    };
    let (r, g, b) = rgb;
    Ok(Color::Rgb(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
    ))
}

fn rgb_components(parser: &mut Parser<'_>) -> Result<Color, Failure> {
    let r = rgb_component(parser)?;
    parser.try_parse(Parser::expect_comma).ok();
    let g = rgb_component(parser)?;
    parser.try_parse(Parser::expect_comma).ok();
    let b = rgb_component(parser)?;
    parser.try_parse(Parser::expect_comma).ok();
    let location = token_location(parser);
    match parser.next() {
        Err(_) => Ok(Color::Rgb(r, g, b)),
        Ok(extra) => Err(Failure::new(
            location,
            format!(
                "rgb() takes three components, found {}",
                render_token(extra)
            ),
        )),
    }
}

fn rgb_component(parser: &mut Parser<'_>) -> Result<f32, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Number { value, .. } if (0.0..=255.0).contains(value) => Ok(value / 255.0),
        _ => Err(Failure::new(
            location,
            format!(
                "rgb() takes three numbers 0-255, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn font_family(parser: &mut Parser<'_>) -> Result<FontFamily, Failure> {
    let (location, token) = next_token(parser)?;
    let name = match &token {
        Token::Ident(name) => name.to_ascii_lowercase(),
        _ => {
            return Err(Failure::new(
                location,
                format!(
                    "font-family takes a family name, found {}",
                    render_token(&token)
                ),
            ))
        }
    };
    match name.as_str() {
        "helvetica" | "sans-serif" => Ok(FontFamily::Helvetica),
        "times" | "serif" => Ok(FontFamily::Times),
        "courier" | "monospace" => Ok(FontFamily::Courier),
        _ => Err(Failure::new(location, format!(
            "unknown font family {name:?}: helvetica, times and courier are available until font embedding lands"
        ))),
    }
}

fn font_weight(parser: &mut Parser<'_>) -> Result<bool, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(false),
        Token::Ident(name) if name.eq_ignore_ascii_case("bold") => Ok(true),
        Token::Number { value, .. } if *value == 400.0 => Ok(false),
        Token::Number { value, .. } if *value == 700.0 => Ok(true),
        _ => Err(Failure::new(
            location,
            format!(
                "font-weight takes normal, bold, 400 or 700, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn font_style(parser: &mut Parser<'_>) -> Result<bool, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Ident(name) if name.eq_ignore_ascii_case("normal") => Ok(false),
        Token::Ident(name) if name.eq_ignore_ascii_case("italic") => Ok(true),
        _ => Err(Failure::new(
            location,
            format!(
                "font-style takes normal or italic, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn line_height(parser: &mut Parser<'_>) -> Result<f32, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Number { value, .. } if *value > 0.0 => Ok(*value),
        _ => Err(Failure::new(
            location,
            format!(
                "line-height takes a positive number, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn text_align(parser: &mut Parser<'_>) -> Result<Align, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Ident(name) if name.eq_ignore_ascii_case("left") => Ok(Align::Left),
        Token::Ident(name) if name.eq_ignore_ascii_case("center") => Ok(Align::Center),
        Token::Ident(name) if name.eq_ignore_ascii_case("right") => Ok(Align::Right),
        Token::Ident(name) if name.eq_ignore_ascii_case("justify") => {
            Err(Failure::new(location, "justify is not supported"))
        }
        _ => Err(Failure::new(
            location,
            format!(
                "text-align takes left, center or right, found {}",
                render_token(&token)
            ),
        )),
    }
}

fn text_decoration(parser: &mut Parser<'_>) -> Result<Decoration, Failure> {
    let (location, token) = next_token(parser)?;
    match &token {
        Token::Ident(name) if name.eq_ignore_ascii_case("none") => Ok(Decoration::None),
        Token::Ident(name) if name.eq_ignore_ascii_case("underline") => Ok(Decoration::Underline),
        Token::Ident(name) if name.eq_ignore_ascii_case("line-through") => {
            Ok(Decoration::LineThrough)
        }
        _ => Err(Failure::new(
            location,
            format!(
                "text-decoration takes none, underline or line-through, found {}",
                render_token(&token)
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(css: &str) -> Vec<Rule> {
        parse_sheet(css).unwrap()
    }

    fn error(css: &str) -> StyleError {
        parse_sheet(css).unwrap_err()
    }

    #[test]
    fn parses_grouped_selectors_and_declarations() {
        let rules = sheet("h1, h2 { font-size: 24pt; color: #c00; }");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].elements, vec![Element::H1, Element::H2]);
        assert_eq!(rules[0].declared.size, Some(FontSize::Pt(24.0)));
        assert_eq!(rules[0].declared.color, Some(Color::Rgb(0.8, 0.0, 0.0)));
    }

    #[test]
    fn parses_units_and_shorthand() {
        let rules = sheet("body { margin: 1in 2cm; font-size: 1.5em; line-height: 1.2; }");
        let d = &rules[0].declared;
        assert_eq!(
            d.margin,
            [
                Some(72.0),
                Some(72.0 / 2.54 * 2.0),
                Some(72.0),
                Some(72.0 / 2.54 * 2.0)
            ]
        );
        assert_eq!(d.size, Some(FontSize::Em(1.5)));
        assert_eq!(d.line_height, Some(1.2));
    }

    #[test]
    fn parses_colors() {
        let rules = sheet("p { color: rgb(255, 128, 0); background-color: navy; }");
        let d = &rules[0].declared;
        assert_eq!(d.color, Some(Color::Rgb(1.0, 128.0 / 255.0, 0.0)));
        assert_eq!(d.background, Some(Color::Rgb(0.0, 0.0, 128.0 / 255.0)));
    }

    #[test]
    fn parses_keyword_properties() {
        let rules = sheet(
            "a { font-family: monospace; font-weight: 700; font-style: italic; \
             text-align: center; text-decoration: underline; }",
        );
        let d = &rules[0].declared;
        assert_eq!(d.family, Some(FontFamily::Courier));
        assert_eq!(d.bold, Some(true));
        assert_eq!(d.italic, Some(true));
        assert_eq!(d.align, Some(Align::Center));
        assert_eq!(d.decoration, Some(Decoration::Underline));
    }

    #[test]
    fn rejects_unknown_selector_property_value_and_unit() {
        assert!(error(".card { color: #000; }").message.contains("selector"));
        assert!(error("p { display: flex; }").message.contains("display"));
        assert!(error("p { text-align: justify; }")
            .message
            .contains("justify"));
        assert!(error("p { font-size: 12px; }").message.contains("px"));
    }

    #[test]
    fn errors_carry_the_location() {
        let e = error("p { color: #000; }\nh1 { volume: 11; }");
        assert_eq!(e.line, 2);
        assert!(e.message.contains("volume"));
    }

    #[test]
    fn comments_and_whitespace_are_ignored() {
        let rules = sheet("/* heading */\nh1 { /* big */ font-size: 20pt; }");
        assert_eq!(rules[0].declared.size, Some(FontSize::Pt(20.0)));
    }

    #[test]
    fn selectors_match_case_insensitively() {
        let rules = sheet("H1, P { font-size: 20pt; }");
        assert_eq!(rules[0].elements, vec![Element::H1, Element::P]);
    }

    #[test]
    fn rejects_rgb_components_outside_0_255() {
        let e = error("p { color: rgb(500, -10, 0); }");
        assert!(e.message.contains("500"));
    }

    #[test]
    fn rejects_a_fourth_rgb_component_at_its_location() {
        let e = error("p { color: rgb(1, 2, 3, 4); }");
        assert_eq!((e.line, e.column), (1, 25));
        assert!(e.message.contains("found 4"), "{}", e.message);
    }

    #[test]
    fn missing_colon_names_the_property() {
        let e = error("p { color #000; }");
        assert_eq!((e.line, e.column), (1, 11));
        assert!(e.message.contains("color"), "{}", e.message);
    }

    #[test]
    fn unterminated_value_fails_at_the_end_of_input() {
        let e = error("p { color:");
        assert_eq!((e.line, e.column), (1, 11));
        assert!(e.message.contains("end of input"), "{}", e.message);
    }
}
