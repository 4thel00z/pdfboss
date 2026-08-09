//! Markdown preview: the per-page Markdown extraction as scrollable text.
//! This module is pure state plus a line-oriented styling pass; the
//! extraction itself runs off the event loop.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::preview::SPINNER;

/// Markdown pane model.
pub struct MarkdownState {
    /// Whether the pane replaces the inspector (`m`).
    pub active: bool,
    pub page: Option<usize>,
    pub source: Option<String>,
    pub scroll: u16,
    pub loading: bool,
    pub spinner_frame: usize,
    pub generation: u64,
    pub error: Option<String>,
}

impl MarkdownState {
    pub fn new() -> MarkdownState {
        MarkdownState {
            active: false,
            page: None,
            source: None,
            scroll: 0,
            loading: false,
            spinner_frame: 0,
            generation: 0,
            error: None,
        }
    }

    /// Marks an extraction in flight for `page`; returns its generation.
    pub fn start_extract(&mut self, page: usize) -> u64 {
        self.generation += 1;
        self.page = Some(page);
        self.loading = true;
        self.source = None;
        self.error = None;
        self.scroll = 0;
        self.generation
    }

    /// Applies a finished extraction; stale generations are dropped.
    /// Returns whether the result was accepted. Nothing here outlives its
    /// generation (unlike the preview's whole-file bytes), so the whole
    /// body is gated on the generation matching.
    pub fn apply_ready(&mut self, generation: u64, result: Result<String, String>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        self.scroll = 0;
        match result {
            Ok(source) => {
                self.source = Some(source);
                self.error = None;
            }
            Err(message) => self.error = Some(message),
        }
        true
    }

    /// 100 ms heartbeat: advances the spinner while an extraction is in
    /// flight.
    pub fn tick(&mut self) {
        if self.loading {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER.len();
        }
    }

    pub fn line_count(&self) -> usize {
        match &self.source {
            Some(source) => source.lines().count(),
            None => 0,
        }
    }

    /// Scrolls by `delta` lines, clamped to the extracted text.
    pub fn scroll_by(&mut self, delta: i32) {
        let target = i64::from(self.scroll) + i64::from(delta);
        self.scroll_to(target.max(0) as u64);
    }

    pub fn scroll_to(&mut self, line: u64) {
        let last = self.line_count().saturating_sub(1) as u64;
        self.scroll = line.min(last).min(u64::from(u16::MAX)) as u16;
    }
}

impl Default for MarkdownState {
    fn default() -> MarkdownState {
        MarkdownState::new()
    }
}

/// The Markdown source as styled terminal lines: one output line per source
/// line, no wrapping (every pane clips at its width). This is a
/// presentation pass, not a parser — a line whose markers do not balance
/// keeps its raw text.
pub fn style_markdown(source: &str) -> Vec<Line<'static>> {
    source.lines().map(style_line).collect()
}

fn style_line(line: &str) -> Line<'static> {
    if let Some(level) = heading_level(line) {
        let style = Style::default()
            .fg(heading_color(level))
            .add_modifier(Modifier::BOLD);
        return Line::from(vec![Span::styled(line.to_string(), style)]);
    }
    if line.starts_with('|') {
        return Line::from(vec![Span::styled(
            line.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )]);
    }
    if let Some(marker_len) = list_marker_len(line) {
        let (marker, rest) = line.split_at(marker_len);
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
        ];
        spans.extend(inline_spans(rest, Style::default()));
        return Line::from(spans);
    }
    Line::from(inline_spans(line, Style::default()))
}

/// `1..=6` for an ATX heading. An escaped leading hash (`\#`, which the
/// Markdown writer emits for body text that starts with one) is prose.
fn heading_level(line: &str) -> Option<u8> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    if !line[hashes..].starts_with(' ') {
        return None;
    }
    Some(hashes as u8)
}

fn heading_color(level: u8) -> Color {
    match level {
        1 => Color::Magenta,
        2 => Color::Cyan,
        _ => Color::Blue,
    }
}

/// Length of a leading `- ` bullet or `12. ` number marker, if any.
fn list_marker_len(line: &str) -> Option<usize> {
    if line.starts_with("- ") {
        return Some(2);
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || !line[digits..].starts_with(". ") {
        return None;
    }
    Some(digits + 2)
}

/// `**bold**` and `*italic*` runs as styled spans; a marker that does not
/// close stays literal text.
fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(at) = rest.find('*') else {
            plain.push_str(rest);
            break;
        };
        let (marker, modifier) = if rest[at..].starts_with("**") {
            ("**", Modifier::BOLD)
        } else {
            ("*", Modifier::ITALIC)
        };
        let after = at + marker.len();
        match emphasis_end(&rest[after..], marker) {
            None => {
                plain.push_str(&rest[..after]);
                rest = &rest[after..];
            }
            Some(offset) => {
                plain.push_str(&rest[..at]);
                if !plain.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut plain), base));
                }
                spans.push(Span::styled(
                    rest[after..after + offset].to_string(),
                    base.add_modifier(modifier),
                ));
                rest = &rest[after + offset + marker.len()..];
            }
        }
    }
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

