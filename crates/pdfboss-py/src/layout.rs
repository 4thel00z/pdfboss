//! A page's layout as Python sees it: the headings, paragraphs, lists and
//! tables the Markdown output is rendered from, each with its box in PDF
//! user space, and the thresholds that cut them as keyword arguments.

use pyo3::exceptions::{PyIndexError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use pdfboss_output::{
    BBox, Block, Cell, LayoutOptions, Line, ListItem, Output, PageLayout, Pitch, Role, Text,
};

use crate::{normalize_index, repr_str};

type Bounds = (f32, f32, f32, f32);

fn bounds(bbox: &BBox) -> Bounds {
    (bbox.x0, bbox.y0, bbox.x1, bbox.y1)
}

fn line_text(line: &Line) -> String {
    line.inlines
        .iter()
        .map(|inline| inline.text.as_str())
        .collect::<String>()
        .trim()
        .to_string()
}

fn pitch_from_str(value: &str) -> PyResult<Pitch> {
    match value {
        "median" => Ok(Pitch::Median),
        "lower_half_median" => Ok(Pitch::LowerHalfMedian),
        other => Err(PyValueError::new_err(format!(
            "paragraph_pitch must be \"median\" or \"lower_half_median\", got {other:?}"
        ))),
    }
}

/// The [`LayoutOptions`] the keyword arguments set over the defaults. An
/// unknown keyword is a `TypeError`, as for any Python function.
pub(crate) fn layout_options(knobs: Option<&Bound<'_, PyDict>>) -> PyResult<LayoutOptions> {
    let mut options = LayoutOptions::default();
    let Some(knobs) = knobs else {
        return Ok(options);
    };
    for (key, value) in knobs.iter() {
        let key: String = key.extract()?;
        match key.as_str() {
            "line_overlap" => options.line_overlap = value.extract()?,
            "paragraph_gap" => options.paragraph_gap = value.extract()?,
            "paragraph_pitch" => options.paragraph_pitch = pitch_from_str(value.extract()?)?,
            "min_pitch_steps" => options.min_pitch_steps = value.extract()?,
            "flow_step_up" => options.flow_step_up = value.extract()?,
            "flow_line_up" => options.flow_line_up = value.extract()?,
            "flow_step_aside" => options.flow_step_aside = value.extract()?,
            "heading_min_delta" => options.heading_min_delta = value.extract()?,
            "heading_max_chars" => options.heading_max_chars = value.extract()?,
            "bold_heading_max_chars" => options.bold_heading_max_chars = value.extract()?,
            "heading_merge_step" => options.heading_merge_step = value.extract()?,
            "list_min_lines" => options.list_min_lines = value.extract()?,
            "list_continuation_indent" => options.list_continuation_indent = value.extract()?,
            "page_title_min_gap" => options.page_title_min_gap = value.extract()?,
            "header_footer_min_pages" => options.header_footer_min_pages = value.extract()?,
            other => {
                return Err(PyTypeError::new_err(format!(
                    "layout() got an unexpected keyword argument {other:?}"
                )))
            }
        }
    }
    Ok(options)
}

/// One cell of a layout table: its text and how many columns and rows it
/// spans. A covered or empty cell has empty text.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct LayoutCell {
    /// The cell's text; empty for an empty cell.
    #[pyo3(get)]
    text: String,
    /// Columns the cell spans.
    #[pyo3(get)]
    colspan: u8,
    /// Rows the cell spans.
    #[pyo3(get)]
    rowspan: u8,
}

#[pymethods]
impl LayoutCell {
    fn __repr__(&self) -> String {
        format!(
            "LayoutCell(text={}, colspan={}, rowspan={})",
            repr_str(&self.text),
            self.colspan,
            self.rowspan
        )
    }
}

impl LayoutCell {
    fn of(cell: &Cell) -> LayoutCell {
        LayoutCell {
            text: cell.line.as_ref().map(line_text).unwrap_or_default(),
            colspan: cell.colspan,
            rowspan: cell.rowspan,
        }
    }
}

/// One item of a layout list: the marker as written (`"-"`, `"1."`), the
/// item's text after it, and the box of the item's lines.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct LayoutListItem {
    /// The marker as the page writes it.
    #[pyo3(get)]
    marker: String,
    /// The item's text, marker removed, lines joined by `"\n"`.
    #[pyo3(get)]
    text: String,
    /// `(x0, y0, x1, y1)` in PDF user space, `y` growing upward.
    #[pyo3(get)]
    bbox: Bounds,
}

#[pymethods]
impl LayoutListItem {
    fn __repr__(&self) -> String {
        format!(
            "LayoutListItem(marker={}, text={}, bbox={:?})",
            repr_str(&self.marker),
            repr_str(&self.text),
            self.bbox
        )
    }
}

