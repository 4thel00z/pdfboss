//! Core PDF machinery: syntax, objects, filters, cross-references and the
//! document model, implemented from the PDF specification (ISO 32000).

pub mod content;
pub mod document;
pub mod error;
pub mod filters;
pub mod geom;
pub mod lexer;
pub mod object;
pub mod objstm;
pub mod parser;
pub mod xref;

pub use document::{Document, Metadata, Page};
pub use error::{Error, Result};
pub use geom::{Matrix, Point, Rect};
pub use object::{Dict, Name, ObjRef, Object, Stream};
