//! Layout engine: turns a parsed Markdown block tree into positioned draw
//! items, paginated to the theme's page size.

use std::path::Path;

use pdfboss_style::{Align, Decoration, Element, TextStyle, Theme};
use pdfboss_write::{Color, ImageData, PageSize, Standard14};

use crate::block::{Block, ListItem, Run};
use crate::report::{sanitize, Report};
use crate::table::place_table;
use crate::wrap::{wrap, Frag, LineBox, StyledRun};
use crate::Error;

/// Fraction of a line's max glyph size the baseline sits below the
/// line-box top. Shared by table cell layout.
pub(crate) const BASELINE: f32 = 0.8;

/// Left indent added per list-nesting level, in points.
const GUTTER: f32 = 18.0;

/// One positioned draw primitive on a page.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    /// A run of text set in one font, size and color.
    Text {
        x: f32,
        y: f32,
        text: String,
        font: Standard14,
        size: f32,
        color: Color,
    },
    /// A filled rectangle.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
    },
    /// A single straight line.
    Stroke {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Color,
    },
    /// A stroked (unfilled) rectangle.
    Frame {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        width: f32,
        color: Color,
    },
    /// An embedded raster or passthrough image.
    #[allow(dead_code)]
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        data: ImageData,
    },
    /// A clickable link rectangle, `y` at the bottom.
    Link {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        uri: String,
    },
}

/// One paginated page's worth of positioned draw items.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct LaidPage {
    pub items: Vec<Item>,
}

/// One page-spanning region opened by [`Engine::begin_span`] and closed by
/// [`Engine::end_span`], broken into per-page pieces at every page break.
pub(crate) struct Segment {
    pub page: usize,
    pub top: f32,
    pub bottom: f32,
    pub item_index: usize,
}

/// An open span being tracked across page breaks.
struct Span {
    page: usize,
    top: f32,
    item_index: usize,
    closed: Vec<Segment>,
}

/// Lays out a parsed block tree into paginated draw items for `page_size`,
/// cascading styles from `theme` and resolving image paths against
/// `base_dir`.
#[allow(dead_code)]
pub(crate) fn layout(
    blocks: &[Block],
    theme: &Theme,
    page_size: PageSize,
    base_dir: &Path,
    report: &mut Report,
) -> Result<Vec<LaidPage>, Error> {
    let mut engine = Engine::new(theme, page_size);
    let base = theme.base();
    engine.blocks(blocks, &base, engine.left, engine.right, base_dir, report)?;
    Ok(engine.pages)
}

/// The paginating layout engine: tracks the current write position, page
/// margins and open spans while blocks are placed in order.
pub(crate) struct Engine<'a> {
    pub(crate) theme: &'a Theme,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    pages: Vec<LaidPage>,
    pub(crate) y: f32,
    pending: f32,
    pub(crate) at_top: bool,
    spans: Vec<Span>,
    depth: u32,
}

