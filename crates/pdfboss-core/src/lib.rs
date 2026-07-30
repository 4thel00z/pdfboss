//! Core PDF machinery: syntax, objects, filters, cross-references, the
//! document model, and lazy element iteration (physical file structure with
//! byte spans plus logical document structure), implemented from the PDF
//! specification (ISO 32000).

pub mod content;
pub mod crypt;
pub mod document;
pub mod elements;
pub mod error;
pub mod filters;
pub mod geom;
pub mod hash;
pub mod lexer;
pub mod object;
pub mod objstm;
pub mod parser;
pub mod pretty;
pub mod source;
pub mod xref;

pub use crypt::Decryptor;
pub use document::{map_pages, page_content_with, Document, DocumentSeed, Metadata, Page};
pub use elements::{Element, ElementOpts, Span, XrefKind};
pub use error::{Error, Result};
pub use geom::{Matrix, Point, Rect};
pub use hash::{FastMap, FastSet, FxHasher};
pub use object::{Dict, Name, ObjRef, Object, Stream};
pub use source::{
    block_on, resolve_sync_with, resolve_with, AsyncObjectSource, BoxFuture, Immediate,
    ObjectSource, MAX_RESOLVE_DEPTH,
};
