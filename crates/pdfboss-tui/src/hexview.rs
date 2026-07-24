//! Hex pane: hexyl-style `offset │ hex │ ascii` lines with byte-class
//! colors, windowed fetching over a span, and objstm-member highlighting.

use pdfboss_core::elements::Span;
use pdfboss_core::ObjRef;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

/// Bytes shown per hex line (8 keeps lines inside a 35%-split 80-col pane).
pub const BYTES_PER_LINE: usize = 8;
/// Bytes fetched per window; spans larger than this stream on demand.
pub const WINDOW_BYTES: usize = 64 * 1024;

/// hexyl-style byte classes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ByteClass {
    Null,
    Printable,
    Whitespace,
    Other,
}

/// Classifies a byte for coloring.
pub fn byte_class(byte: u8) -> ByteClass {
    match byte {
        0x00 => ByteClass::Null,
        b'\t' | b'\n' | b'\x0c' | b'\r' => ByteClass::Whitespace,
        0x20..=0x7e => ByteClass::Printable,
        // 0x0b (vertical tab) and everything non-ascii-printable.
        0x01..=0x1f | 0x7f..=0xff => ByteClass::Other,
    }
}

/// Color per byte class.
pub fn class_color(class: ByteClass) -> Color {
    match class {
        ByteClass::Null => Color::DarkGray,
        ByteClass::Printable => Color::Cyan,
        ByteClass::Whitespace => Color::Green,
        ByteClass::Other => Color::Yellow,
    }
}

/// Where the hex pane's bytes come from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HexSource {
    /// A byte range of the physical file; offsets shown are absolute.
    File { span: Span },
    /// The decoded bytes of an object-stream container; offsets shown are
    /// relative to the decoded buffer (a member's range is highlighted).
    DecodedObjStm { container: ObjRef },
}

/// Hex pane model. `scroll_line` addresses the whole source in
/// [`BYTES_PER_LINE`]-byte lines; only one [`WINDOW_BYTES`] window of
/// bytes is resident at a time.
pub struct HexState {
    pub source: Option<HexSource>,
    /// Total viewable length: span length (File) or decoded length (ObjStm).
    pub total_len: u64,
    /// Absolute display offset of relative offset 0 (span.start for File).
    pub base: u64,
    /// Relative offset of `bytes[0]` within the source.
    pub window_start: u64,
    pub bytes: Vec<u8>,
    pub scroll_line: u64,
    /// Highlighted byte range, relative to the source (objstm members).
    pub highlight: Option<Span>,
    pub loading: bool,
    pub error: Option<String>,
}

impl HexState {
    pub fn new() -> HexState {
        HexState {
            source: None,
            total_len: 0,
            base: 0,
            window_start: 0,
            bytes: Vec::new(),
            scroll_line: 0,
            highlight: None,
            loading: false,
            error: None,
        }
    }

    /// Points the pane at a new source; bytes arrive via [`apply_loaded`].
    pub fn set_source(&mut self, source: HexSource) {
        self.total_len = match &source {
            HexSource::File { span } => span.end.saturating_sub(span.start),
            // Unknown until the container is decoded.
            HexSource::DecodedObjStm { .. } => 0,
        };
        self.base = match &source {
            HexSource::File { span } => span.start,
            HexSource::DecodedObjStm { .. } => 0,
        };
        self.source = Some(source);
        self.window_start = 0;
        self.bytes.clear();
        self.scroll_line = 0;
        self.highlight = None;
        self.loading = true;
        self.error = None;
    }

    /// Empties the pane (folder-ish selections have no bytes).
    pub fn clear(&mut self) {
        self.source = None;
        self.total_len = 0;
        self.base = 0;
        self.window_start = 0;
        self.bytes.clear();
        self.scroll_line = 0;
        self.highlight = None;
        self.loading = false;
        self.error = None;
    }

    /// Installs a loaded window.
    pub fn apply_loaded(&mut self, window_start: u64, total_len: u64, bytes: Vec<u8>) {
        self.window_start = window_start;
        self.total_len = total_len;
        self.bytes = bytes;
        self.loading = false;
        self.error = None;
    }

    /// Total number of hex lines in the source.
    pub fn line_count(&self) -> u64 {
        self.total_len.div_ceil(BYTES_PER_LINE as u64)
    }

    /// Scrolls by `delta` lines, clamped to the source.
    pub fn scroll_by(&mut self, delta: i64) {
        let target = if delta.is_negative() {
            self.scroll_line.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll_line.saturating_add(delta as u64)
        };
        self.scroll_to(target);
    }

