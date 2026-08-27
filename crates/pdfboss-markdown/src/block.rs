//! The Markdown block tree: a CommonMark+GFM document parsed into blocks
//! and inline runs, independent of any layout or wrapping concern.

#![allow(dead_code)]

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// A run of inline text sharing one set of styling flags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

/// A block-level element of a parsed document.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        runs: Vec<Run>,
    },
    Paragraph {
        runs: Vec<Run>,
    },
    CodeBlock {
        text: String,
    },
    BlockQuote {
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Table {
        aligns: Vec<CellAlign>,
        head: Vec<Vec<Run>>,
        rows: Vec<Vec<Vec<Run>>>,
    },
    Rule,
    Image {
        path: String,
    },
}

/// One item of a `Block::List`, with its own nested blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A table column's text alignment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellAlign {
    Default,
    Left,
    Center,
    Right,
}

/// Parse a CommonMark+GFM document into a block tree. The `u32` counts
/// raw-HTML fragments that were skipped rather than represented.
pub(crate) fn parse_blocks(md: &str) -> (Vec<Block>, u32) {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::new();
    for event in Parser::new_ext(md, options) {
        builder.event(event);
    }
    builder.finish()
}

#[derive(Default)]
struct Builder {
    stack: Vec<Vec<Block>>,
    runs: Vec<Run>,
    leaf: Leaf,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<String>,
    image_depth: u32,
    pending_images: Vec<String>,
    lists: Vec<ListContext>,
    tables: Vec<TableContext>,
    skipped_html: u32,
}

#[derive(Default, PartialEq)]
enum Leaf {
    #[default]
    None,
    Paragraph,
    Heading(u8),
    Code(String),
    Cell,
}

#[derive(Default)]
struct ListContext {
    start: Option<u64>,
    items: Vec<ListItem>,
    pending_task: Option<bool>,
}

#[derive(Default)]
struct TableContext {
    aligns: Vec<CellAlign>,
    head: Vec<Vec<Run>>,
    rows: Vec<Vec<Vec<Run>>>,
    row: Vec<Vec<Run>>,
    in_head: bool,
}

impl Builder {
    fn new() -> Builder {
        Builder {
            stack: vec![Vec::new()],
            ..Builder::default()
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Paragraph) => self.leaf = Leaf::Paragraph,
            Event::End(TagEnd::Paragraph) => self.flush_paragraph(),
            Event::Start(Tag::Heading { level, .. }) => {
                self.leaf = Leaf::Heading(heading_level(level))
            }
            Event::End(TagEnd::Heading(_)) => {
                let Leaf::Heading(level) = std::mem::take(&mut self.leaf) else {
                    return;
                };
                let runs = std::mem::take(&mut self.runs);
                self.push_block(Block::Heading { level, runs });
            }
            Event::Start(Tag::CodeBlock(_)) => self.leaf = Leaf::Code(String::new()),
            Event::End(TagEnd::CodeBlock) => {
                let Leaf::Code(text) = std::mem::take(&mut self.leaf) else {
                    return;
                };
                self.push_block(Block::CodeBlock {
                    text: text.trim_end_matches('\n').to_string(),
                });
            }
            Event::Start(Tag::BlockQuote(_)) => self.stack.push(Vec::new()),
            Event::End(TagEnd::BlockQuote(_)) => {
                let blocks = self.stack.pop().unwrap_or_default();
                self.push_block(Block::BlockQuote { blocks });
            }
            Event::Start(Tag::List(start)) => self.lists.push(ListContext {
                start,
                ..ListContext::default()
            }),
            Event::End(TagEnd::List(_)) => {
                let Some(list) = self.lists.pop() else { return };
                self.push_block(Block::List {
                    start: list.start,
                    items: list.items,
                });
            }
            Event::Start(Tag::Item) => self.stack.push(Vec::new()),
            Event::End(TagEnd::Item) => {
                self.flush_loose_runs();
                let blocks = self.stack.pop().unwrap_or_default();
                let Some(list) = self.lists.last_mut() else {
                    return;
                };
                list.items.push(ListItem {
                    task: list.pending_task.take(),
                    blocks,
                });
            }
            Event::TaskListMarker(checked) => {
                let Some(list) = self.lists.last_mut() else {
                    return;
                };
                list.pending_task = Some(checked);
            }
            Event::Start(Tag::Table(aligns)) => self.tables.push(TableContext {
                aligns: aligns.iter().map(cell_align).collect(),
                ..TableContext::default()
            }),
            Event::End(TagEnd::Table) => {
                let Some(table) = self.tables.pop() else {
                    return;
                };
                self.push_block(Block::Table {
                    aligns: table.aligns,
                    head: table.head,
                    rows: table.rows,
                });
            }
            Event::Start(Tag::TableHead) => self.set_in_head(true),
            Event::End(TagEnd::TableHead) => {
                let Some(table) = self.tables.last_mut() else {
                    return;
                };
                table.head = std::mem::take(&mut table.row);
                table.in_head = false;
            }
            Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableRow) => self.close_row(),
            Event::Start(Tag::TableCell) => self.leaf = Leaf::Cell,
            Event::End(TagEnd::TableCell) => {
                self.leaf = Leaf::None;
                let runs = std::mem::take(&mut self.runs);
                let Some(table) = self.tables.last_mut() else {
                    return;
                };
                table.row.push(runs);
            }
            Event::Start(Tag::Emphasis) => self.italic += 1,
            Event::End(TagEnd::Emphasis) => self.italic = self.italic.saturating_sub(1),
            Event::Start(Tag::Strong) => self.bold += 1,
            Event::End(TagEnd::Strong) => self.bold = self.bold.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => self.strike += 1,
            Event::End(TagEnd::Strikethrough) => self.strike = self.strike.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => self.links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                self.links.pop();
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.image_depth += 1;
                self.pending_images.push(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => self.image_depth = self.image_depth.saturating_sub(1),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => self.run(&text, true),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.text("\n"),
            Event::Rule => self.push_block(Block::Rule),
            Event::Html(_) | Event::InlineHtml(_) => self.skipped_html += 1,
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if self.image_depth > 0 {
            return;
        }
        if let Leaf::Code(buffer) = &mut self.leaf {
            buffer.push_str(text);
            return;
        }
        self.run(text, false);
    }

