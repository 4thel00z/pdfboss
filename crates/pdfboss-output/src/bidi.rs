//! Reading order for right-to-left lines: an assembled line holds its text
//! in drawing order, and the Unicode Bidirectional Algorithm (UAX #9) puts
//! it back in the order it is read.

use crate::ir::Inline;
use std::ops::Range;
use unicode_bidi::{bidi_class, BidiClass, Level, ParagraphBidiInfo};

/// A byte range of one inline's text that is already in reading order: the
/// text of a span from `/ReversedChars` or `/ActualText`.
pub(crate) struct LogicalText {
    pub inline: usize,
    pub bytes: Range<usize>,
}

/// One unit the reordering moves as a whole: a letter with the combining
/// marks ([`is_mark`]) drawn after it, or a whole [`LogicalText`].
struct Unit {
    inline: usize,
    bytes: Range<usize>,
    /// The character whose direction class stands for the unit.
    class_of: char,
}

/// Whether `c` is a strong right-to-left character (bidi class R or AL).
fn is_rtl(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// Whether `c` is a combining mark: bidi class NSM, or an Arabic mark in
/// an isolated presentation form, which fonts often map their mark glyphs
/// to: the shadda ligatures U+FC5E to U+FC63 and the isolated fathatan,
/// dammatan, kasratan, fatha, damma, kasra, shadda and sukun at the even
/// code points U+FE70 to U+FE7E. The odd code points there are medial
/// forms on a tatweel, which has an advance of its own.
pub(crate) fn is_mark(c: char) -> bool {
    match c {
        '\u{FC5E}'..='\u{FC63}' => true,
        '\u{FE70}'..='\u{FE7E}' => (c as u32).is_multiple_of(2),
        _ => bidi_class(c) == BidiClass::NSM,
    }
}

/// Whether `text` holds a strong right-to-left character. Every such
/// character is U+0590 or above, so ASCII text and the Latin scripts below
/// that point skip the class lookup.
fn holds_rtl(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    text.chars().any(|c| c >= '\u{0590}' && is_rtl(c))
}

/// The line's runs in reading order. `inlines` holds the line in drawing
/// order, leftmost glyph first. The line reads right to left when it has
/// more strong right-to-left characters than strong left-to-right ones; the
/// first-strong rule of UAX #9 does not apply, since in drawing order the
/// first character of a right-to-left line is the last one read. The
/// algorithm resolves each unit's level as if the drawing order were the
/// reading order and reverses the runs it would reverse for display, which
/// undoes that reversal. A line without a right-to-left character comes
/// back unchanged.
pub(crate) fn reading_order(inlines: Vec<Inline>, logical: &[LogicalText]) -> Vec<Inline> {
    if !inlines.iter().any(|inline| holds_rtl(&inline.text)) {
        return inlines;
    }
    let units = units(&inlines, logical);
    let classes: String = units
        .iter()
        .map(|unit| match bidi_class(unit.class_of) {
            BidiClass::B => '\u{FFFC}',
            _ => unit.class_of,
        })
        .collect();
    let info = ParagraphBidiInfo::new(&classes, Some(paragraph_level(&inlines)));
    let levels = info.reordered_levels_per_char(0..classes.len());
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for index in ParagraphBidiInfo::reorder_visual(&levels) {
        let unit = &units[index];
        let source = &inlines[unit.inline];
        let text = &source.text[unit.bytes.clone()];
        if let Some(last) = out.last_mut().filter(|last| same_style(last, source)) {
            last.text.push_str(text);
            continue;
        }
        out.push(Inline {
            text: text.to_string(),
            bold: source.bold,
            italic: source.italic,
            code: source.code,
        });
    }
    out
}

/// Right to left when the line has more strong right-to-left characters
/// than strong left-to-right ones, else left to right. Marks do not count,
/// though the Arabic presentation-form marks are classed as letters.
fn paragraph_level(inlines: &[Inline]) -> Level {
    let (rtl, ltr) = inlines
        .iter()
        .flat_map(|inline| inline.text.chars())
        .filter(|&c| !is_mark(c))
        .fold((0usize, 0usize), |(rtl, ltr), c| match bidi_class(c) {
            BidiClass::R | BidiClass::AL => (rtl + 1, ltr),
            BidiClass::L => (rtl, ltr + 1),
            _ => (rtl, ltr),
        });
    if rtl > ltr {
        return Level::rtl();
    }
    Level::ltr()
}

/// The line cut into [`Unit`]s, in drawing order.
fn units(inlines: &[Inline], logical: &[LogicalText]) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    for (index, inline) in inlines.iter().enumerate() {
        let mut held = logical
            .iter()
            .filter(|text| text.inline == index)
            .peekable();
        let mut cluster_open = false;
        for (at, c) in inline.text.char_indices() {
            if held.peek().is_some_and(|text| at >= text.bytes.end) {
                held.next();
            }
            if let Some(text) = held.peek().filter(|text| text.bytes.contains(&at)) {
                if at == text.bytes.start {
                    let span = &inline.text[text.bytes.clone()];
                    units.push(Unit {
                        inline: index,
                        bytes: text.bytes.clone(),
                        class_of: span.chars().find(|&c| is_strong(c)).unwrap_or(c),
                    });
                }
                cluster_open = false;
                continue;
            }
            let end = at + c.len_utf8();
            if cluster_open && is_mark(c) {
                if let Some(last) = units.last_mut() {
                    last.bytes.end = end;
                }
                continue;
            }
            units.push(Unit {
                inline: index,
                bytes: at..end,
                class_of: c,
            });
            cluster_open = true;
        }
    }
    units
}

fn is_strong(c: char) -> bool {
    matches!(bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::L)
}

fn same_style(a: &Inline, b: &Inline) -> bool {
    a.bold == b.bold && a.italic == b.italic && a.code == b.code
}

#[cfg(test)]
mod tests {
    use super::{holds_rtl, paragraph_level};
    use crate::ir::Inline;
    use unicode_bidi::Level;

    fn inline(text: &str) -> Inline {
        Inline {
            text: text.to_string(),
            bold: false,
            italic: false,
            code: false,
        }
    }

    #[test]
    fn presentation_form_marks_do_not_decide_the_paragraph_direction() {
        let line = [inline("ab \u{628}\u{FE7C}\u{FE7C}")];
        assert_eq!(paragraph_level(&line), Level::ltr());
        let line = [inline("a \u{628}\u{62A}\u{FE7C}")];
        assert_eq!(paragraph_level(&line), Level::rtl());
    }

    #[test]
    fn holds_rtl_sees_hebrew_and_arabic_and_nothing_below_them() {
        assert!(!holds_rtl("plain ascii 123"));
        assert!(!holds_rtl("Größe – «quoted» €"));
        assert!(holds_rtl("\u{5D0}"));
        assert!(holds_rtl("word \u{627}\u{644} word"));
        assert!(!holds_rtl("\u{660}\u{661}"));
    }
}