/// Where the run opened by `marker` closes inside `text`, if it does. Both
/// ends must hug their text — a marker with whitespace on the inside is
/// arithmetic or a bullet glyph, not emphasis.
fn emphasis_end(text: &str, marker: &str) -> Option<usize> {
    if !text.starts_with(|c: char| !c.is_whitespace()) {
        return None;
    }
    let mut from = 0usize;
    while let Some(found) = text[from..].find(marker) {
        let at = from + found;
        if text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
        {
            return Some(at);
        }
        from = at + marker.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(line: &Line<'_>) -> Vec<String> {
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn start_extract_bumps_generation_and_spins() {
        let mut markdown = MarkdownState::new();
        let first = markdown.start_extract(0);
        let second = markdown.start_extract(1);
        assert!(second > first);
        assert_eq!(markdown.page, Some(1));
        assert!(markdown.loading);
        let before = markdown.spinner_frame;
        markdown.tick();
        assert_ne!(
            markdown.spinner_frame, before,
            "spinner advances while extracting"
        );
    }

    #[test]
    fn apply_ready_ignores_stale_generations() {
        let mut markdown = MarkdownState::new();
        let stale = markdown.start_extract(0);
        let current = markdown.start_extract(0);
        assert!(!markdown.apply_ready(stale, Ok("# stale".to_string())));
        assert!(markdown.source.is_none(), "stale text is not installed");
        assert!(markdown.loading, "stale result leaves the spinner on");
        assert!(markdown.apply_ready(current, Ok("# fresh".to_string())));
        assert!(!markdown.loading);
        assert_eq!(markdown.source.as_deref(), Some("# fresh"));
        assert!(markdown.apply_ready(current, Err("boom".to_string())));
        assert_eq!(markdown.error.as_deref(), Some("boom"));
    }

    #[test]
    fn scrolling_clamps_to_the_extracted_lines() {
        let mut markdown = MarkdownState::new();
        markdown.scroll_by(5);
        assert_eq!(markdown.scroll, 0, "nothing extracted yet");
        let generation = markdown.start_extract(0);
        markdown.apply_ready(generation, Ok("a\nb\nc".to_string()));
        assert_eq!(markdown.line_count(), 3);
        markdown.scroll_by(10);
        assert_eq!(markdown.scroll, 2, "clamped to the last line");
        markdown.scroll_by(-10);
        assert_eq!(markdown.scroll, 0, "clamped at the top");
    }

    #[test]
    fn headings_are_bold_and_colored() {
        let lines = style_markdown("# Title\n### Sub");
        assert_eq!(texts(&lines[0]), vec!["# Title"]);
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Magenta));
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn escaped_leading_hash_is_prose() {
        let lines = style_markdown("\\# not a heading");
        assert_eq!(texts(&lines[0]), vec!["\\# not a heading"]);
        assert!(
            !lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "an escaped hash is body text the writer escaped, not a heading"
        );
        let lines = style_markdown("#no space");
        assert!(!lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn list_items_are_indented_behind_their_marker() {
        let lines = style_markdown("- first\n12. twelfth\n-nomarker");
        assert_eq!(texts(&lines[0]), vec!["  ", "- ", "first"]);
        assert_eq!(texts(&lines[1]), vec!["  ", "12. ", "twelfth"]);
        assert_eq!(texts(&lines[2]), vec!["-nomarker"], "no marker, no indent");
    }

    #[test]
    fn pipe_rows_are_dimmed() {
        let lines = style_markdown("| a | b |");
        assert_eq!(texts(&lines[0]), vec!["| a | b |"]);
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn emphasis_runs_become_styled_spans() {
        let lines = style_markdown("plain **bold** and *italic* tail");
        assert_eq!(
            texts(&lines[0]),
            vec!["plain ", "bold", " and ", "italic", " tail"]
        );
        assert!(lines[0].spans[1]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(lines[0].spans[3]
            .style
            .add_modifier
            .contains(Modifier::ITALIC));
    }

    #[test]
    fn unbalanced_markers_stay_literal() {
        let lines = style_markdown("2 * 3 = 6 and **dangling");
        assert_eq!(texts(&lines[0]), vec!["2 * 3 = 6 and **dangling"]);
        assert!(
            !lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD),
            "an unmatched marker is arithmetic, not emphasis"
        );
    }

    #[test]
    fn plain_prose_is_one_unstyled_span() {
        let lines = style_markdown("just words");
        assert_eq!(texts(&lines[0]), vec!["just words"]);
        assert_eq!(lines[0].spans[0].style, Style::default());
    }
}