impl LayoutListItem {
    fn of(item: &ListItem) -> LayoutListItem {
        let first: String = item
            .lines
            .first()
            .map(|line| {
                line.inlines
                    .iter()
                    .map(|inline| inline.text.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let lines = item.lines.iter().skip(1).map(line_text);
        let marker: String = first.chars().take(item.marker_len).collect();
        let rest: String = first.chars().skip(item.marker_len).collect();
        let text: Vec<String> = std::iter::once(rest.trim().to_string())
            .chain(lines)
            .collect();
        LayoutListItem {
            marker: marker.trim().to_string(),
            text: text.join("\n"),
            bbox: bounds(&item.bbox),
        }
    }
}

/// One block of a page's layout, in reading order: a `"heading"` with its
/// `level`, a `"paragraph"` with its `role` (`"body"`, `"quote"`,
/// `"page_header"`, `"page_footer"`), a `"list"` with its `items`, or a
/// `"table"` with its `rows`.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct LayoutBlock {
    /// `"heading"`, `"paragraph"`, `"list"` or `"table"`.
    #[pyo3(get)]
    kind: &'static str,
    /// A heading's level, 1 to 6; `None` for other blocks.
    #[pyo3(get)]
    level: Option<u8>,
    /// A paragraph's role; `None` for other blocks.
    #[pyo3(get)]
    role: Option<&'static str>,
    /// `(x0, y0, x1, y1)` in PDF user space, `y` growing upward, like
    /// `Span.bbox`.
    #[pyo3(get)]
    bbox: Bounds,
    /// The block as plain text, as `extract_text` writes it.
    #[pyo3(get)]
    text: String,
    /// A list's items; empty for other blocks.
    #[pyo3(get)]
    items: Vec<LayoutListItem>,
    /// A table's rows of cells; empty for other blocks.
    #[pyo3(get)]
    rows: Vec<Vec<LayoutCell>>,
}

#[pymethods]
impl LayoutBlock {
    fn __repr__(&self) -> String {
        format!(
            "LayoutBlock(kind={}, bbox={:?}, text={})",
            repr_str(self.kind),
            self.bbox,
            repr_str(&self.text)
        )
    }
}

fn role_str(role: &Role) -> &'static str {
    match role {
        Role::Body => "body",
        Role::Quote => "quote",
        Role::PageHeader => "page_header",
        Role::PageFooter => "page_footer",
    }
}

impl LayoutBlock {
    fn of(block: &Block) -> LayoutBlock {
        let text = Text.render(&[PageLayout {
            blocks: vec![block.clone()],
        }]);
        let (kind, level, role, bbox, items, rows) = match block {
            Block::Heading { level, bbox, .. } => {
                ("heading", Some(*level), None, bbox, Vec::new(), Vec::new())
            }
            Block::Paragraph { role, bbox, .. } => (
                "paragraph",
                None,
                Some(role_str(role)),
                bbox,
                Vec::new(),
                Vec::new(),
            ),
            Block::List { items, bbox } => (
                "list",
                None,
                None,
                bbox,
                items.iter().map(LayoutListItem::of).collect(),
                Vec::new(),
            ),
            Block::Table { rows, bbox } => (
                "table",
                None,
                None,
                bbox,
                Vec::new(),
                rows.iter()
                    .map(|row| row.iter().map(LayoutCell::of).collect())
                    .collect(),
            ),
        };
        LayoutBlock {
            kind,
            level,
            role,
            bbox: bounds(bbox),
            text: text.trim_end().to_string(),
            items,
            rows,
        }
    }
}

/// A page's layout as Python blocks.
pub(crate) fn blocks_of(layout: &PageLayout) -> Vec<LayoutBlock> {
    layout.blocks.iter().map(LayoutBlock::of).collect()
}

/// Every page's blocks, or only the pages `pages` names, in the order given;
/// negative indexes count from the end.
pub(crate) fn selected_pages(
    layouts: &[PageLayout],
    pages: Option<Vec<isize>>,
) -> PyResult<Vec<Vec<LayoutBlock>>> {
    let Some(pages) = pages else {
        return Ok(layouts.iter().map(blocks_of).collect());
    };
    pages
        .into_iter()
        .map(|index| {
            normalize_index(index, layouts.len())
                .map(|index| blocks_of(&layouts[index]))
                .ok_or_else(|| PyIndexError::new_err(format!("page index {index} out of range")))
        })
        .collect()
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<LayoutBlock>()?;
    module.add_class::<LayoutListItem>()?;
    module.add_class::<LayoutCell>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LayoutListItem;
    use pdfboss_output::{BBox, Inline, Line, ListItem, Marker};

    fn line(text: &str) -> Line {
        Line {
            inlines: vec![Inline {
                text: text.to_string(),
                bold: false,
                italic: false,
                code: false,
            }],
            y: 0.0,
            x: 0.0,
            end_x: 0.0,
            size: 10.0,
        }
    }

    #[test]
    fn an_indented_item_keeps_its_marker_and_text() {
        let item = ListItem {
            marker: Marker::Bullet,
            marker_len: 4,
            lines: vec![line("  - item"), line("   wrapped ")],
            bbox: BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
            },
        };
        let python = LayoutListItem::of(&item);
        assert_eq!(python.marker, "-");
        assert_eq!(python.text, "item\nwrapped");
    }
}
