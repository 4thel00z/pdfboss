//! Page labels (ISO 32000-1 §12.4.2): the labelling ranges of the catalog's
//! `/PageLabels` number tree, and the label each page shows.

use crate::object::{decode_text_string, Dict};
use crate::source::AsyncObjectSource;
use crate::tree::{self, resolved_dict};

/// A page-numbering style, the `/S` of a page label dictionary (Table 159).
///
/// Covers ISO 32000-1 §12.4.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// Arabic numerals: 1, 2, 3.
    Decimal,
    /// Uppercase Roman numerals: I, II, III.
    RomanUpper,
    /// Lowercase Roman numerals: i, ii, iii.
    RomanLower,
    /// Uppercase letters: A to Z, then AA to ZZ, and so on.
    LettersUpper,
    /// Lowercase letters: a to z, then aa to zz, and so on.
    LettersLower,
}

impl LabelStyle {
    /// The style's `/S` name.
    pub fn code(self) -> &'static str {
        match self {
            LabelStyle::Decimal => "D",
            LabelStyle::RomanUpper => "R",
            LabelStyle::RomanLower => "r",
            LabelStyle::LettersUpper => "A",
            LabelStyle::LettersLower => "a",
        }
    }

    /// The style an `/S` name selects; `None` for any other name.
    pub fn from_code(code: &str) -> Option<LabelStyle> {
        Some(match code {
            "D" => LabelStyle::Decimal,
            "R" => LabelStyle::RomanUpper,
            "r" => LabelStyle::RomanLower,
            "A" => LabelStyle::LettersUpper,
            "a" => LabelStyle::LettersLower,
            _ => return None,
        })
    }

    /// The numeric portion of a label for the number `n`, which starts at
    /// 1: Roman numerals in subtractive form, letters as one letter for 1
    /// to 26 and the same letter repeated once more for every further 26.
    /// The number 0 has no Roman or letter form and gives an empty string.
    ///
    /// Covers ISO 32000-1 §12.4.2.
    pub fn numeral(self, n: u32) -> String {
        match self {
            LabelStyle::Decimal => n.to_string(),
            LabelStyle::RomanUpper => roman(n),
            LabelStyle::RomanLower => roman(n).to_lowercase(),
            LabelStyle::LettersUpper => letters(n, b'A'),
            LabelStyle::LettersLower => letters(n, b'a'),
        }
    }
}

/// `n` as an uppercase Roman numeral in subtractive form; thousands past
/// 3999 keep adding M. Empty for 0.
fn roman(mut n: u32) -> String {
    const STEPS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, numeral) in STEPS {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

/// `n` as letters: the letter at position `(n - 1) % 26` from `first`,
/// repeated once for every 26 the number spans. Empty for 0.
fn letters(n: u32, first: u8) -> String {
    if n == 0 {
        return String::new();
    }
    let letter = char::from(first + ((n - 1) % 26) as u8);
    let count = (n - 1) / 26 + 1;
    std::iter::repeat_n(letter, count as usize).collect()
}

/// One labelling range: every page from `first_page` up to the next
/// range's first page shows `prefix` followed by the numeral of
/// `start_at + (page - first_page)` in `style`; without a style the label is
/// the prefix alone.
///
/// Covers ISO 32000-1 §12.4.2.
#[derive(Debug, Clone, PartialEq)]
pub struct PageLabel {
    /// 0-based page index where this range begins.
    pub first_page: usize,
    /// Numbering style, the `/S` entry; `None` shows only `prefix`.
    pub style: Option<LabelStyle>,
    /// Text before every number in the range, the `/P` entry.
    pub prefix: Option<String>,
    /// The number shown on `first_page`, the `/St` entry; 1 by default.
    pub start_at: u32,
}

impl PageLabel {
    /// The label of the page at `index`, which must lie in this range.
    ///
    /// Covers ISO 32000-1 §12.4.2.
    pub fn label(&self, index: usize) -> String {
        let mut label = self.prefix.clone().unwrap_or_default();
        if let Some(style) = self.style {
            let offset = u32::try_from(index.saturating_sub(self.first_page)).unwrap_or(u32::MAX);
            label.push_str(&style.numeral(self.start_at.saturating_add(offset)));
        }
        label
    }
}

/// The document's labelling ranges from the catalog's `/PageLabels` number
/// tree, sorted by first page; `None` when the catalog has no such tree.
/// A range whose value is not a dictionary is skipped, an unknown `/S`
/// reads as no style, and a `/St` below 1 reads as 1.
///
/// Covers ISO 32000-1 §12.4.2.
pub async fn page_labels_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<Vec<PageLabel>> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let root = resolved_dict(src, catalog.get("PageLabels")?).await?;
    let mut ranges = Vec::new();
    for (first_page, value) in tree::entries::<i64, S>(src, &root).await {
        let Ok(first_page) = usize::try_from(first_page) else {
            continue;
        };
        let Some(dict) = resolved_dict(src, &value).await else {
            continue;
        };
        ranges.push(PageLabel {
            first_page,
            style: match dict.get("S") {
                Some(s) => src
                    .resolve(s)
                    .await
                    .ok()
                    .and_then(|s| LabelStyle::from_code(&s.as_name()?.0)),
                None => None,
            },
            prefix: match dict.get("P") {
                Some(p) => src
                    .resolve(p)
                    .await
                    .ok()
                    .and_then(|p| p.as_str_bytes().map(decode_text_string)),
                None => None,
            },
            start_at: match dict.get("St") {
                Some(st) => src
                    .resolve(st)
                    .await
                    .ok()
                    .and_then(|st| st.as_int())
                    .and_then(|st| u32::try_from(st).ok())
                    .filter(|st| *st >= 1)
                    .unwrap_or(1),
                None => 1,
            },
        });
    }
    ranges.sort_by_key(|range| range.first_page);
    Some(ranges)
}

