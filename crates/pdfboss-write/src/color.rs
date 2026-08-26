//! Device color values for content generation. The first public color
//! vocabulary in the workspace — the render crate keeps its own private
//! read-side `ColorSpace`.

use pdfboss_core::content::Op;

/// A device color: gray, RGB or CMYK, components in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    /// DeviceGray.
    Gray(f32),
    /// DeviceRGB.
    Rgb(f32, f32, f32),
    /// DeviceCMYK.
    Cmyk(f32, f32, f32, f32),
}

impl Color {
    /// Black in DeviceGray.
    pub const BLACK: Color = Color::Gray(0.0);
    /// White in DeviceGray.
    pub const WHITE: Color = Color::Gray(1.0);

    /// The operator that selects this color for filling (`g`/`rg`/`k`).
    pub(crate) fn fill_op(self) -> Op {
        todo!("fill op for {self:?}")
    }

    /// The operator that selects this color for stroking (`G`/`RG`/`K`).
    pub(crate) fn stroke_op(self) -> Op {
        todo!("stroke op for {self:?}")
    }
}
