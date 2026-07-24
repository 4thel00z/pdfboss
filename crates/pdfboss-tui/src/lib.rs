//! Interactive terminal explorer for PDF internals, implemented from
//! ISO 32000 on top of `pdfboss-aio`'s async document model.
//!
//! State machine (`app`), pane models (`tree`, `inspector`, `hexview`,
//! `preview`, `search`), key mapping (`input`) and rendering (`ui`) are
//! pure and unit-testable; only [`run`] touches the real terminal.

pub mod hexview;
pub mod search;
pub mod tree;
