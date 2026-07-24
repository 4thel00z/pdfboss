//! Interactive terminal explorer for PDF internals, implemented from
//! ISO 32000 on top of `pdfboss-aio`'s async document model.
//!
//! State machine (`app`), pane models (`tree`, `inspector`, `hexview`,
//! `preview`, `search`), key mapping (`input`) and rendering (`ui`) are
//! pure and unit-testable; only [`run`] touches the real terminal.

pub mod app;
pub mod hexview;
pub mod input;
pub mod inspector;
pub mod preview;
pub mod search;
pub mod tree;
pub mod ui;
