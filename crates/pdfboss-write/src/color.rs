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
        match self {
            Color::Gray(gray) => Op::SetFillGray(gray),
            Color::Rgb(r, g, b) => Op::SetFillRGB(r, g, b),
            Color::Cmyk(c, m, y, k) => Op::SetFillCMYK(c, m, y, k),
        }
    }

    /// The operator that selects this color for stroking (`G`/`RG`/`K`).
    pub(crate) fn stroke_op(self) -> Op {
        match self {
            Color::Gray(gray) => Op::SetStrokeGray(gray),
            Color::Rgb(r, g, b) => Op::SetStrokeRGB(r, g, b),
            Color::Cmyk(c, m, y, k) => Op::SetStrokeCMYK(c, m, y, k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_op_maps_each_variant() {
        assert_eq!(Color::Gray(0.25).fill_op(), Op::SetFillGray(0.25));
        assert_eq!(
            Color::Rgb(0.1, 0.2, 0.3).fill_op(),
            Op::SetFillRGB(0.1, 0.2, 0.3)
        );
        assert_eq!(
            Color::Cmyk(0.1, 0.2, 0.3, 0.4).fill_op(),
            Op::SetFillCMYK(0.1, 0.2, 0.3, 0.4)
        );
        assert_eq!(Color::BLACK.fill_op(), Op::SetFillGray(0.0));
        assert_eq!(Color::WHITE.fill_op(), Op::SetFillGray(1.0));
    }

    #[test]
    fn stroke_op_maps_each_variant() {
        assert_eq!(Color::Gray(0.75).stroke_op(), Op::SetStrokeGray(0.75));
        assert_eq!(
            Color::Rgb(0.4, 0.5, 0.6).stroke_op(),
            Op::SetStrokeRGB(0.4, 0.5, 0.6)
        );
        assert_eq!(
            Color::Cmyk(0.5, 0.6, 0.7, 0.8).stroke_op(),
            Op::SetStrokeCMYK(0.5, 0.6, 0.7, 0.8)
        );
    }
}