impl<'a> Engine<'a> {
    /// A fresh engine for `page_size`, positioned at the top of one empty
    /// page inside `theme`'s body margins.
    fn new(theme: &'a Theme, page_size: PageSize) -> Engine<'a> {
        let (width, height) = page_size.dimensions();
        let margin = theme.margin(Element::Body);
        let top = height - margin.top;
        Engine {
            theme,
            left: margin.left,
            right: width - margin.right,
            top,
            bottom: margin.bottom,
            pages: vec![LaidPage::default()],
            y: top,
            pending: 0.0,
            at_top: true,
            spans: Vec::new(),
            depth: 0,
        }
    }

    /// The current (last) page.
    fn page(&mut self) -> &mut LaidPage {
        self.pages
            .last_mut()
            .expect("engine always holds at least one page")
    }

    /// The current page's index.
    fn page_index(&self) -> usize {
        self.pages.len() - 1
    }

    /// Appends one draw item to the current page.
    pub(crate) fn push(&mut self, item: Item) {
        self.page().items.push(item);
    }

    /// Applies a block's leading margin: collapses with the still-pending
    /// bottom margin of the previous block, and is a no-op at the top of a
    /// page.
    pub(crate) fn gap(&mut self, top_margin: f32) {
        if self.at_top {
            self.pending = 0.0;
            return;
        }
        self.y -= self.pending.max(top_margin);
        self.pending = 0.0;
    }

    /// Records a block's trailing margin as pending, to collapse with the
    /// next block's leading margin.
    pub(crate) fn after(&mut self, bottom_margin: f32) {
        self.pending = self.pending.max(bottom_margin);
        self.at_top = false;
    }

    /// Closes every open span's current page-piece, starts a fresh page,
    /// and resets the write position to its top.
    fn break_page(&mut self) {
        let page = self.pages.len();
        for span in &mut self.spans {
            span.closed.push(Segment {
                page: span.page,
                top: span.top,
                bottom: self.bottom,
                item_index: span.item_index,
            });
            span.top = self.top;
            span.item_index = 0;
            span.page = page;
        }
        self.pages.push(LaidPage::default());
        self.y = self.top;
        self.at_top = true;
        self.pending = 0.0;
    }

    /// Breaks to a new page first if placing `h` more points of content
    /// would run past the bottom margin. Never breaks at the top of an
    /// already-empty page.
    pub(crate) fn need(&mut self, h: f32) {
        if self.at_top {
            return;
        }
        if self.y - h < self.bottom {
            self.break_page();
        }
    }

    /// Opens a new span at the current write position.
    fn begin_span(&mut self) {
        let page = self.page_index();
        let item_index = self.page().items.len();
        self.spans.push(Span {
            page,
            top: self.y,
            item_index,
            closed: Vec::new(),
        });
    }

    /// Closes the innermost open span, returning every page-piece it spans,
    /// including the one still open at the current write position.
    fn end_span(&mut self) -> Vec<Segment> {
        let Some(span) = self.spans.pop() else {
            return Vec::new();
        };
        let mut closed = span.closed;
        closed.push(Segment {
            page: span.page,
            top: span.top,
            bottom: self.y,
            item_index: span.item_index,
        });
        closed
    }

    /// Places already-wrapped lines top-down from the current write
    /// position, breaking pages as needed, and advances past them.
    fn place_lines(&mut self, lines: Vec<LineBox>, style: &TextStyle, left: f32, right: f32) {
        for line in lines {
            let h = style.line_height * line.max_size;
            self.need(h);
            let baseline = self.y - BASELINE * line.max_size;
            let origin = match style.align {
                Align::Left => left,
                Align::Center => left + (right - left - line.width) / 2.0,
                Align::Right => right - line.width,
            };
            for item in frag_items(&line.frags, origin, baseline) {
                self.push(item);
            }
            self.y -= h;
            self.at_top = false;
        }
    }

    /// Dispatches each block in document order, appending items to the
    /// current page. `Paragraph`, `Heading`, `Rule`, `List`, `BlockQuote`,
    /// `CodeBlock` and `Table` land here; `Image` lands in a later task.
    fn blocks(
        &mut self,
        blocks: &[Block],
        base: &TextStyle,
        left: f32,
        right: f32,
        base_dir: &Path,
        report: &mut Report,
    ) -> Result<(), Error> {
        for block in blocks {
            match block {
                Block::Paragraph { runs } => {
                    let style = base.apply(self.theme.declared(Element::P));
                    let margin = self.theme.margin(Element::P);
                    self.gap(margin.top);
                    let styled = styled_runs(self.theme, runs, &style, report);
                    let lines = wrap(&styled, right - left, style.size)?;
                    self.place_lines(lines, &style, left, right);
                    self.after(margin.bottom);
                }
                Block::Heading { level, runs } => {
                    let element = heading_element(*level);
                    let style = base.apply(self.theme.declared(element));
                    let margin = self.theme.margin(element);
                    self.gap(margin.top);
                    let styled = styled_runs(self.theme, runs, &style, report);
                    let lines = wrap(&styled, right - left, style.size)?;
                    let total: f32 = lines
                        .iter()
                        .map(|line| style.line_height * line.max_size)
                        .sum();
                    let follow = margin.bottom + base.line_height * base.size;
                    if total + follow <= self.top - self.bottom {
                        self.need(total + follow);
                    }
                    self.place_lines(lines, &style, left, right);
                    self.after(margin.bottom);
                }
                Block::Rule => {
                    let margin = self.theme.margin(Element::Hr);
                    self.gap(margin.top);
                    let style = base.apply(self.theme.declared(Element::Hr));
                    self.need(1.0);
                    self.push(Item::Stroke {
                        x1: left,
                        y1: self.y - 0.5,
                        x2: right,
                        y2: self.y - 0.5,
                        width: 0.75,
                        color: style.color,
                    });
                    self.y -= 1.0;
                    self.at_top = false;
                    self.after(margin.bottom);
                }
                Block::List { start, items } => {
                    self.list(*start, items, base, left, right, base_dir, report)?;
                }
                Block::BlockQuote { blocks } => {
                    self.quote(blocks, base, left, right, base_dir, report)?;
                }
                Block::CodeBlock { text } => {
                    self.code_block(text, base, left, right, report)?;
                }
                Block::Table { aligns, head, rows } => {
                    place_table(self, aligns, head, rows, base, left, right, report)?;
                }
                Block::Image { .. } => {}
            }
        }
        Ok(())
    }

    /// Lays out a `List` block: a marker or task checkbox to the left of
    /// each item, then the item's own blocks recursively laid out one
    /// `GUTTER` further in.
    #[allow(clippy::too_many_arguments)]
    fn list(
        &mut self,
        start: Option<u64>,
        items: &[ListItem],
        base: &TextStyle,
        left: f32,
        right: f32,
        base_dir: &Path,
        report: &mut Report,
    ) -> Result<(), Error> {
        let element = if start.is_some() {
            Element::Ol
        } else {
            Element::Ul
        };
        let margin = self.theme.margin(element);
        self.gap(margin.top);
        let depth = self.depth;
        let li_margin = self.theme.margin(Element::Li);
        for (i, item) in items.iter().enumerate() {
            let style = base.apply(self.theme.declared(Element::Li));
            self.gap(li_margin.top);
            let baseline = self.y - BASELINE * style.size;
            match item.task {
                Some(checked) => self.checkbox(checked, &style, left, baseline),
                None => self.marker(start, i, depth, &style, left, baseline)?,
            }
            self.depth = depth + 1;
            self.blocks(&item.blocks, &style, left + GUTTER, right, base_dir, report)?;
            self.depth = depth;
            self.after(li_margin.bottom);
        }
        self.after(margin.bottom);
        Ok(())
    }

    /// Paints an ordered numeral (`start` given, counting from it) or an
    /// unordered bullet (dot at `depth` zero, dash when nested), right-
    /// aligned to end `4pt` left of the item's text edge.
    fn marker(
        &mut self,
        start: Option<u64>,
        index: usize,
        depth: u32,
        style: &TextStyle,
        left: f32,
        baseline: f32,
    ) -> Result<(), Error> {
        let text = match start {
            Some(start_value) => format!("{}.", start_value + index as u64),
            None if depth == 0 => "\u{2022}".to_string(),
            None => "\u{2013}".to_string(),
        };
        let width = style.font().text_width(&text, style.size)?;
        self.push(Item::Text {
            x: left + GUTTER - 4.0 - width,
            y: baseline,
            text,
            font: style.font(),
            size: style.size,
            color: style.color,
        });
        Ok(())
    }

    /// Paints a task item's checkbox: a stroked square, with two strokes
    /// drawing a check mark inside it when `checked`.
    fn checkbox(&mut self, checked: bool, style: &TextStyle, left: f32, baseline: f32) {
        let side = 0.66 * style.size;
        let x = left + GUTTER - 4.0 - side;
        let y = baseline;
        self.push(Item::Frame {
            x,
            y,
            w: side,
            h: side,
            width: 0.9,
            color: style.color,
        });
        if !checked {
            return;
        }
        self.push(Item::Stroke {
            x1: x + 0.2 * side,
            y1: y + 0.45 * side,
            x2: x + 0.42 * side,
            y2: y + 0.2 * side,
            width: 0.9,
            color: style.color,
        });
        self.push(Item::Stroke {
            x1: x + 0.42 * side,
            y1: y + 0.2 * side,
            x2: x + 0.85 * side,
            y2: y + 0.8 * side,
            width: 0.9,
            color: style.color,
        });
    }

    /// Lays out a `BlockQuote` block: its inner blocks indented by the
    /// blockquote's left margin, then a vertical bar painted beside each
    /// page-piece the quote spans.
    fn quote(
        &mut self,
        blocks: &[Block],
        base: &TextStyle,
        left: f32,
        right: f32,
        base_dir: &Path,
        report: &mut Report,
    ) -> Result<(), Error> {
        let style = base.apply(self.theme.declared(Element::Blockquote));
        let margin = self.theme.margin(Element::Blockquote);
        self.gap(margin.top);
        self.begin_span();
        self.blocks(
            blocks,
            &style,
            left + margin.left,
            right - margin.right,
            base_dir,
            report,
        )?;
        for segment in self.end_span().into_iter().rev() {
            let item = Item::Rect {
                x: left + margin.left - 8.0,
                y: segment.bottom,
                w: 2.0,
                h: segment.top - segment.bottom,
                color: style.color,
            };
            self.pages[segment.page]
                .items
                .insert(segment.item_index, item);
        }
        self.after(margin.bottom);
        Ok(())
    }

    /// Lays out a `CodeBlock` block: preformatted lines set in the code
    /// font and broken hard at character boundaries, with the element's
    /// background painted behind each page-piece the block spans.
    fn code_block(
        &mut self,
        text: &str,
        base: &TextStyle,
        left: f32,
        right: f32,
        report: &mut Report,
    ) -> Result<(), Error> {
        let style = base.apply(self.theme.declared(Element::Pre));
        let margin = self.theme.margin(Element::Pre);
        let padding = self.theme.padding(Element::Pre);
        let background = self.theme.background(Element::Pre);
        self.gap(margin.top);
        let inner_width = (right - left) - padding.left - padding.right;
        let h = style.line_height * style.size;
        self.need(h + padding.top);
        self.begin_span();
        self.y -= padding.top;
        for line in text.split('\n') {
            let expanded = line.replace('\t', "    ");
            let clean = sanitize(&expanded, style.font(), report);
            for row in char_rows(&clean, style.font(), style.size, inner_width)? {
                self.need(h);
                let baseline = self.y - BASELINE * style.size;
                if !row.is_empty() {
                    self.push(Item::Text {
                        x: left + padding.left,
                        y: baseline,
                        text: row,
                        font: style.font(),
                        size: style.size,
                        color: style.color,
                    });
                }
                self.y -= h;
                self.at_top = false;
            }
        }
        self.y -= padding.bottom;
        let segments = self.end_span();
        if let Some(color) = background {
            for segment in segments.into_iter().rev() {
                let item = Item::Rect {
                    x: left,
                    y: segment.bottom,
                    w: right - left,
                    h: segment.top - segment.bottom,
                    color,
                };
                self.pages[segment.page]
                    .items
                    .insert(segment.item_index, item);
            }
        }
        self.after(margin.bottom);
        Ok(())
    }
}

/// A link rect folded across consecutive same-URI fragments on one line,
/// widened as later fragments extend it, and flushed to one `Item::Link`
/// once the run of matching fragments ends.
struct OpenLink {
    uri: String,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl OpenLink {
    fn item(self) -> Item {
        Item::Link {
            x: self.x1,
            y: self.y1,
            w: self.x2 - self.x1,
            h: self.y2 - self.y1,
            uri: self.uri,
        }
    }
}

/// Emits the background/text/decoration/link items for one line's
/// fragments, `origin` being the line's left edge and `baseline` its text
/// baseline. Consecutive fragments sharing a link URI fold into one
/// `Item::Link` rect. Shared by [`Engine::place_lines`] and table cell
/// line placement, since table rows place lines manually rather than
/// through `place_lines` (which page-breaks).
pub(crate) fn frag_items(frags: &[Frag], origin: f32, baseline: f32) -> Vec<Item> {
    let mut items = Vec::new();
    let mut open_link: Option<OpenLink> = None;
    for frag in frags {
        let x = origin + frag.dx;
        if let Some(color) = frag.background {
            items.push(Item::Rect {
                x,
                y: baseline - 0.25 * frag.size,
                w: frag.width,
                h: 1.10 * frag.size,
                color,
            });
        }
        items.push(Item::Text {
            x,
            y: baseline,
            text: frag.text.clone(),
            font: frag.font,
            size: frag.size,
            color: frag.color,
        });
        match frag.decoration {
            Decoration::Underline => items.push(Item::Stroke {
                x1: x,
                y1: baseline - 0.10 * frag.size,
                x2: x + frag.width,
                y2: baseline - 0.10 * frag.size,
                width: 0.05 * frag.size,
                color: frag.color,
            }),
            Decoration::LineThrough => items.push(Item::Stroke {
                x1: x,
                y1: baseline + 0.25 * frag.size,
                x2: x + frag.width,
                y2: baseline + 0.25 * frag.size,
                width: 0.05 * frag.size,
                color: frag.color,
            }),
            Decoration::None => {}
        }
        let bottom = baseline - 0.25 * frag.size;
        let top = baseline + 0.85 * frag.size;
        let Some(uri) = &frag.link else {
            if let Some(open) = open_link.take() {
                items.push(open.item());
            }
            continue;
        };
        match &mut open_link {
            Some(open) if open.uri == *uri => {
                open.x2 = x + frag.width;
                open.y1 = open.y1.min(bottom);
                open.y2 = open.y2.max(top);
            }
            _ => {
                if let Some(old) = open_link.take() {
                    items.push(old.item());
                }
                open_link = Some(OpenLink {
                    uri: uri.clone(),
                    x1: x,
                    x2: x + frag.width,
                    y1: bottom,
                    y2: top,
                });
            }
        }
    }
    if let Some(open) = open_link.take() {
        items.push(open.item());
    }
    items
}

/// Resolves a run's declared styling onto `base`: theme rules for code,
/// link, and strikethrough runs, then explicit bold/italic flags.
fn run_style(theme: &Theme, base: &TextStyle, run: &Run) -> TextStyle {
    let mut style = *base;
    if run.code {
        style = style.apply(theme.declared(Element::Code));
    }
    if run.link.is_some() {
        style = style.apply(theme.declared(Element::A));
    }
    if run.strike {
        style = style.apply(theme.declared(Element::Del));
    }
    if run.bold {
        style.bold = true;
    }
    if run.italic {
        style.italic = true;
    }
    style
}

/// Resolves and sanitizes a paragraph, heading, or table cell's inline
/// runs into wrap-ready styled runs.
pub(crate) fn styled_runs(
    theme: &Theme,
    runs: &[Run],
    base: &TextStyle,
    report: &mut Report,
) -> Vec<StyledRun> {
    runs.iter()
        .map(|run| {
            let style = run_style(theme, base, run);
            StyledRun {
                text: sanitize(&run.text, style.font(), report),
                font: style.font(),
                size: style.size,
                color: style.color,
                background: run.code.then(|| theme.background(Element::Code)).flatten(),
                decoration: style.decoration,
                link: run.link.clone(),
            }
        })
        .collect()
}

/// Greedily splits `text` into rows no wider than `max_width`, breaking at
/// character boundaries only (no word-boundary preference). Empty text
/// yields one empty row so a blank code line keeps its vertical space.
fn char_rows(
    text: &str,
    font: Standard14,
    size: f32,
    max_width: f32,
) -> Result<Vec<String>, pdfboss_write::Error> {
    if text.is_empty() {
        return Ok(vec![String::new()]);
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0.0f32;
    for ch in text.chars() {
        let mut buffer = [0u8; 4];
        let piece = ch.encode_utf8(&mut buffer);
        let width = font.text_width(piece, size)?;
        if !row.is_empty() && row_width + width > max_width {
            rows.push(std::mem::take(&mut row));
            row_width = 0.0;
        }
        row.push(ch);
        row_width += width;
    }
    rows.push(row);
    Ok(rows)
}

/// The heading element for a level 1-6 (anything outside that range
/// clamps to `H6`).
fn heading_element(level: u8) -> Element {
    match level {
        1 => Element::H1,
        2 => Element::H2,
        3 => Element::H3,
        4 => Element::H4,
        5 => Element::H5,
        _ => Element::H6,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pdfboss_style::Theme;

    use super::*;
    use crate::block::parse_blocks;
    use crate::report::Report;

    fn laid(md: &str, css: &str) -> Vec<LaidPage> {
        let theme = Theme::parse(css).unwrap();
        let (blocks, _) = parse_blocks(md);
        let mut report = Report::default();
        layout(&blocks, &theme, PageSize::A4, Path::new("."), &mut report).unwrap()
    }

    const MONO: &str =
        "body { font-family: courier; font-size: 10pt; line-height: 1.0; margin: 100pt; }";

    #[test]
    fn paragraph_lines_stack_top_down() {
        let pages = laid("hello world\n", MONO);
        assert_eq!(pages.len(), 1);
        let texts: Vec<(&str, f32, f32)> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, x, y, .. } => Some((text.as_str(), *x, *y)),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec![("hello world", 100.0, 741.89 - 8.0)]);
    }

    #[test]
    fn page_breaks_when_the_column_is_full() {
        let many_lines = "word\n\n".repeat(80);
        let pages = laid(&many_lines, MONO);
        assert!(pages.len() > 1);
        let first_page_min_y = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { y, .. } => Some(*y),
                _ => None,
            })
            .fold(f32::MAX, f32::min);
        assert!(
            first_page_min_y >= 100.0,
            "text stays above the bottom margin: {first_page_min_y}"
        );
    }

