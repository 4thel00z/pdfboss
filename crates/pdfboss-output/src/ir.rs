//! The layout intermediate representation: what the structure pass builds
//! from spans and what every output adapter renders.

use serde::Serialize;

/// A device-space box: `y` grows upward, as in PDF user space.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// A run of same-styled text within a line. `text` already carries the
/// spaces the word-gap rule inserted, so rendering a line is concatenation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inline {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

/// One visual line. The geometry travels with it because later structure
/// passes — lists, tables, page headers and footers — classify lines by it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Line {
    pub inlines: Vec<Inline>,
    /// Baseline of the line's first span.
    pub y: f32,
    /// Left edge: the leftmost span's origin.
    pub x: f32,
    /// Right edge: the rightmost span's end, after its last glyph's advance.
    pub end_x: f32,
    /// The largest font size on the line.
    pub size: f32,
}

/// What introduces a list item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Marker {
    Bullet,
    Number(u32),
}

/// One list item: its marker, the marker text's length in characters (the
/// continuation indent a wrapped item is measured against), and its lines.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListItem {
    pub marker: Marker,
    pub marker_len: usize,
    pub lines: Vec<Line>,
}

/// One table cell. An empty cell — or one covered by a neighbour's span —
/// carries no line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cell {
    pub line: Option<Line>,
    pub colspan: u8,
    pub rowspan: u8,
}

/// What a paragraph is to the page: its body, or a page header or footer repeated on
/// every page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Role {
    Body,
    PageHeader,
    PageFooter,
}

/// One structural unit of a page, in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Block {
    Heading {
        level: u8,
        lines: Vec<Line>,
        bbox: BBox,
    },
    Paragraph {
        lines: Vec<Line>,
        bbox: BBox,
        role: Role,
    },
    List {
        items: Vec<ListItem>,
        bbox: BBox,
    },
    Table {
        rows: Vec<Vec<Cell>>,
        bbox: BBox,
    },
}

/// One page's blocks in reading order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageLayout {
    pub blocks: Vec<Block>,
}
