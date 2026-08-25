//! Layout analysis and output rendering for pdfboss: turns `pdfboss-text`
//! spans into a structured layout IR, and the IR into a document.

mod ir;
mod markdown;
mod output;
mod structure;

use pdfboss_core::{AsyncObjectSource, Document, Page, Result};

pub use ir::{BBox, Block, Cell, Inline, Line, ListItem, Marker, PageLayout, Role};
pub use markdown::Markdown;
pub use output::{Output, Text};
pub use pdfboss_text::{
    ExtractReport, FontCache, Ruling, SkipCause, SkippedText, SkippedTextKind, TextSpan,
};
pub use structure::{
    document_layout, document_layout_with_rulings, layout, page_layout, page_layout_with_rulings,
};

/// Extracts the page's text with positional layout applied: spans grouped
/// into lines, lines ordered top to bottom and joined with `\n`, spaces
/// inserted at horizontal gaps.
///
/// Lenient the way rendering is: content that will not fetch, decode, or
/// parse yields no text rather than an error, so one unreadable stream
/// never costs a caller the rest of the document. Use
/// [`extract_text_reporting`] to see what (if anything) was left out.
///
/// Optional-content layers the document's default configuration turns off
/// are excluded, exactly as `pdfboss_text`'s document-level entries exclude
/// them; the source-generic `_with` twins have no document to read that
/// configuration from and extract every layer.
pub fn extract_text(doc: &Document, page: &Page) -> Result<String> {
    let (text, _) = extract_text_reporting(doc, page)?;
    Ok(text)
}

/// [`extract_text`] against any object source, awaiting whatever I/O the
/// source needs to read the page — the same span extraction and layout,
/// minus the document's optional-content configuration, which a bare
/// source cannot supply: every layer extracts here.
///
/// The source is taken by value and the page by reference. That combination is
/// what a consumer needs to spawn the result: the future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the page.
/// See `pdfboss_core::source`'s "Signing a shared algorithm".
pub async fn extract_text_with<S: AsyncObjectSource>(src: S, page: &Page) -> Result<String> {
    let (text, _) = extract_text_reporting_with(src, page).await?;
    Ok(text)
}

/// [`extract_text`] with the report of what could not be read: an
/// [`ExtractReport`] whose entries name each skipped stream and why —
/// unsupported filters (the passthrough image codecs included), undecodable
/// bytes, unparseable content, missing resources, exhausted form limits.
/// An empty text with an empty report really is an empty page.
pub fn extract_text_reporting(doc: &Document, page: &Page) -> Result<(String, ExtractReport)> {
    let (spans, report) = pdfboss_text::extract_spans_reporting(doc, page)?;
    Ok((Text.render(&[page_layout(&spans)]), report))
}

/// [`extract_text_reporting`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons — every optional-content
/// layer included.
pub async fn extract_text_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<(String, ExtractReport)> {
    let (spans, report) = pdfboss_text::extract_spans_reporting_with(src, page).await?;
    Ok((Text.render(&[page_layout(&spans)]), report))
}

/// [`extract_text_reporting`] with fonts cached across pages: a caller
/// walking a whole document — `pdfboss_core::map_pages` included — passes one
/// [`FontCache`] to every page and each font loads once for the document.
/// The text is identical to the uncached call's, page for page.
///
/// There is no `_with` twin: an asynchronous caller composes
/// `pdfboss_text::extract_spans_reporting_cached_with` with the pure
/// [`page_layout`] and [`Text`], exactly as this function does.
pub fn extract_text_reporting_cached(
    doc: &Document,
    page: &Page,
    fonts: &FontCache,
) -> Result<(String, ExtractReport)> {
    let (spans, report) = pdfboss_text::extract_spans_reporting_cached(doc, page, fonts)?;
    Ok((Text.render(&[page_layout(&spans)]), report))
}

/// Extracts the whole document as Markdown: ATX headings, paragraphs, and
/// emphasis over the same positional layout [`extract_text`] renders flat.
///
/// Heading sizes are ranked against every page at once, so a title page or
/// a chapter opener — all of it larger than body text — is read as headings
/// rather than as its own idea of body size.
///
/// Lenient like [`extract_text`]: unreadable content costs its own text and
/// nothing else. Use [`extract_markdown_reporting`] to see what was left
/// out.
pub fn extract_markdown(doc: &Document) -> Result<String> {
    let (markdown, _) = extract_markdown_reporting(doc)?;
    Ok(markdown)
}