    #[test]
    fn heading_never_ends_a_page() {
        let mut md = "filler\n\n".repeat(200);
        md.push_str("# Trailing heading\n\nbody after\n");
        let pages = laid(&md, MONO);
        let on_page_with_heading: Vec<&LaidPage> = pages
            .iter()
            .filter(|p| {
                p.items.iter().any(
                    |item| matches!(item, Item::Text { text, .. } if text.contains("Trailing")),
                )
            })
            .collect();
        let heading_page = on_page_with_heading[0];
        assert!(
            heading_page
                .items
                .iter()
                .any(|item| matches!(item, Item::Text { text, .. } if text.contains("body after"))),
            "the heading brought its following line along"
        );
    }

    #[test]
    fn heading_keep_reserves_its_bottom_margin() {
        let mut md = "filler\n\n".repeat(33);
        md.push_str("# Trailing heading\n\nbody after\n");
        let pages = laid(&md, MONO);
        let heading_page = pages
            .iter()
            .position(|p| {
                p.items.iter().any(
                    |item| matches!(item, Item::Text { text, .. } if text.contains("Trailing")),
                )
            })
            .unwrap();
        assert!(pages[heading_page]
            .items
            .iter()
            .any(|item| matches!(item, Item::Text { text, .. } if text.contains("body after"))));
        assert!(!pages[heading_page]
            .items
            .iter()
            .any(|item| matches!(item, Item::Text { text, .. } if text == "filler")));
    }

