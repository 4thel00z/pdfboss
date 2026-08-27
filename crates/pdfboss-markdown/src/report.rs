//! Character replacement and sanitizing report for Markdown rendering.
//! Tracks unencodable characters and HTML fragments skipped during composition.

use pdfboss_write::Standard14;
use std::collections::BTreeMap;

/// Report of characters replaced and HTML fragments skipped during sanitizing.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Characters that were unencodable and replaced with '?', with their counts.
    pub replaced: BTreeMap<char, u32>,
    /// Number of raw HTML fragments that were skipped.
    pub skipped_html: u32,
}

impl Report {
    /// Whether this report is empty (no replacements, no HTML skipped).
    pub fn is_empty(&self) -> bool {
        self.replaced.is_empty() && self.skipped_html == 0
    }

    /// A summary describing all replacements and skipped HTML, or an empty
    /// string if the report is empty. Deterministic order (BTreeMap).
    pub fn summary(&self) -> String {
        let has_replaced = !self.replaced.is_empty();
        let has_html = self.skipped_html > 0;

        if !has_replaced && !has_html {
            return String::new();
        }

        let mut parts = Vec::new();

        if has_replaced {
            let total: u32 = self.replaced.values().sum();
            let char_plural = if total == 1 {
                "character"
            } else {
                "characters"
            };
            let chars_str = self
                .replaced
                .iter()
                .map(|(ch, count)| format!("'{}'×{}", ch, count))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "replaced {} {} unavailable in the standard fonts: {}",
                total, char_plural, chars_str
            ));
        }

        if has_html {
            let fragment_plural = if self.skipped_html == 1 {
                "raw html fragment"
            } else {
                "raw html fragments"
            };
            parts.push(format!("skipped {} {}", self.skipped_html, fragment_plural));
        }

        parts.join("; ")
    }
}

/// Replaces characters that cannot be encoded in the given font with '?',
/// tallying them in the report. Newlines are preserved. All other unencodable
/// or width-unmeasurable characters are replaced.
#[allow(dead_code)]
pub(crate) fn sanitize(text: &str, font: Standard14, report: &mut Report) -> String {
    text.chars()
        .map(|ch| {
            if ch == '\n' {
                return ch;
            }
            let mut buffer = [0u8; 4];
            let encoded = font.encode(ch.encode_utf8(&mut buffer)).is_ok();
            if encoded && font.width(ch).is_some() {
                return ch;
            }
            *report.replaced.entry(ch).or_insert(0) += 1;
            '?'
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_unencodable_chars_and_tallies() {
        let mut report = Report::default();
        let clean = sanitize("ok 🎉🎉 中", Standard14::Helvetica, &mut report);
        assert_eq!(clean, "ok ?? ?");
        assert_eq!(report.replaced.get(&'🎉'), Some(&2));
        assert_eq!(report.replaced.get(&'中'), Some(&1));
    }

    #[test]
    fn newline_survives_as_the_hard_break_marker() {
        let mut report = Report::default();
        assert_eq!(sanitize("a\nb", Standard14::Courier, &mut report), "a\nb");
        assert!(report.is_empty());
    }

    #[test]
    fn summary_names_chars_counts_and_html() {
        let mut report = Report {
            skipped_html: 1,
            ..Report::default()
        };
        sanitize("中", Standard14::Helvetica, &mut report);
        let summary = report.summary();
        assert!(summary.contains("'中'"), "{summary}");
        assert!(summary.contains("1 raw html fragment"), "{summary}");
    }
}
