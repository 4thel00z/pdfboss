//! Lazy iteration over a document's elements: the physical file structure
//! (header, indirect objects, cross-reference sections, trailer, startxref,
//! eof) with byte spans, and the logical document structure (pages, fonts,
//! images, annotations, content operators). ISO 32000 §7.5 (file structure)
//! and §7.7 (document structure).

use crate::content::Op;
use crate::object::{Dict, Name, ObjRef, Object};

/// Byte range in the physical file, end-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

impl Span {
    /// A span from `start` (inclusive) to `end` (exclusive).
    pub fn new(start: u64, end: u64) -> Span {
        Span { start, end }
    }

    /// Number of bytes covered; inverted spans count as zero.
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Kind of a cross-reference section (ISO 32000 §7.5.4 / §7.5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefKind {
    /// A classic `xref` table.
    Table,
    /// A cross-reference stream.
    Stream,
}

/// One element of a document, physical or logical.
#[derive(Debug, Clone)]
pub enum Element {
    /// The `%PDF-x.y` header.
    Header { version: (u8, u8), span: Span },
    /// One indirect object.
    IndirectObject {
        r: ObjRef,
        object: Object,
        /// Span of `N G obj … endobj` in the file. For objects stored in an
        /// object stream this is the container stream object's span.
        span: Span,
        /// For objects inside an object stream: the container's reference
        /// and this object's byte range within the *decoded* stream data.
        in_objstm: Option<(ObjRef, Span)>,
    },
    /// One cross-reference section (table or stream).
    XrefSection {
        kind: XrefKind,
        span: Span,
        entries: usize,
    },
    /// The trailer: the merged trailer dictionary plus the byte range of the
    /// newest trailer region (classic `trailer << … >>`, or the newest
    /// cross-reference stream object when no classic trailer exists).
    Trailer { dict: Dict, span: Span },
    /// The `startxref` keyword and its offset operand.
    StartXref { offset: u64, span: Span },
    /// The `%%EOF` marker.
    Eof { span: Span },

    /// One page (logical).
    Page { index: usize, r: ObjRef },
    /// One font referenced from a page's resources.
    Font {
        page: Option<usize>,
        r: ObjRef,
        subtype: Name,
        base_font: Option<Name>,
    },
    /// One image XObject referenced from a page's resources.
    Image {
        page: Option<usize>,
        r: ObjRef,
        width: u32,
        height: u32,
    },
    /// One annotation on a page.
    Annotation {
        page: usize,
        r: ObjRef,
        subtype: Name,
    },
    /// One content-stream operator of a page.
    ContentOp {
        page: usize,
        op: Op,
        /// Byte range within the page's decoded, concatenated content.
        span_in_content: Span,
    },
}

/// Selects which element layers [`crate::Document::elements`] yields.
#[derive(Debug, Clone)]
pub struct ElementOpts {
    /// Yield physical file-structure elements.
    pub physical: bool,
    /// Yield logical document-structure elements.
    pub logical: bool,
    /// Restrict logical elements to these 0-based page indices.
    pub pages: Option<Vec<usize>>,
    /// Yield [`Element::ContentOp`] items (high-volume; off by default).
    pub content_ops: bool,
}

impl Default for ElementOpts {
    fn default() -> Self {
        ElementOpts {
            physical: true,
            logical: true,
            pages: None,
            content_ops: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_opts_defaults() {
        let opts = ElementOpts::default();
        assert!(opts.physical);
        assert!(opts.logical);
        assert!(opts.pages.is_none());
        assert!(!opts.content_ops);
    }

    #[test]
    fn span_length_and_emptiness() {
        let span = Span::new(10, 25);
        assert_eq!(span.len(), 15);
        assert!(!span.is_empty());
        assert!(Span::new(7, 7).is_empty());
        assert_eq!(Span::new(9, 3).len(), 0);
    }
}