    fn run(&mut self, text: &str, code: bool) {
        self.runs.push(Run {
            text: text.to_string(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            code,
            strike: self.strike > 0,
            link: self.links.last().cloned(),
        });
    }

    fn push_block(&mut self, block: Block) {
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        frame.push(block);
    }

    fn flush_paragraph(&mut self) {
        self.leaf = Leaf::None;
        let runs = std::mem::take(&mut self.runs);
        let blank = runs.iter().all(|run| run.text.trim().is_empty());
        if blank && self.pending_images.len() == 1 {
            let Some(path) = self.pending_images.pop() else {
                return;
            };
            self.push_block(Block::Image { path });
            return;
        }
        if !runs.is_empty() {
            self.push_block(Block::Paragraph { runs });
        }
        let images = std::mem::take(&mut self.pending_images);
        for path in images {
            self.push_block(Block::Image { path });
        }
    }

    fn flush_loose_runs(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.runs);
        self.push_block(Block::Paragraph { runs });
    }

    fn close_row(&mut self) {
        let Some(table) = self.tables.last_mut() else {
            return;
        };
        if table.in_head {
            return;
        }
        let row = std::mem::take(&mut table.row);
        table.rows.push(row);
    }

    fn set_in_head(&mut self, in_head: bool) {
        let Some(table) = self.tables.last_mut() else {
            return;
        };
        table.in_head = in_head;
    }

    fn finish(mut self) -> (Vec<Block>, u32) {
        let blocks = self.stack.pop().unwrap_or_default();
        (blocks, self.skipped_html)
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn cell_align(alignment: &Alignment) -> CellAlign {
    match alignment {
        Alignment::None => CellAlign::Default,
        Alignment::Left => CellAlign::Left,
        Alignment::Center => CellAlign::Center,
        Alignment::Right => CellAlign::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Run {
        Run {
            text: text.to_string(),
            ..Run::default()
        }
    }

    #[test]
    fn heading_paragraph_and_emphasis() {
        let (blocks, skipped) = parse_blocks("# Title\n\nplain **bold** *italic* `code`\n");
        assert_eq!(skipped, 0);
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 1,
                runs: vec![plain("Title")]
            }
        );
        let Block::Paragraph { runs } = &blocks[1] else {
            panic!("expected paragraph")
        };
        assert_eq!(runs[0], plain("plain "));
        assert_eq!(
            runs[1],
            Run {
                text: "bold".into(),
                bold: true,
                ..Run::default()
            }
        );
        assert_eq!(
            runs[3],
            Run {
                text: "italic".into(),
                italic: true,
                ..Run::default()
            }
        );
        assert_eq!(
            runs[5],
            Run {
                text: "code".into(),
                code: true,
                ..Run::default()
            }
        );
    }

    #[test]
    fn links_strikethrough_and_hard_breaks() {
        let (blocks, _) = parse_blocks("[docs](https://x.y) and ~~gone~~ end  \nnext line\n");
        let Block::Paragraph { runs } = &blocks[0] else {
            panic!()
        };
        assert_eq!(runs[0].link.as_deref(), Some("https://x.y"));
        assert!(runs.iter().any(|r| r.strike && r.text == "gone"));
        assert!(
            runs.iter().any(|r| r.text == "\n"),
            "hard break becomes a newline run"
        );
    }

    #[test]
    fn nested_list_with_tasks() {
        let md = "1. first\n2. [x] done\n   - inner\n";
        let (blocks, _) = parse_blocks(md);
        let Block::List { start, items } = &blocks[0] else {
            panic!()
        };
        assert_eq!(*start, Some(1));
        assert_eq!(items[1].task, Some(true));
        assert!(matches!(
            items[1].blocks.last(),
            Some(Block::List { start: None, .. })
        ));
    }

    #[test]
    fn blockquote_code_rule_and_table() {
        let md = "> quoted\n\n```\nlet x = 1;\n```\n\n---\n\n| a | b |\n|:--|--:|\n| 1 | 2 |\n";
        let (blocks, _) = parse_blocks(md);
        assert!(matches!(&blocks[0], Block::BlockQuote { blocks } if blocks.len() == 1));
        assert_eq!(
            blocks[1],
            Block::CodeBlock {
                text: "let x = 1;".into()
            }
        );
        assert_eq!(blocks[2], Block::Rule);
        let Block::Table { aligns, head, rows } = &blocks[3] else {
            panic!()
        };
        assert_eq!(aligns, &vec![CellAlign::Left, CellAlign::Right]);
        assert_eq!(head.len(), 2);
        assert_eq!(rows[0][1], vec![plain("2")]);
    }

    #[test]
    fn lone_image_becomes_a_block_and_html_is_counted() {
        let (blocks, skipped) = parse_blocks("![alt](pic.png)\n\n<div>x</div>\n");
        assert_eq!(
            blocks[0],
            Block::Image {
                path: "pic.png".into()
            }
        );
        assert!(skipped >= 1);
    }
}
