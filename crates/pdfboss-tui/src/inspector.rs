//! Inspector pane: the selected element pretty-printed, with `d` cycling
//! raw bytes / decoded bytes / disassembled content operators for streams,
//! and a cursor over `N G R` references for Enter-to-jump.

use pdfboss_core::content::parse_content;
use pdfboss_core::pretty;
use pdfboss_core::Dict;
use pdfboss_core::{ObjRef, Object};

/// Maximum lines the Raw/Decoded byte views materialize.
const MAX_BYTE_LINES: usize = 2000;

/// Which view of the selection is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InspectorMode {
    Pretty,
    Raw,
    Decoded,
    Ops,
}

/// Async payloads the inspector consumes.
#[derive(Debug, Clone)]
pub enum InspectorPayload {
    /// The parsed object (streams carry raw data in `Object::Stream`).
    Object { r: ObjRef, object: Object },
    /// Decoded stream data for the Decoded/Ops views.
    Decoded { r: ObjRef, data: Vec<u8> },
}

/// Inspector pane model.
pub struct InspectorState {
    pub title: String,
    pub object: Option<(ObjRef, Object)>,
    pub decoded: Option<Vec<u8>>,
    pub mode: InspectorMode,
    pub scroll: u16,
    pub lines: Vec<String>,
    /// `(line index, ref)` pairs found in the Pretty text, display order.
    pub refs: Vec<(usize, ObjRef)>,
    pub ref_cursor: Option<usize>,
    pub loading: bool,
}

impl InspectorState {
    pub fn new() -> InspectorState {
        InspectorState {
            title: String::new(),
            object: None,
            decoded: None,
            mode: InspectorMode::Pretty,
            scroll: 0,
            lines: Vec::new(),
            refs: Vec::new(),
            ref_cursor: None,
            loading: false,
        }
    }

    /// Shows plain informational lines (folders, xref summaries, errors).
    pub fn show_message(&mut self, title: &str, lines: Vec<String>) {
        self.title = title.to_string();
        self.object = None;
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.lines = lines;
        self.refs = Vec::new();
        self.ref_cursor = None;
        self.loading = false;
    }

    /// Placeholder while an object fetch is in flight.
    pub fn show_loading(&mut self, title: &str) {
        self.show_message(title, vec!["loading\u{2026}".to_string()]);
        self.loading = true;
    }

    /// Installs a fetched object and rebuilds the Pretty view.
    pub fn set_object(&mut self, r: ObjRef, object: Object) {
        self.title = format!("obj {} {}", r.num, r.gen);
        self.object = Some((r, object));
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.ref_cursor = None;
        self.loading = false;
        self.rebuild();
    }

    /// Installs decoded stream data (ignored unless it matches the shown
    /// object) and refreshes decoded-backed views.
    pub fn set_decoded(&mut self, r: ObjRef, data: Vec<u8>) {
        let matches_current = self
            .object
            .as_ref()
            .is_some_and(|(shown, ..)| shown.num == r.num && shown.gen == r.gen);
        if !matches_current {
            return;
        }
        self.decoded = Some(data);
        if matches!(self.mode, InspectorMode::Decoded | InspectorMode::Ops) {
            self.rebuild();
        }
    }

    /// Shows a bare dictionary (the trailer) pretty-printed, refs jumpable.
    pub fn set_dict(&mut self, title: &str, dict: &Dict) {
        let text = pretty::format_dict(dict);
        self.title = title.to_string();
        self.object = None;
        self.decoded = None;
        self.mode = InspectorMode::Pretty;
        self.scroll = 0;
        self.lines = text.lines().map(str::to_string).collect();
        self.refs = ref_lines(&text);
        self.ref_cursor = None;
        self.loading = false;
    }

    /// Whether the shown object is a stream (enables `d` cycling).
    pub fn is_stream(&self) -> bool {
        self.object
            .as_ref()
            .is_some_and(|(.., object)| object.as_stream().is_some())
    }

    /// Cycles Pretty → Raw → Decoded → Ops → Pretty on streams. Returns
    /// true when the new view needs decoded data not yet present.
    pub fn cycle_mode(&mut self) -> bool {
        if !self.is_stream() {
            return false;
        }
        self.mode = match self.mode {
            InspectorMode::Pretty => InspectorMode::Raw,
            InspectorMode::Raw => InspectorMode::Decoded,
            InspectorMode::Decoded => InspectorMode::Ops,
            InspectorMode::Ops => InspectorMode::Pretty,
        };
        self.scroll = 0;
        self.rebuild();
        matches!(self.mode, InspectorMode::Decoded | InspectorMode::Ops) && self.decoded.is_none()
    }