    #[test]
    fn margins_collapse_between_blocks() {
        let pages = laid(
            "first\n\nsecond\n",
            "body { font-family: courier; font-size: 10pt; line-height: 1.0; margin: 100pt; } \
             p { margin-top: 10pt; margin-bottom: 30pt; }",
        );
        let ys: Vec<f32> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { y, .. } => Some(*y),
                _ => None,
            })
            .collect();
        assert_eq!(ys[0] - ys[1], 40.0, "collapsed max(30,10) + line height 10");
    }

    #[test]
    fn alignment_centers_and_right_aligns() {
        let pages = laid(
            "mid\n",
            "body { font-family: courier; font-size: 10pt; margin: 100pt; text-align: center; }",
        );
        let Item::Text { x, .. } = pages[0]
            .items
            .iter()
            .find(|i| matches!(i, Item::Text { .. }))
            .unwrap()
        else {
            panic!()
        };
        let width = 3.0 * 6.0;
        let avail = 595.28 - 200.0;
        assert!((x - (100.0 + (avail - width) / 2.0)).abs() < 0.01);
    }

    #[test]
    fn links_underline_and_emit_rects() {
        let pages = laid("[docs](https://x.y)\n", MONO);
        assert!(pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Link { uri, .. } if uri == "https://x.y")));
        assert!(
            pages[0]
                .items
                .iter()
                .any(|i| matches!(i, Item::Stroke { .. })),
            "underline stroked"
        );
    }

    #[test]
    fn bullets_indent_and_mark() {
        let pages = laid("- one\n- two\n  - inner\n", MONO);
        let texts: Vec<(String, f32)> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, x, .. } => Some((text.clone(), *x)),
                _ => None,
            })
            .collect();
        assert!(texts
            .iter()
            .any(|(t, x)| t == "one" && (*x - 118.0).abs() < 0.01));
        assert!(texts
            .iter()
            .any(|(t, x)| t == "inner" && (*x - 136.0).abs() < 0.01));
        assert!(
            texts.iter().any(|(t, _)| t == "\u{2022}"),
            "bullet marker painted"
        );
    }

    #[test]
    fn ordered_markers_count_from_start() {
        let pages = laid("3. three\n4. four\n", MONO);
        let markers: Vec<String> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } if text.ends_with('.') => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["3.", "4."]);
    }

    #[test]
    fn task_items_draw_checkboxes() {
        let pages = laid("- [ ] open\n- [x] done\n", MONO);
        let frames = pages[0]
            .items
            .iter()
            .filter(|i| matches!(i, Item::Frame { .. }))
            .count();
        let checks = pages[0]
            .items
            .iter()
            .filter(|i| matches!(i, Item::Stroke { .. }))
            .count();
        assert_eq!(frames, 2);
        assert_eq!(checks, 2, "two strokes draw one check mark");
    }

    #[test]
    fn blockquote_draws_a_bar_and_indents() {
        let pages = laid("> quoted text\n", MONO);
        assert!(pages[0].items.iter().any(
            |i| matches!(i, Item::Rect { x, w, .. } if (*w - 2.0).abs() < 0.01 && *x < 124.0)
        ));
        assert!(pages[0]
            .items
            .iter()
            .any(|i| matches!(i, Item::Text { x, .. } if (*x - 124.0).abs() < 0.01)));
    }

    #[test]
    fn code_block_paints_background_and_preserves_lines() {
        let pages = laid("```\nfn main() {\n    body();\n}\n```\n", MONO);
        let texts: Vec<String> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["fn main() {", "    body();", "}"]);
        assert!(
            pages[0]
                .items
                .iter()
                .any(|i| matches!(i, Item::Rect { w, .. } if *w > 300.0)),
            "full-width background"
        );
        assert!(
            matches!(pages[0].items[0], Item::Rect { .. }),
            "background painted beneath the text"
        );
    }

    #[test]
    fn overlong_code_lines_break_at_characters_not_words() {
        let long = format!("```\n{}\n```\n", "x".repeat(200));
        let pages = laid(&long, MONO);
        let texts: Vec<String> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.len() >= 2);
        assert!(texts[0].chars().all(|c| c == 'x'));
    }

    #[test]
    fn code_block_splits_across_pages_with_backgrounds_on_both() {
        let long = format!("```\n{}```\n", "line\n".repeat(120));
        let pages = laid(&long, MONO);
        assert!(pages.len() >= 2);
        for page in &pages[..2] {
            assert!(page.items.iter().any(|i| matches!(i, Item::Rect { .. })));
        }
    }

    #[test]
    fn code_block_tabs_become_four_spaces() {
        let pages = laid("```\n\tbody();\n```\n", MONO);
        let texts: Vec<String> = pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["    body();"]);
    }
}
