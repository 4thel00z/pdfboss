//! Greedy line breaker over mixed-style text runs.

use pdfboss_style::Decoration;
use pdfboss_write::{Color, Error, Standard14};

/// A run of text sharing one set of styling and layout properties.
#[derive(Debug, Clone)]
pub(crate) struct StyledRun {
    /// The run's text.
    pub text: String,
    /// The font this run is set in.
    pub font: Standard14,
    /// Font size in points.
    pub size: f32,
    /// Text fill color.
    pub color: Color,
    /// Optional background fill color.
    pub background: Option<Color>,
    /// Underline / strike-through / none.
    pub decoration: Decoration,
    /// Optional link destination.
    pub link: Option<String>,
}

/// A positioned, measured slice of text within a line, carrying its run's
/// style.
#[derive(Debug, Clone)]
pub(crate) struct Frag {
    /// X offset from the start of the line.
    pub dx: f32,
    /// Measured width of `text`.
    pub width: f32,
    /// The fragment's text.
    pub text: String,
    /// The font this fragment is set in.
    pub font: Standard14,
    /// Font size in points.
    pub size: f32,
    /// Text fill color.
    pub color: Color,
    /// Optional background fill color.
    pub background: Option<Color>,
    /// Underline / strike-through / none.
    pub decoration: Decoration,
    /// Optional link destination.
    pub link: Option<String>,
}

/// One wrapped line: its fragments, total width, and the size to use for
/// line-height purposes (the largest fragment size, or `empty_size` when the
/// line has no fragments).
#[derive(Debug, Clone)]
pub(crate) struct LineBox {
    /// The line's fragments, in order.
    pub frags: Vec<Frag>,
    /// Sum of fragment widths.
    pub width: f32,
    /// The line's size for line-height purposes.
    pub max_size: f32,
}

/// One token of the piece stream: a word, a run of whitespace (not
/// containing a newline), or a hard line break.
enum Piece<'a> {
    Word { run: usize, text: &'a str },
    Space { run: usize, text: &'a str },
    Break,
}

/// Splits each run's text into maximal newline, whitespace, and word
/// segments, tagged with their originating run index.
fn pieces(runs: &[StyledRun]) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let text = run.text.as_str();
        let mut start = 0;
        let mut kind: Option<bool> = None;
        for (offset, ch) in text.char_indices() {
            if ch == '\n' {
                push_segment(&mut out, index, text, start, offset, kind);
                out.push(Piece::Break);
                start = offset + ch.len_utf8();
                kind = None;
                continue;
            }
            let whitespace = ch.is_whitespace();
            if kind != Some(whitespace) {
                push_segment(&mut out, index, text, start, offset, kind);
                start = offset;
                kind = Some(whitespace);
            }
        }
        push_segment(&mut out, index, text, start, text.len(), kind);
    }
    out
}

fn push_segment<'a>(
    out: &mut Vec<Piece<'a>>,
    run: usize,
    text: &'a str,
    start: usize,
    end: usize,
    kind: Option<bool>,
) {
    if start >= end {
        return;
    }
    let Some(whitespace) = kind else { return };
    let segment = &text[start..end];
    if whitespace {
        out.push(Piece::Space { run, text: segment });
        return;
    }
    out.push(Piece::Word { run, text: segment });
}

/// Measures `text` set in `run`'s font and size.
fn measure(run: &StyledRun, text: &str) -> Result<f32, Error> {
    run.font.text_width(text, run.size)
}

/// Wraps `runs` into lines no wider than `max_width`. Break opportunities
/// are whitespace runs; a word may span multiple styled runs. `empty_size`
/// is the `max_size` given to fragment-less lines produced by hard breaks.
pub(crate) fn wrap(
    runs: &[StyledRun],
    max_width: f32,
    empty_size: f32,
) -> Result<Vec<LineBox>, Error> {
    let mut wrapper = Wrapper::new(runs, max_width, empty_size);
    for piece in pieces(runs) {
        wrapper.piece(piece)?;
    }
    wrapper.finish()
}