    /// Scrolls to an absolute line, clamped to the last line.
    pub fn scroll_to(&mut self, line: u64) {
        let last = self.line_count().saturating_sub(1);
        self.scroll_line = line.min(last);
    }

    /// If the rows `scroll_line..scroll_line+visible` need bytes outside
    /// the resident window, returns the window start to fetch.
    pub fn visible_window_missing(&self, visible: u16) -> Option<u64> {
        if self.source.is_none() || self.total_len == 0 {
            return None;
        }
        let first_byte = self.scroll_line * BYTES_PER_LINE as u64;
        let last_byte =
            ((self.scroll_line + visible as u64) * BYTES_PER_LINE as u64).min(self.total_len);
        let window_end = self.window_start + self.bytes.len() as u64;
        if first_byte >= self.window_start && last_byte <= window_end {
            return None;
        }
        Some(window_for_line(self.total_len, self.scroll_line).0)
    }

    /// Pane title: source and viewed range.
    pub fn title(&self) -> String {
        match &self.source {
            None => "Hex".to_string(),
            Some(HexSource::File { span }) => {
                format!("Hex {:#x}..{:#x}", span.start, span.end)
            }
            Some(HexSource::DecodedObjStm { container }) => {
                if self.total_len == 0 {
                    format!("Hex obj {} {} decoded", container.num, container.gen)
                } else {
                    format!(
                        "Hex obj {} {} decoded {:#x}..{:#x}",
                        container.num, container.gen, 0, self.total_len
                    )
                }
            }
        }
    }
}

impl Default for HexState {
    fn default() -> HexState {
        HexState::new()
    }
}

/// The [`WINDOW_BYTES`]-aligned window (relative start, length) containing
/// 8-byte line `line` of a `total_len`-byte source.
pub fn window_for_line(total_len: u64, line: u64) -> (u64, usize) {
    let byte = line * BYTES_PER_LINE as u64;
    let start = (byte / WINDOW_BYTES as u64) * WINDOW_BYTES as u64;
    let len = (total_len - start.min(total_len)).min(WINDOW_BYTES as u64) as usize;
    (start, len)
}

/// Columns `(first, end_exclusive)` of a line starting at relative offset
/// `line_off` with `len` bytes that fall inside `highlight`.
pub fn highlight_cols(line_off: u64, len: usize, highlight: Span) -> Option<(usize, usize)> {
    let line_end = line_off + len as u64;
    let start = highlight.start.max(line_off);
    let end = highlight.end.min(line_end);
    if start >= end {
        return None;
    }
    Some(((start - line_off) as usize, (end - line_off) as usize))
}