/// [`extract_markdown`] with one [`ExtractReport`] per page, in page order.
///
/// Each page's rulings ride along with its spans: a table whose structure is
/// drawn as borders is read from them ahead of lane occupancy.
pub fn extract_markdown_reporting(doc: &Document) -> Result<(String, Vec<ExtractReport>)> {
    let fonts = FontCache::default();
    let per_page = pdfboss_core::map_pages(doc, |doc: &Document, page: &Page| {
        pdfboss_text::extract_spans_and_rulings_reporting_cached(doc, page, &fonts)
    });
    let mut pages = Vec::with_capacity(per_page.len());
    let mut reports = Vec::with_capacity(per_page.len());
    for outcome in per_page {
        let (spans, rulings, report) = outcome?;
        pages.push((spans, rulings));
        reports.push(report);
    }
    Ok((
        Markdown.render(&document_layout_with_rulings(&pages)),
        reports,
    ))
}

/// One page as Markdown, ranking heading sizes against that page alone.
/// [`extract_markdown`] is the better answer whenever the document is at
/// hand — a page whose text is all one size has no heading to find.
pub fn extract_page_markdown(doc: &Document, page: &Page) -> Result<String> {
    let (spans, rulings, _) = pdfboss_text::extract_spans_and_rulings_reporting(doc, page)?;
    Ok(Markdown.render(&[page_layout_with_rulings(&spans, &rulings)]))
}