struct Wrapper<'a> {
    runs: &'a [StyledRun],
    max_width: f32,
    empty_size: f32,
    lines: Vec<LineBox>,
    line: Vec<Frag>,
    line_width: f32,
    word: Vec<(usize, String)>,
    word_width: f32,
    spaces: Vec<(usize, String)>,
    space_width: f32,
}

impl<'a> Wrapper<'a> {
    fn new(runs: &'a [StyledRun], max_width: f32, empty_size: f32) -> Wrapper<'a> {
        Wrapper {
            runs,
            max_width,
            empty_size,
            lines: Vec::new(),
            line: Vec::new(),
            line_width: 0.0,
            word: Vec::new(),
            word_width: 0.0,
            spaces: Vec::new(),
            space_width: 0.0,
        }
    }

    fn piece(&mut self, piece: Piece<'a>) -> Result<(), Error> {
        match piece {
            Piece::Space { run, text } => self.space(run, text),
            Piece::Word { run, text } => self.word(run, text),
            Piece::Break => self.hard_break(),
        }
    }

    fn space(&mut self, run: usize, text: &str) -> Result<(), Error> {
        self.flush_word()?;
        let width = measure(&self.runs[run], text)?;
        self.spaces.push((run, text.to_string()));
        self.space_width += width;
        Ok(())
    }

    fn word(&mut self, run: usize, text: &str) -> Result<(), Error> {
        let width = measure(&self.runs[run], text)?;
        self.word_width += width;
        if let Some((last_run, last_text)) = self.word.last_mut() {
            if *last_run == run {
                last_text.push_str(text);
                return Ok(());
            }
        }
        self.word.push((run, text.to_string()));
        Ok(())
    }

    fn hard_break(&mut self) -> Result<(), Error> {
        self.flush_word()?;
        self.emit_line();
        Ok(())
    }

    fn flush_word(&mut self) -> Result<(), Error> {
        if self.word.is_empty() {
            return Ok(());
        }
        if !self.line.is_empty()
            && self.line_width + self.space_width + self.word_width > self.max_width
        {
            self.emit_line();
        }
        if self.line.is_empty() && self.word_width > self.max_width {
            self.char_break()?;
        }
        if !self.line.is_empty() {
            self.append_spaces()?;
        }
        self.spaces.clear();
        self.space_width = 0.0;
        let word = std::mem::take(&mut self.word);
        self.word_width = 0.0;
        for (run, text) in word {
            self.append_frag(run, text)?;
        }
        Ok(())
    }

    fn char_break(&mut self) -> Result<(), Error> {
        let word = std::mem::take(&mut self.word);
        self.word_width = 0.0;
        let mut current: Vec<(usize, String)> = Vec::new();
        let mut current_width = 0.0f32;
        for (run, text) in word {
            for ch in text.chars() {
                let mut buffer = [0u8; 4];
                let piece = ch.encode_utf8(&mut buffer);
                let width = measure(&self.runs[run], piece)?;
                if current_width + width > self.max_width && !current.is_empty() {
                    self.spaces.clear();
                    self.space_width = 0.0;
                    for (run, text) in std::mem::take(&mut current) {
                        self.append_frag(run, text)?;
                    }
                    current_width = 0.0;
                    self.emit_line();
                }
                match current.last_mut() {
                    Some((last_run, last_text)) if *last_run == run => last_text.push(ch),
                    _ => current.push((run, piece.to_string())),
                }
                current_width += width;
            }
        }
        self.word = current;
        self.word_width = current_width;
        Ok(())
    }

    fn append_spaces(&mut self) -> Result<(), Error> {
        let spaces = std::mem::take(&mut self.spaces);
        for (run, text) in spaces {
            let width = measure(&self.runs[run], &text)?;
            let dx = self.line_width;
            self.push_frag(run, text, dx, width);
        }
        Ok(())
    }

    fn append_frag(&mut self, run: usize, text: String) -> Result<(), Error> {
        let width = measure(&self.runs[run], &text)?;
        let dx = self.line_width;
        self.push_frag(run, text, dx, width);
        Ok(())
    }

    fn push_frag(&mut self, run: usize, text: String, dx: f32, width: f32) {
        let source = &self.runs[run];
        if let Some(last) = self.line.last_mut() {
            let same_style = last.font == source.font
                && last.size == source.size
                && last.color == source.color
                && last.background == source.background
                && last.decoration == source.decoration
                && last.link == source.link;
            if same_style {
                last.text.push_str(&text);
                last.width += width;
                self.line_width += width;
                return;
            }
        }
        self.line.push(Frag {
            dx,
            width,
            text,
            font: source.font,
            size: source.size,
            color: source.color,
            background: source.background,
            decoration: source.decoration,
            link: source.link.clone(),
        });
        self.line_width += width;
    }

    fn emit_line(&mut self) {
        let frags = std::mem::take(&mut self.line);
        let width = self.line_width;
        let max_size = frags
            .iter()
            .map(|frag| frag.size)
            .fold(None::<f32>, |acc, size| match acc {
                Some(current) => Some(current.max(size)),
                None => Some(size),
            })
            .unwrap_or(self.empty_size);
        self.lines.push(LineBox {
            frags,
            width,
            max_size,
        });
        self.line_width = 0.0;
    }

    fn finish(mut self) -> Result<Vec<LineBox>, Error> {
        self.flush_word()?;
        if !self.line.is_empty() {
            self.emit_line();
        }
        Ok(self.lines)
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_style::Decoration;
    use pdfboss_write::{Color, Standard14};

    use super::*;

    fn mono(text: &str) -> StyledRun {
        StyledRun {
            text: text.into(),
            font: Standard14::Courier,
            size: 10.0,
            color: Color::BLACK,
            background: None,
            decoration: Decoration::None,
            link: None,
        }
    }

    #[test]
    fn wraps_at_word_boundaries() {
        let lines = wrap(&[mono("aaa bbb ccc")], 42.0, 10.0).unwrap();
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.frags.iter().map(|f| f.text.as_str()).collect::<String>())
            .collect();
        assert_eq!(texts, vec!["aaa bbb", "ccc"]);
        assert_eq!(lines[0].width, 42.0);
    }

    #[test]
    fn a_word_spanning_two_runs_stays_together() {
        let mut bold = mono("fix");
        bold.font = Standard14::CourierBold;
        let lines = wrap(&[mono("re"), bold, mono("ed after")], 42.0, 10.0).unwrap();
        let first: String = lines[0].frags.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(first, "refixed");
        assert_eq!(lines[0].frags.len(), 3);
        assert_eq!(lines[0].frags[1].dx, 12.0);
    }

    #[test]
    fn hard_break_forces_a_new_line_and_blank_lines_survive() {
        let lines = wrap(&[mono("a\n\nb")], 100.0, 10.0).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].frags.is_empty());
        assert_eq!(lines[1].max_size, 10.0);
    }

    #[test]
    fn overlong_word_breaks_by_character() {
        let lines = wrap(&[mono("abcdefghij")], 30.0, 10.0).unwrap();
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.frags.iter().map(|f| f.text.as_str()).collect::<String>())
            .collect();
        assert_eq!(texts, vec!["abcde", "fghij"]);
    }

    #[test]
    fn spaces_at_a_break_are_dropped() {
        let lines = wrap(&[mono("aaa   bbb")], 20.0, 10.0).unwrap();
        assert_eq!(lines[0].width, 18.0);
        assert_eq!(lines[1].frags[0].text, "bbb");
        assert_eq!(lines[1].frags[0].dx, 0.0);
    }
}