    /// Short mode name for the pane title.
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            InspectorMode::Pretty => "pretty",
            InspectorMode::Raw => "raw",
            InspectorMode::Decoded => "decoded",
            InspectorMode::Ops => "ops",
        }
    }

    /// Moves the ref cursor (Pretty view); scroll follows the cursor line.
    pub fn move_cursor(&mut self, delta: i32) {
        if self.refs.is_empty() {
            let next = i64::from(self.scroll) + i64::from(delta);
            self.scroll = next.clamp(0, self.lines.len().saturating_sub(1) as i64) as u16;
            return;
        }
        let last = self.refs.len() - 1;
        let next = match self.ref_cursor {
            None if delta > 0 => 0,
            None => return,
            Some(index) => {
                let moved = index as i64 + i64::from(delta);
                moved.clamp(0, last as i64) as usize
            }
        };
        self.ref_cursor = Some(next);
        let line = self.refs[next].0;
        self.scroll = line.saturating_sub(2) as u16;
    }

    /// The ref under the cursor, for Enter-to-jump.
    pub fn current_ref(&self) -> Option<ObjRef> {
        let index = self.ref_cursor?;
        Some(self.refs.get(index)?.1)
    }

    fn rebuild(&mut self) {
        let Some((.., object)) = self.object.as_ref() else {
            return;
        };
        match self.mode {
            InspectorMode::Pretty => {
                let text = pretty::format_object(object);
                self.lines = text.lines().map(str::to_string).collect();
                self.refs = ref_lines(&text);
            }
            InspectorMode::Raw => {
                let raw: &[u8] = match object.as_stream() {
                    Some(stream) => &stream.data,
                    None => &[],
                };
                self.lines = bytes_lines(raw);
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
            InspectorMode::Decoded => {
                self.lines = match self.decoded.as_deref() {
                    Some(data) => bytes_lines(data),
                    None => vec!["decoding\u{2026}".to_string()],
                };
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
            InspectorMode::Ops => {
                self.lines = match self.decoded.as_deref() {
                    Some(data) => ops_lines(data),
                    None => vec!["decoding\u{2026}".to_string()],
                };
                self.refs = Vec::new();
                self.ref_cursor = None;
            }
        }
    }
}

impl Default for InspectorState {
    fn default() -> InspectorState {
        InspectorState::new()
    }
}

/// Scans pretty-printed text for `N G R` reference tokens, returning
/// `(line index, ref)` pairs in display order.
pub fn ref_lines(text: &str) -> Vec<(usize, ObjRef)> {
    let mut found = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
            .filter(|token| !token.is_empty())
            .collect();
        for window in tokens.windows(3) {
            if window[2] != "R" {
                continue;
            }
            let (Ok(num), Ok(gen)) = (window[0].parse::<u32>(), window[1].parse::<u16>()) else {
                continue;
            };
            found.push((line_index, ObjRef { num, gen }));
        }
    }
    found
}

/// Byte views: split on newlines, map non-printable bytes to `·`, cap at
/// `MAX_BYTE_LINES` lines.
pub fn bytes_lines(data: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for segment in data.split(|byte| *byte == b'\n') {
        if lines.len() == MAX_BYTE_LINES {
            lines.push("\u{2026} (truncated)".to_string());
            return lines;
        }
        let text: String = segment
            .iter()
            .map(|byte| {
                if (0x20..=0x7e).contains(byte) {
                    char::from(*byte)
                } else {
                    '\u{b7}'
                }
            })
            .collect();
        lines.push(text);
    }
    lines
}