/// The label of the page at `index` under `ranges`: the last range starting
/// at or before it. A page before the first range, which the clause does
/// not allow, shows its 1-based page number, as viewers do.
///
/// Covers ISO 32000-1 §12.4.2.
pub fn page_label(ranges: &[PageLabel], index: usize) -> String {
    match ranges.iter().rev().find(|range| range.first_page <= index) {
        Some(range) => range.label(index),
        None => (index + 1).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    // Covers ISO 32000-1 §12.4.2.
    #[test]
    fn styles_number_pages_as_table_159_says() {
        assert_eq!(LabelStyle::Decimal.numeral(1), "1");
        assert_eq!(LabelStyle::Decimal.numeral(1040), "1040");
        for (n, roman) in [
            (1, "i"),
            (4, "iv"),
            (9, "ix"),
            (14, "xiv"),
            (40, "xl"),
            (90, "xc"),
            (400, "cd"),
            (1994, "mcmxciv"),
            (3999, "mmmcmxcix"),
            (4000, "mmmm"),
        ] {
            assert_eq!(LabelStyle::RomanLower.numeral(n), roman, "{n}");
            assert_eq!(
                LabelStyle::RomanUpper.numeral(n),
                roman.to_uppercase(),
                "{n}"
            );
        }
        for (n, letters) in [(1, "a"), (26, "z"), (27, "aa"), (52, "zz"), (53, "aaa")] {
            assert_eq!(LabelStyle::LettersLower.numeral(n), letters, "{n}");
            assert_eq!(
                LabelStyle::LettersUpper.numeral(n),
                letters.to_uppercase(),
                "{n}"
            );
        }
        assert_eq!(LabelStyle::RomanLower.numeral(0), "");
        assert_eq!(LabelStyle::LettersUpper.numeral(0), "");
        assert_eq!(LabelStyle::Decimal.numeral(0), "0");
        for style in [
            LabelStyle::Decimal,
            LabelStyle::RomanUpper,
            LabelStyle::RomanLower,
            LabelStyle::LettersUpper,
            LabelStyle::LettersLower,
        ] {
            assert_eq!(LabelStyle::from_code(style.code()), Some(style));
        }
        assert_eq!(LabelStyle::from_code("X"), None);
    }

    /// `pages` pages whose catalog carries `catalog_extra`.
    fn doc(pages: usize, catalog_extra: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 10 + i)).collect();
        b.object(
            2,
            &format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                kids.join(" ")
            ),
        );
        for i in 0..pages {
            b.object(
                10 + i as u32,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            );
        }
        Document::load(b.build(1)).expect("load")
    }

    fn labels(doc: &Document) -> Vec<String> {
        (0..doc.page_count())
            .map(|i| doc.page_label(i).unwrap())
            .collect()
    }

    // Covers ISO 32000-1 §12.4.2.
    #[test]
    fn the_clauses_example_labels_its_pages() {
        // The example of 12.4.2: roman front matter, decimal body, and an
        // appendix prefixed A- starting at 8.
        let doc = doc(
            9,
            "/PageLabels << /Nums [0 << /S /r >> 4 << /S /D >> 7 << /S /D /P (A-) /St 8 >>] >>",
        );
        assert_eq!(
            labels(&doc),
            ["i", "ii", "iii", "iv", "1", "2", "3", "A-8", "A-9"]
        );
        let ranges = doc.page_labels().unwrap();
        assert_eq!(ranges.len(), 3);
        assert_eq!(
            ranges[2],
            PageLabel {
                first_page: 7,
                style: Some(LabelStyle::Decimal),
                prefix: Some("A-".to_string()),
                start_at: 8,
            }
        );
        assert_eq!(doc.page_label(9), None, "past the last page");
    }

    // Covers ISO 32000-1 §12.4.2.
    #[test]
    fn ranges_without_a_style_or_a_page_zero_still_label() {
        // No /S: the prefix alone. The tree starts at page 1, so page 0
        // (which the clause requires a range for) shows its page number. A
        // /St of 0 counts as 1, an unknown /S as no style, a UTF-16 prefix
        // decodes, and a non-dictionary value is skipped.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R /PageLabels 30 0 R >>");
        b.object(
            2,
            "<< /Type /Pages /Kids [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R] /Count 5 >>",
        );
        for i in 10..15 {
            b.object(i, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        }
        b.object(
            30,
            "<< /Nums [1 << /P (Cover) >> 2 << /S /D /St 0 >> 3 << /S /Q /P <FEFF00A7> >> 4 (junk)] >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(labels(&doc), ["1", "Cover", "1", "§", "§"]);
        // No /PageLabels at all: no labels, rather than page numbers.
        let plain = self::doc(2, "");
        assert_eq!(plain.page_labels(), None);
        assert_eq!(plain.page_label(0), None);
    }
}