/// Renders one hex line: `AAAAAAAA │ xx xx … │ ascii`, byte-class colored,
/// with columns in `hl` shown REVERSED.
pub fn hex_line(abs_off: u64, bytes: &[u8], hl: Option<(usize, usize)>) -> Line<'static> {
    let mut parts: Vec<ratatui::text::Span<'static>> = Vec::new();
    parts.push(ratatui::text::Span::styled(
        format!("{:08x} \u{2502} ", abs_off),
        Style::default().fg(Color::DarkGray),
    ));
    let highlighted =
        |column: usize| -> bool { hl.is_some_and(|(first, end)| column >= first && column < end) };
    for column in 0..BYTES_PER_LINE {
        match bytes.get(column) {
            Some(byte) => {
                let mut style = Style::default().fg(class_color(byte_class(*byte)));
                if highlighted(column) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                parts.push(ratatui::text::Span::styled(format!("{:02x} ", byte), style));
            }
            None => parts.push(ratatui::text::Span::raw("   ")),
        }
    }
    parts.push(ratatui::text::Span::styled(
        "\u{2502} ".to_string(),
        Style::default().fg(Color::DarkGray),
    ));
    for (column, byte) in bytes.iter().enumerate() {
        let symbol = if (0x20..=0x7e).contains(byte) {
            char::from(*byte).to_string()
        } else {
            "\u{b7}".to_string()
        };
        let mut style = Style::default().fg(class_color(byte_class(*byte)));
        if highlighted(column) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        parts.push(ratatui::text::Span::styled(symbol, style));
    }
    Line::from(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::elements::Span;

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|part| part.content.as_ref())
            .collect()
    }

    #[test]
    fn byte_classes() {
        assert_eq!(byte_class(0x00), ByteClass::Null);
        assert_eq!(byte_class(b'A'), ByteClass::Printable);
        assert_eq!(byte_class(b' '), ByteClass::Printable);
        assert_eq!(byte_class(b'\n'), ByteClass::Whitespace);
        assert_eq!(byte_class(b'\t'), ByteClass::Whitespace);
        assert_eq!(byte_class(b'\r'), ByteClass::Whitespace);
        assert_eq!(byte_class(0xE2), ByteClass::Other);
    }

    #[test]
    fn hex_line_formats_full_and_short_rows() {
        let full = hex_line(0, b"%PDF-1.7", None);
        assert_eq!(
            line_text(&full),
            "00000000 \u{2502} 25 50 44 46 2d 31 2e 37 \u{2502} %PDF-1.7"
        );
        let short = hex_line(8, &[0x0a, 0x25, 0xe2, 0xe3, 0xcf, 0xd3, 0x0a], None);
        assert_eq!(
            line_text(&short),
            "00000008 \u{2502} 0a 25 e2 e3 cf d3 0a    \u{2502} \u{b7}%\u{b7}\u{b7}\u{b7}\u{b7}\u{b7}"
        );
    }

    #[test]
    fn hex_line_reverses_highlighted_columns() {
        let line = hex_line(0, b"ABCDEFGH", Some((2, 5)));
        // Byte cells 2..5 and their ascii cells carry REVERSED style.
        let styled: Vec<(String, bool)> = line
            .spans
            .iter()
            .map(|part| {
                (
                    part.content.as_ref().to_string(),
                    part.style
                        .add_modifier
                        .contains(ratatui::style::Modifier::REVERSED),
                )
            })
            .collect();
        let reversed_text: String = styled
            .iter()
            .filter(|(_, on)| *on)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(reversed_text, "43 44 45 CDE");
    }

    #[test]
    fn window_math_covers_span_in_aligned_chunks() {
        assert_eq!(window_for_line(100, 0), (0, 100));
        assert_eq!(window_for_line(200_000, 0), (0, WINDOW_BYTES));
        // Line 8192 starts at byte 65536: second window.
        assert_eq!(window_for_line(200_000, 8192), (65536, WINDOW_BYTES));
        // Final window is short.
        assert_eq!(window_for_line(200_000, 24576), (196_608, 3392));
    }

    #[test]
    fn highlight_math_clamps_to_line() {
        let hl = Span { start: 10, end: 20 };
        assert_eq!(highlight_cols(0, 8, hl), None);
        assert_eq!(highlight_cols(8, 8, hl), Some((2, 8)));
        assert_eq!(highlight_cols(16, 8, hl), Some((0, 4)));
        assert_eq!(highlight_cols(24, 8, hl), None);
        assert_eq!(
            highlight_cols(8, 4, Span { start: 10, end: 11 }),
            Some((2, 3))
        );
    }

    #[test]
    fn state_scrolls_within_span_and_reports_missing_window() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::File {
            span: Span {
                start: 0x10,
                end: 0x10 + 200_000,
            },
        });
        assert!(hex.loading);
        hex.apply_loaded(0, 200_000, vec![0u8; WINDOW_BYTES]);
        assert!(!hex.loading);
        assert_eq!(hex.line_count(), 25_000);
        hex.scroll_by(5);
        assert_eq!(hex.scroll_line, 5);
        assert_eq!(hex.visible_window_missing(7), None);
        hex.scroll_to(24_999);
        assert_eq!(hex.scroll_line, 24_999);
        assert_eq!(hex.visible_window_missing(7), Some(196_608));
        hex.scroll_by(-50_000);
        assert_eq!(hex.scroll_line, 0);
        assert_eq!(hex.title(), "Hex 0x10..0x30d50");
    }

    #[test]
    fn objstm_source_titles_and_holds_highlight() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::DecodedObjStm {
            container: pdfboss_core::ObjRef { num: 9, gen: 0 },
        });
        hex.highlight = Some(Span { start: 4, end: 30 });
        hex.apply_loaded(0, 64, vec![0u8; 64]);
        assert_eq!(hex.title(), "Hex obj 9 0 decoded 0x0..0x40");
    }

    #[test]
    fn cleared_state_has_no_title_range() {
        let mut hex = HexState::new();
        hex.set_source(HexSource::File {
            span: Span { start: 0, end: 8 },
        });
        hex.clear();
        assert_eq!(hex.title(), "Hex");
        assert_eq!(hex.line_count(), 0);
    }
}