/// Disassembles decoded content-stream bytes, one operator per line.
pub fn ops_lines(data: &[u8]) -> Vec<String> {
    match parse_content(data) {
        Ok(ops) => ops.iter().map(|op| format!("{:?}", op)).collect(),
        Err(error) => vec![format!("content parse failed: {error}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{Dict, Name, ObjRef, Object, Stream};

    fn catalog() -> Object {
        let mut dict = Dict::new();
        dict.insert(
            Name("Type".to_string()),
            Object::Name(Name("Catalog".to_string())),
        );
        dict.insert(
            Name("Pages".to_string()),
            Object::Ref(ObjRef { num: 2, gen: 0 }),
        );
        Object::Dict(dict)
    }

    fn content_stream() -> Object {
        Object::Stream(Stream {
            dict: Dict::new(),
            data: b"raw-bytes".to_vec(),
        })
    }

    #[test]
    fn ref_lines_finds_references_with_line_numbers() {
        let text = "<<\n  /Pages 2 0 R\n  /Other [3 1 R 4 0 R]\n>>";
        assert_eq!(
            ref_lines(text),
            vec![
                (1, ObjRef { num: 2, gen: 0 }),
                (2, ObjRef { num: 3, gen: 1 }),
                (2, ObjRef { num: 4, gen: 0 }),
            ]
        );
        assert_eq!(ref_lines("no refs 12 here"), Vec::new());
    }

    #[test]
    fn set_object_builds_pretty_lines_and_refs() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 1, gen: 0 }, catalog());
        assert_eq!(inspector.title, "obj 1 0");
        assert_eq!(
            inspector.lines,
            vec!["<<", "  /Pages 2 0 R", "  /Type /Catalog", ">>"]
        );
        assert_eq!(inspector.refs, vec![(1, ObjRef { num: 2, gen: 0 })]);
        assert!(!inspector.is_stream());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn cursor_moves_over_refs_and_reports_current() {
        let mut inspector = InspectorState::new();
        let mut dict = Dict::new();
        dict.insert(
            Name("A".to_string()),
            Object::Ref(ObjRef { num: 7, gen: 0 }),
        );
        dict.insert(
            Name("B".to_string()),
            Object::Ref(ObjRef { num: 9, gen: 0 }),
        );
        inspector.set_object(ObjRef { num: 1, gen: 0 }, Object::Dict(dict));
        assert_eq!(inspector.current_ref(), None);
        inspector.move_cursor(1);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 7, gen: 0 }));
        inspector.move_cursor(1);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 9, gen: 0 }));
        inspector.move_cursor(1);
        assert_eq!(
            inspector.current_ref(),
            Some(ObjRef { num: 9, gen: 0 }),
            "clamped at last ref"
        );
        inspector.move_cursor(-5);
        assert_eq!(inspector.current_ref(), Some(ObjRef { num: 7, gen: 0 }));
    }

    #[test]
    fn cycle_mode_on_non_stream_stays_pretty() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 1, gen: 0 }, catalog());
        assert!(!inspector.cycle_mode());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn cycle_mode_walks_stream_views_and_requests_decode() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 4, gen: 0 }, content_stream());
        assert!(!inspector.cycle_mode(), "raw needs no decode");
        assert_eq!(inspector.mode, InspectorMode::Raw);
        assert_eq!(inspector.lines, vec!["raw-bytes"]);
        assert!(inspector.cycle_mode(), "decoded view needs data");
        assert_eq!(inspector.mode, InspectorMode::Decoded);
        assert_eq!(inspector.lines, vec!["decoding\u{2026}"]);
        inspector.set_decoded(ObjRef { num: 4, gen: 0 }, b"BT /F1 12 Tf ET".to_vec());
        assert_eq!(inspector.lines, vec!["BT /F1 12 Tf ET"]);
        assert!(!inspector.cycle_mode(), "ops reuses decoded data");
        assert_eq!(inspector.mode, InspectorMode::Ops);
        assert_eq!(
            inspector.lines,
            vec!["BeginText", "SetFont(Name(\"F1\"), 12.0)", "EndText"]
        );
        assert!(!inspector.cycle_mode());
        assert_eq!(inspector.mode, InspectorMode::Pretty);
    }

    #[test]
    fn stale_decoded_payload_is_ignored() {
        let mut inspector = InspectorState::new();
        inspector.set_object(ObjRef { num: 4, gen: 0 }, content_stream());
        inspector.cycle_mode();
        inspector.cycle_mode();
        inspector.set_decoded(ObjRef { num: 9, gen: 0 }, b"junk".to_vec());
        assert_eq!(
            inspector.lines,
            vec!["decoding\u{2026}"],
            "wrong object dropped"
        );
    }

    #[test]
    fn ops_lines_reports_parse_failure_inline() {
        // `pdfboss_core::content::parse_content` is lenient about most malformed
        // input (unknown operators and even an unterminated string literal are
        // dropped silently, never an error — see its module docs); the one
        // documented error path is operand nesting past `MAX_NESTING_DEPTH`
        // (128), which array-open tokens with no matching close trip.
        let deeply_nested = vec![b'['; 200];
        let lines = ops_lines(&deeply_nested);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("content parse failed: "));
    }

    #[test]
    fn bytes_lines_replaces_non_printable_and_caps_output() {
        assert_eq!(bytes_lines(b"ab\ncd"), vec!["ab", "cd"]);
        assert_eq!(bytes_lines(&[0x00, 0x41]), vec!["\u{b7}A"]);
        let big = vec![b'\n'; 2500];
        let lines = bytes_lines(&big);
        assert_eq!(lines.len(), 2001);
        assert_eq!(lines[2000], "\u{2026} (truncated)");
    }

    #[test]
    fn show_message_and_loading() {
        let mut inspector = InspectorState::new();
        inspector.show_message("Document", vec!["version: 1.7".to_string()]);
        assert_eq!(inspector.title, "Document");
        assert_eq!(inspector.lines, vec!["version: 1.7"]);
        assert!(inspector.refs.is_empty());
        inspector.show_loading("obj 3 0");
        assert!(inspector.loading);
        assert_eq!(inspector.lines, vec!["loading\u{2026}"]);
    }
}
