//! CSS-subset themes for pdfboss document composition: element-type
//! selectors resolved into concrete text and box styles.

pub mod parse;
pub mod style;
pub mod theme;

pub use parse::StyleError;
pub use style::{Align, Declared, Decoration, Edges, Element, FontFamily, FontSize, TextStyle};
pub use theme::Theme;