/// [`extract_page_markdown`] against any object source. Signed like
/// [`extract_text_with`], for the same reasons — every optional-content
/// layer included.
///
/// There is no document-level `_with`: an asynchronous caller collects each
/// page's spans and rulings with
/// `pdfboss_text::extract_spans_and_rulings_reporting_with` and then calls
/// the pure [`document_layout_with_rulings`] and [`Markdown`], which is the
/// same document-wide ranking without a second I/O path to keep in step.
pub async fn extract_page_markdown_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
) -> Result<String> {
    let (spans, rulings, _) =
        pdfboss_text::extract_spans_and_rulings_reporting_with(src, page).await?;
    Ok(Markdown.render(&[page_layout_with_rulings(&spans, &rulings)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::{block_on, resolve_with, BoxFuture, ObjRef, Object, Stream};
    use pdfboss_testkit::{doc_with_graphics, multi_page_doc, simple_doc, PdfBuilder};
    use std::future::Future;

    fn page_text(doc: &Document, index: usize) -> String {
        let page = doc.page(index).unwrap();
        extract_text(doc, &page).unwrap()
    }

    /// Non-whitespace token runs, counted — the content-preservation
    /// currency of the ruled oracle branch.
    fn token_counts(text: &str) -> std::collections::BTreeMap<&str, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for token in text.split_whitespace() {
            *counts.entry(token).or_default() += 1;
        }
        counts
    }

    /// The Text adapter over a ruling-free layout must reproduce the pre-IR
    /// string builder exactly — the local form of the corpus parity gate.
    /// [`structure::layout_reference`] is that builder, kept as the oracle.
    ///
    /// A ruling-fed layout genuinely reorders text — a merged logical row
    /// reads cell-major where the flat flow reads line-major — so the ruled
    /// branch asserts content preservation instead: the multiset of
    /// non-whitespace token runs equals the flat flow's, counted exactly.
    /// Loss, duplication, and fused tokens all break the count.
    #[test]
    fn text_adapter_matches_layout_on_fixtures() {
        let mut headings = 0usize;
        let mut splits = 0usize;
        let mut tables = 0usize;
        let mut ruled_tables = 0usize;
        for content in structure::tests::fixture_contents() {
            let doc = Document::load(doc_with_graphics(&content)).unwrap();
            let page = doc.page(0).unwrap();
            let (spans, rulings, report) =
                pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page).unwrap();
            assert!(report.is_complete(), "unexpected skips: {report:?}");
            let layout = page_layout_with_rulings(&spans, &rulings);
            headings += layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Heading { .. }))
                .count();
            let paragraphs = layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Paragraph { .. }))
                .count();
            splits += usize::from(paragraphs > 1);
            let fixture_tables = layout
                .blocks
                .iter()
                .filter(|block| matches!(block, Block::Table { .. }))
                .count();
            tables += fixture_tables;
            if !rulings.is_empty() {
                ruled_tables += fixture_tables;
            }
            let via_ir = Text.render(&[layout]);
            let flat = structure::layout_reference(&spans);
            if rulings.is_empty() {
                assert_eq!(via_ir, flat, "content: {content}");
            } else {
                assert_eq!(
                    token_counts(&via_ir),
                    token_counts(&flat),
                    "content: {content}\nvia IR: {via_ir}\nflat flow: {flat}"
                );
            }
        }
        // A fixture set that classifies nothing would pass this test without
        // ever reaching the code it guards.
        assert!(headings > 0, "no fixture produced a heading block");
        assert!(splits > 0, "no fixture split into several paragraphs");
        assert!(tables > 0, "no fixture produced a table block");
        assert!(ruled_tables > 0, "no fixture produced a ruled table block");
    }

    /// Markdown of a one-page document with `content` as its raw content
    /// stream, through document-level size statistics.
    fn markdown_of(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[spans]))
    }

    /// [`markdown_of`] on a page whose resources carry `/F1` Helvetica and
    /// `/F2` Helvetica-Bold, so a span's boldness comes from a real font.
    fn markdown_of_two_fonts(content: &str) -> String {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content.as_bytes());
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        Markdown.render(&document_layout(&[spans]))
    }

    /// Sizes rank into levels; body text stays a paragraph. 24pt > 16pt > 12pt
    /// body: `#` for 24, `##` for 16.
    #[test]
    fn heading_levels_by_size_rank() {
        let content = "BT /F1 24 Tf 72 740 Td (Title) Tj \
                       /F1 16 Tf 0 -40 Td (Section) Tj \
                       /F1 12 Tf 0 -30 Td (Body text long enough to look like body.) Tj \
                       0 -14 Td (More body keeps twelve the dominant size.) Tj \
                       0 -14 Td (And a third line for good measure.) Tj ET";
        let md = markdown_of(content);
        assert!(md.contains("# Title\n"), "md: {md}");
        assert!(md.contains("## Section\n"), "md: {md}");
        assert!(!md.contains("# Body"), "md: {md}");
    }

    /// Sizes past the sixth ladder rank clamp to `######` instead of falling
    /// out of the ladder: the buckets nearest body size are the real section
    /// headings, and one stray oversized logo must not evict them.
    #[test]
    fn ranks_past_six_clamp_to_level_six() {
        let heads = [36, 28, 24, 20, 18, 16, 14, 12, 11]
            .iter()
            .enumerate()
            .map(|(index, size)| {
                let y = 750 - 50 * index;
                format!("BT /F1 {size} Tf 72 {y} Td (Head {size}) Tj ET ")
            })
            .collect::<String>();
        let body = (0..3)
            .map(|index| {
                let y = 260 - 14 * index;
                format!(
                    "BT /F1 10 Tf 72 {y} Td (Body line {index} is long enough to be body.) Tj ET "
                )
            })
            .collect::<String>();
        let md = markdown_of(&format!("{heads}{body}"));
        assert!(md.starts_with("# Head 36"), "md: {md}");
        assert!(md.contains("###### Head 16"), "md: {md}");
        assert!(md.contains("###### Head 14"), "md: {md}");
        assert!(md.contains("###### Head 12"), "md: {md}");
        assert!(md.contains("###### Head 11"), "md: {md}");
    }

    /// A whitespace-only line at heading size is still classified as a
    /// heading; Markdown must not emit a bare `#` for it.
    #[test]
    fn blank_heading_line_emits_nothing() {
        let md = markdown_of(
            "BT /F1 24 Tf 72 740 Td (   ) Tj \
             /F1 12 Tf 0 -40 Td (Body line one is long enough to be body.) Tj \
             0 -14 Td (Body line two keeps twelve the dominant size.) Tj \
             0 -14 Td (And a third body line seals it.) Tj ET",
        );
        assert!(!md.contains('#'), "md: {md:?}");
    }

    /// Emphasis wraps maximal same-style runs, with the spaces left outside
    /// the markers.
    #[test]
    fn bold_run_renders_as_strong() {
        let md = markdown_of_two_fonts(
            "BT /F1 12 Tf 72 720 Td (plain ) Tj /F2 12 Tf (loud) Tj /F1 12 Tf ( tail) Tj \
             0 -14 Td (body body body body) Tj 0 -14 Td (body body body body) Tj ET",
        );
        assert!(md.contains("plain **loud** tail"), "md: {md}");
    }

    #[test]
    fn bullet_lines_become_list_items() {
        let content = "BT /F1 12 Tf 72 720 Td (\\225 first item) Tj \
                       0 -14 Td (\\225 second item) Tj \
                       0 -14 Td (Body sentence after the list ends here.) Tj ET";
        // \225 is bullet in WinAnsi. Fixture font must be WinAnsi-encoded.
        let md = markdown_of(content);
        assert!(md.contains("- first item\n- second item"), "md: {md}");
        assert!(!md.contains('\u{2022}'), "marker replaced, not kept: {md}");
    }

    #[test]
    fn numbered_items_keep_their_numbers() {
        let content = "BT /F1 12 Tf 72 720 Td (1. alpha) Tj 0 -14 Td (2. beta) Tj \
                       0 -14 Td (12) Tj ET";
        let md = markdown_of(content);
        assert!(md.contains("1. alpha\n2. beta"), "md: {md}");
        assert!(
            md.contains("12"),
            "a bare number line is not a list item: {md}"
        );
    }

    #[test]
    fn hanging_indent_continues_an_item() {
        // Continuation line starts right of the marker column.
        let content = "BT /F1 12 Tf 72 720 Td (\\225 a long item that) Tj \
                       10 -14 Td (wraps to a second line) Tj ET";
        let md = markdown_of(content);
        assert!(
            md.contains("- a long item that\nwraps to a second line")
                || md.contains("- a long item that wraps to a second line"),
            "md: {md}"
        );
    }

    /// Three lanes, four aligned rows -> one pipe table.
    #[test]
    fn lane_grid_becomes_pipe_table() {
        let md = markdown_of(&structure::tests::lane_grid_content());
        assert!(md.contains("| r0c0 | r0c1 | r0c2 |"), "md: {md}");
        assert!(
            md.contains("| --- | --- | --- |"),
            "separator after header: {md}"
        );
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
    }

    /// Page-edge lines sharing the band with a grid leave as prose: the
    /// running header does not take the header row's place — which, being
    /// wide enough to cross every lane, would also flip the block to the HTML
    /// dialect as a merged cell — and the page number is not a last row. A
    /// single-cell line between two rows is a wrapped cell and stays a row.
    #[test]
    fn page_edge_lines_around_the_grid_stay_prose() {
        let md = markdown_of(&structure::tests::grid_with_edge_lines_content());
        let header = structure::tests::RUNNING_HEADER;
        assert!(
            !md.contains("<table>"),
            "an edge line flipped the dialect: {md}"
        );
        assert!(
            md.contains(&format!(
                "{header}\n\n| r0c0 | r0c1 | r0c2 |\n| --- | --- | --- |"
            )),
            "md: {md}"
        );
        assert!(
            md.contains("| r1c0 | r1c1 | r1c2 |\n| wrapped cell |  |  |\n| r2c0 |"),
            "wrapped cell is a row: {md}"
        );
        assert!(md.contains("| r3c0 | r3c1 | r3c2 |"), "md: {md}");
        assert!(!md.contains("| 24 |"), "page number is not a row: {md}");
        assert!(md.ends_with("\n\n24"), "md: {md}");
    }

    /// A lane held open by a page number out in the margin is not a cell
    /// column: hoisting the number empties it, and two columns of rows are a
    /// layout. Modeled on a bench page whose two-column pitch read as a
    /// three-column table with an empty third cell in every row.
    #[test]
    fn a_margin_page_number_does_not_manufacture_a_column() {
        let md = markdown_of(&structure::tests::margin_number_grid_content());
        assert!(!md.contains('|'), "two columns are not a table: {md}");
        assert!(!md.contains("<table>"), "two columns are not a table: {md}");
        assert!(md.contains("r0c0 r0c1"), "rows still read as prose: {md}");
        assert!(md.ends_with("\n\n3"), "the page number survives: {md}");
    }

    /// A cell crossing the lane gap forces the HTML dialect with colspan.
    #[test]
    fn spanning_cell_switches_to_html_table() {
        // Same 4x3 grid as lane_grid_becomes_pipe_table, except row 0's first
        // cell is one long string whose advance (testkit default width 500 →
        // 5pt/char at 10pt) runs from x=72 past lane 1's start at x=250.
        let mut content = String::from(
            "BT /F1 10 Tf 1 0 0 1 72 700 Tm (a merged header cell spanning two lanes xx) Tj \
             1 0 0 1 430 700 Tm (r0c2) Tj ",
        );
        for (row, y) in [(1, 680.0), (2, 660.0), (3, 640.0)] {
            for (col, x) in [(0, 72.0), (1, 250.0), (2, 430.0)] {
                content += &format!("1 0 0 1 {x} {y} Tm (r{row}c{col}) Tj ");
            }
        }
        content += "ET";
        let md = markdown_of(&content);
        assert!(md.contains("<table>"), "md: {md}");
        assert!(md.contains("colspan=\"2\""), "md: {md}");
        assert!(!md.contains("| r1c0 |"), "one table, one dialect: {md}");
    }

    /// Markdown of a one-page document whose content stream draws rulings,
    /// through the real document entry point, which threads them.
    fn markdown_of_drawn(content: &str) -> String {
        let doc = Document::load(doc_with_graphics(content)).unwrap();
        extract_markdown(&doc).unwrap()
    }

    /// A drawn 2x2 grid leaves one lane, which the lane gates can never
    /// admit; the rulings alone make it a table.
    #[test]
    fn a_ruled_grid_becomes_a_pipe_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_grid_content());
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "md: {md}"
        );
    }

    /// The corpus failure the ruled path fixes: a single-column boxed list
    /// is a one-column table, which no lane-occupancy gate could ever admit.
    #[test]
    fn a_single_column_boxed_list_becomes_a_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_boxed_list_content());
        assert!(
            md.contains(
                "| first item |\n| --- |\n| second item |\n| third item |\n| fourth item |"
            ),
            "md: {md}"
        );
    }

    /// One ruled band holding three visual lines is one logical row: the
    /// wrapped cell's fragments join with single spaces and the other cells
    /// stay intact.
    #[test]
    fn a_wrapped_band_merges_into_one_logical_row() {
        let md = markdown_of_drawn(&structure::tests::ruled_wrapped_band_content());
        assert!(
            md.contains("| h1 | h2 | h3 |\n| --- | --- | --- |\n| m1 | m2 | m3 |"),
            "md: {md}"
        );
        assert!(
            md.contains("| wrap one wrap two wrap three | solo | tail |"),
            "the band's lines merge into one row: {md}"
        );
        assert!(
            !md.contains("| wrap two |"),
            "no fragmentary row survives: {md}"
        );
    }

    /// A grid ruled only on its interior boundaries: the header band above
    /// the top horizontal is claimed via the verticals' reach, the text
    /// overflowing the outer verticals opens a column on each side, and the
    /// rule-less data band's lines become one row per anchor line.
    #[test]
    fn an_open_edged_grid_becomes_a_full_table() {
        let md = markdown_of_drawn(&structure::tests::ruled_open_grid_content());
        assert!(
            md.contains(
                "| name | count | note |\n| --- | --- | --- |\n\
                 | alpha | one | xx |\n| beta | two | yy |\n\
                 | gamma | three | zz |\n| delta | four | ww |"
            ),
            "md: {md}"
        );
    }

    /// Records wrapping inside a rule-less band fold behind their anchor
    /// lines: the continuation populates no anchor cell, so it is the same
    /// row still being written, not a row of its own.
    #[test]
    fn wrapped_records_fold_behind_their_anchors() {
        let md = markdown_of_drawn(&structure::tests::ruled_wrapped_records_content());
        assert!(
            md.contains(
                "| name | org | count |\n| --- | --- | --- |\n\
                 | one | recordaa wrapa | c1 |\n| two | recordbb wrapb | c2 |"
            ),
            "md: {md}"
        );
    }

    /// A rule-less band whose first line populates a single cell holds one
    /// vertically centered record: it merges whole instead of shattering at
    /// its anchor column.
    #[test]
    fn a_centered_record_band_merges_whole() {
        let md = markdown_of_drawn(&structure::tests::ruled_centered_record_content());
        assert!(
            md.contains(
                "| name | org | count |\n| --- | --- | --- |\n\
                 | actlinea actlineb actlinec actlined | union | c9 |"
            ),
            "md: {md}"
        );
    }

    /// A drawn grid no longer claims its whole segment: the whitespace-laned
    /// rows below it still become a table of their own — exactly the table
    /// the lane path alone emits — and blocks stay in reading order.
    #[test]
    fn a_ruled_grid_and_a_lane_grid_share_a_segment() {
        let content = structure::tests::ruled_grid_above_lane_grid_content();
        let md = markdown_of_drawn(&content);
        assert!(
            md.contains("| a1 | b1 |\n| --- | --- |\n| a2 | b2 |"),
            "the drawn grid stays a table: {md}"
        );
        let lane_table = [
            "| r0c0 | r0c1 | r0c2 |",
            "| --- | --- | --- |",
            "| r1c0 | r1c1 | r1c2 |",
            "| r2c0 | r2c1 | r2c2 |",
            "| r3c0 | r3c1 | r3c2 |",
        ]
        .join("\n");
        assert!(
            md.contains(&lane_table),
            "the laned rows stay a table: {md}"
        );
        assert!(
            md.find("| a1 |").unwrap() < md.find("| r0c0 |").unwrap(),
            "reading order: {md}"
        );
        assert!(
            markdown_of(&content).contains(&lane_table),
            "the lane path alone emits the same table"
        );
    }

    /// A grid boundary inside a sub-word gap would split a word the flat
    /// flow writes whole: the grid is rejected and the segment stays prose.
    #[test]
    fn a_ruling_inside_a_sub_word_gap_rejects_the_grid() {
        let md = markdown_of_drawn(&structure::tests::ruled_sub_word_gap_content());
        assert!(!md.contains('|'), "no table: {md}");
        assert!(!md.contains("<table>"), "no table: {md}");
        assert!(md.contains("world"), "the word survives whole: {md}");
    }

    /// The spans-only entry points delegate with no rulings: a page whose
    /// only table is drawn stays prose through them, exactly as before.
    #[test]
    fn spans_only_layout_ignores_drawn_grids() {
        let doc = Document::load(doc_with_graphics(
            &structure::tests::ruled_boxed_list_content(),
        ))
        .unwrap();
        let page = doc.page(0).unwrap();
        let (spans, report) = pdfboss_text::extract_spans_reporting(&doc, &page).unwrap();
        assert!(report.is_complete(), "unexpected skips: {report:?}");
        let layout = page_layout(&spans);
        assert!(
            layout
                .blocks
                .iter()
                .all(|block| !matches!(block, Block::Table { .. })),
            "no rulings, no table: {layout:?}"
        );
    }

    /// A lone page of huge text must not become all headings under per-page
    /// stats when the document knows better.
    #[test]
    fn document_stats_beat_page_stats() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 5 0 R >>",
        );
        b.object(
            4,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>",
        );
        b.stream(
            5,
            "",
            b"BT /F1 12 Tf 72 720 Td (Body line one is long enough.) Tj \
              0 -14 Td (Body line two keeps twelve dominant.) Tj \
              0 -14 Td (Body line three seals it.) Tj ET",
        );
        b.stream(6, "", b"BT /F1 24 Tf 72 720 Td (Chapter Two) Tj ET");
        b.object(
            7,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let pages: Vec<Vec<TextSpan>> = (0..2)
            .map(|i| {
                let page = doc.page(i).unwrap();
                pdfboss_text::extract_spans_reporting(&doc, &page)
                    .unwrap()
                    .0
            })
            .collect();
        let md = Markdown.render(&document_layout(&pages));
        assert!(
            md.contains("# Chapter Two"),
            "doc stats make it a heading: {md}"
        );
        let alone = Markdown.render(&[page_layout(&pages[1])]);
        assert!(
            !alone.contains("# "),
            "page stats alone see 24pt as body: {alone}"
        );
    }

    /// A running title repeated at the top of every page and a page number at
    /// the bottom disappear from markdown but stay in text.
    #[test]
    fn running_headers_and_page_numbers_are_tagged() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>");
        for (page_obj, contents_obj) in [(3u32, 6u32), (4, 7), (5, 8)] {
            b.object(
                page_obj,
                &format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                     /Resources << /Font << /F1 9 0 R >> >> /Contents {contents_obj} 0 R >>"
                ),
            );
        }
        for (contents_obj, n) in [(6u32, 1u32), (7, 2), (8, 3)] {
            b.stream(
                contents_obj,
                "",
                format!(
                    "BT /F1 10 Tf 72 770 Td (ACME REPORT) Tj \
                     /F1 12 Tf 0 -50 Td (Page {n} body text differs everywhere.) Tj \
                     0 -14 Td (A second body line pads the page.) Tj \
                     /F1 10 Tf 200 -666 Td ({n}) Tj ET"
                )
                .as_bytes(),
            );
        }
        b.object(
            9,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let pages: Vec<Vec<TextSpan>> = (0..3)
            .map(|i| {
                let page = doc.page(i).unwrap();
                pdfboss_text::extract_spans_reporting(&doc, &page)
                    .unwrap()
                    .0
            })
            .collect();
        let layouts = document_layout(&pages);
        let md = Markdown.render(&layouts);
        assert!(!md.contains("ACME REPORT"), "md: {md}");
        assert!(!md.contains("\n1\n"), "page number dropped: {md}");
        assert!(md.contains("body text differs"), "body survives: {md}");
        let text = Text.render(&layouts);
        assert!(
            text.contains("ACME REPORT"),
            "text keeps everything: {text}"
        );
    }

    #[test]
    fn simple_doc_exact_text() {
        let doc = Document::load(simple_doc("Hello, world!")).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");
    }

    #[test]
    fn multi_page_doc_per_page() {
        let doc = Document::load(multi_page_doc(&["Page one", "Page two", "Page three"])).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(page_text(&doc, 0), "Page one");
        assert_eq!(page_text(&doc, 1), "Page two");
        assert_eq!(page_text(&doc, 2), "Page three");
    }

    #[test]
    fn differences_remap_in_extraction() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (AB) Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha] >> >>",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3B1}B");
    }

    #[test]
    fn type0_font_with_tounicode_stream() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td <00010001> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
             /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /DW 600 >>",
        );
        b.stream(
            7,
            "",
            b"1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
              1 beginbfchar <0001> <03A9> endbfchar",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3A9}\u{3A9}");
    }

    #[test]
    fn form_xobject_recursion() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (out) Tj ET /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // No own /Resources: falls back to the page's, so /F1 resolves.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Matrix [1 0 0 1 0 -20]",
            b"BT /F1 12 Tf 72 720 Td (in) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "out\nin");
    }

    /// A form XObject that carries its own `/Resources` **without** a `/Font`
    /// entry must still find the page's font. Resource lookup is a chain,
    /// innermost first with a per-name fallback (ISO 32000 §8.10.2 and
    /// §7.8.3) — not replace-or-inherit.
    ///
    /// `/Differences` is what makes the failure visible rather than silent:
    /// through the page's `/F1`, byte 65 decodes to alpha; through the
    /// fallback font it stays `"A"`. Without the chain the text is still
    /// extracted, just decoded with the wrong font, which is why no existing
    /// test caught this.
    #[test]
    fn form_with_partial_resources_still_sees_the_page_font() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Custom \
             /Encoding << /BaseEncoding /WinAnsiEncoding \
             /Differences [65 /alpha] >> >>",
        );
        // Own /Resources present, but it defines no /Font at all.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /ProcSet [/PDF /Text] >>",
            b"BT /F1 12 Tf 72 720 Td (A) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "\u{3B1}");
    }

    /// Hex-encodes `data` for an `/ASCIIHexDecode` stream — the benign
    /// trailing filter of the pass-through tests: refusal must be about the
    /// image codecs, not about `/Filter` being present at all.
    fn hex(data: &[u8]) -> Vec<u8> {
        data.iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .chain(*b">")
            .collect()
    }

    /// One page whose `/Contents` (object 4) carries `stream_dict` around
    /// `content`, with `/F1` a WinAnsi Helvetica.
    fn contents_doc(stream_dict: &str, content: &[u8]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, stream_dict, content);
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        Document::load(b.build(1)).unwrap()
    }

    /// A page `/Contents` whose trailing `/Filter` is an image codec is
    /// refused, and the refusal is a report entry, not an error: the page
    /// yields no text instead of costing a document-level caller every
    /// other page. The bytes are deliberately valid operators to prove the
    /// refusal happens on the label.
    #[test]
    fn image_codec_page_contents_yield_no_text_and_one_report_entry() {
        let doc = contents_doc(
            "/Filter /JPXDecode",
            b"BT /F1 12 Tf 72 720 Td (ghost) Tj ET",
        );
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "", "the passthrough bytes must not be parsed");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::PageContents,
                cause: SkipCause::UnsupportedFilter("JPXDecode".to_string()),
            }],
        );
        // The plain entry point is the same leniency without the report.
        assert_eq!(extract_text(&doc, &page).unwrap(), "");
    }

    /// The inverse: a benign trailing filter the decoder can run must keep
    /// decoding. Over-refusal here would silently drop the text of every
    /// compressed page while the suite stayed green.
    #[test]
    fn a_filtered_page_contents_still_extracts() {
        let doc = contents_doc(
            "/Filter /ASCIIHexDecode",
            &hex(b"BT /F1 12 Tf 72 720 Td (plain sight) Tj ET"),
        );
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "plain sight");
        assert!(report.is_complete(), "nothing was skipped: {report:?}");
    }

    /// A form XObject whose trailing `/Filter` is an image codec is refused
    /// with a report entry — the same accountable skip rendering records —
    /// while the rest of the page still extracts. Before the report channel
    /// existed this text vanished with zero signal.
    #[test]
    fn image_codec_form_content_is_refused_and_reported() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (kept) Tj ET /Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /DCTDecode",
            b"BT /F1 12 Tf 72 700 Td (ghost) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "kept", "the page's own text survives the refusal");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::Form,
                cause: SkipCause::UnsupportedFilter("DCTDecode".to_string()),
            }],
        );
    }

    /// The form-level inverse: a benign trailing filter on a form decodes
    /// and its text extracts, with a complete report.
    #[test]
    fn a_filtered_form_still_extracts() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /ASCIIHexDecode",
            &hex(b"BT /F1 12 Tf 72 720 Td (decoded) Tj ET"),
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "decoded");
        assert!(report.is_complete(), "nothing was skipped: {report:?}");
    }

    /// A self-invoking form recurses to the depth cap, and the cap is a
    /// report entry rather than a silent stop.
    #[test]
    fn exhausted_form_depth_is_reported() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // No own /Resources: the page's names itself, so each level invokes
        // the next until the depth cap bites at the innermost one.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (x) Tj ET /Fx Do",
        );
        let doc = Document::load(b.build(1)).unwrap();
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert!(!text.is_empty(), "the levels above the cap still extract");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::Form,
                cause: SkipCause::LimitExceeded,
            }],
        );
    }

    /// A `Do` whose name resolves to nothing usable is reported: whether it
    /// held text cannot be known, so a complete report must not pretend so.
    #[test]
    fn a_missing_xobject_is_reported() {
        let doc = contents_doc("", b"BT /F1 12 Tf 72 720 Td (here) Tj ET /Nope Do");
        let page = doc.page(0).unwrap();
        let (text, report) = extract_text_reporting(&doc, &page).unwrap();
        assert_eq!(text, "here");
        assert_eq!(
            report.skipped,
            vec![SkippedText {
                kind: SkippedTextKind::XObject,
                cause: SkipCause::Missing,
            }],
        );
    }

    /// A conforming file may make any dictionary value indirect (ISO 32000-1
    /// 7.3.8.1), `/Subtype` included. The form dispatch resolves it rather
    /// than requiring a direct name — a form declared through a reference
    /// used to be dropped as "not a form", burning its invocation-budget
    /// slot and silently losing its whole text subtree.
    #[test]
    fn a_form_whose_subtype_is_indirect_still_extracts() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fx 6 0 R >> >> \
             /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Fx Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        b.stream(
            6,
            "/Type /XObject /Subtype 8 0 R /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (via ref) Tj ET",
        );
        b.object(8, "/Form");
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "via ref");
    }

    /// The same chain rule for a nested form: an inner form named only in the
    /// page's `/XObject` must be reachable from a form that has its own
    /// `/Resources` without an `/XObject` entry.
    #[test]
    fn form_with_partial_resources_still_sees_the_page_xobject() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Outer 6 0 R /Inner 7 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/Outer Do");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        );
        // Outer has its own /Resources naming neither /Inner nor /Font.
        b.stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
             /Resources << /ProcSet [/PDF /Text] >>",
            b"/Inner Do",
        );
        b.stream(
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 612 792]",
            b"BT /F1 12 Tf 72 720 Td (deep) Tj ET",
        );
        let doc = Document::load(b.build(1)).unwrap();
        assert_eq!(page_text(&doc, 0), "deep");
    }

    /// An asynchronous source that answers everything with `null`.
    ///
    /// The heap field is load-bearing rather than decorative. rustc const-promotes
    /// a reference to a unit struct to `&'static`, so a unit stub would satisfy
    /// the `'static` assertion below even under a signature that assertion exists
    /// to reject — a test that cannot fail. A `Vec` cannot be promoted.
    ///
    /// It is also deliberately `Send + Sync`. The helpers inside the shared
    /// implementation borrow the source across their awaits, so the owning future
    /// is `Send` only when the source is `Sync`; every genuinely asynchronous
    /// source already is, because `resolve_with` requires it.
    struct NullSource {
        payload: Vec<u8>,
    }

    impl AsyncObjectSource for NullSource {
        fn get(&self, _r: ObjRef) -> BoxFuture<'_, Result<Object>> {
            Box::pin(std::future::ready(Ok(Object::Null)))
        }

        fn stream_data<'a>(&'a self, _s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(std::future::ready(Ok(self.payload.clone())))
        }

        fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
            Box::pin(resolve_with(self, o))
        }
    }

    /// The asynchronous entry point must produce a future a runtime's `spawn`
    /// and the Python bindings will accept, which means `Send + 'static`.
    ///
    /// The `async move` block is the shape a consumer actually writes: it owns
    /// the source and the page, and the borrow of the page that
    /// `extract_text_with` takes is created inside it. That is what makes the
    /// future `'static` despite the `&Page` parameter — and asserting it here also
    /// pins `Page: Send + Sync`, since the block holds one across its awaits.
    ///
    /// Every other test in this crate now drives this same implementation through
    /// `block_on`, so behaviour is covered by the exact-string assertions above.
    /// What none of them can see is this type, which is the entire point of the
    /// exercise. The document is dropped first to show the page stands alone.
    #[test]
    fn the_async_entry_point_yields_a_spawnable_future() {
        fn assert_send_static<F: Future + Send + 'static>(_: &F) {}

        let doc = Document::load(simple_doc("Hello")).unwrap();
        let text_page = doc.page(0).unwrap();
        drop(doc);

        let text = async move {
            extract_text_with(
                NullSource {
                    payload: Vec::new(),
                },
                &text_page,
            )
            .await
        };
        assert_send_static(&text);

        // A source that resolves everything to null yields a page with no
        // contents, so driving this only proves the wiring is reachable.
        assert_eq!(block_on(text).unwrap(), "");
    }

    #[test]
    fn committed_fixture_files() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");
        let hello = std::fs::read(format!("{dir}/hello.pdf")).unwrap();
        let doc = Document::load(hello).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");

        let three = std::fs::read(format!("{dir}/three-pages.pdf")).unwrap();
        let doc = Document::load(three).unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(page_text(&doc, 0), "Page one");
        assert_eq!(page_text(&doc, 1), "Page two");
        assert_eq!(page_text(&doc, 2), "Page three");

        let xs = std::fs::read(format!("{dir}/xref-stream.pdf")).unwrap();
        let doc = Document::load(xs).unwrap();
        assert_eq!(page_text(&doc, 0), "Hello, world!");
    }
}
