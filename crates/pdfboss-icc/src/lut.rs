//! Multi-dimensional lookup transforms: lut8Type (clause 10.9), lut16Type
//! (clause 10.8), and lutAToBType (clause 10.10), evaluated device-to-PCS.

use crate::curve::Curve;
use crate::math::{lab_legacy16, lab_to_xyz, lab_v4, mat_apply, pcs_xyz_scale, s15f16, Mat3, D50};
use crate::IccError;

/// Most input channels a lookup transform accepts here; the raster paths
/// carry at most this many components.
pub(crate) const MAX_INPUTS: usize = 8;

/// How the 0..=1 values leaving the transform encode the PCS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PcsEncoding {
    Xyz,
    LabLegacy,
    LabV4,
}

fn pcs_to_xyz(pcs: [f32; 3], encoding: PcsEncoding) -> [f32; 3] {
    match encoding {
        PcsEncoding::Xyz => [
            pcs_xyz_scale(pcs[0]),
            pcs_xyz_scale(pcs[1]),
            pcs_xyz_scale(pcs[2]),
        ],
        PcsEncoding::LabLegacy => lab_to_xyz(lab_legacy16(pcs[0], pcs[1], pcs[2]), D50),
        PcsEncoding::LabV4 => lab_to_xyz(lab_v4(pcs[0], pcs[1], pcs[2]), D50),
    }
}

fn be16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn interp16(table: &[u16], x: f32) -> f32 {
    let last = table.len() - 1;
    let pos = x.clamp(0.0, 1.0) * last as f32;
    let i = (pos as usize).min(last.saturating_sub(1));
    let frac = pos - i as f32;
    let lo = table[i] as f32;
    let hi = table[(i + 1).min(last)] as f32;
    (lo + (hi - lo) * frac) / 65535.0
}

/// Multilinear interpolation in a CLUT whose first dimension varies least
/// rapidly (clauses 10.8 to 10.10). `grid` holds the per-dimension point
/// counts; entries are `outs` values per grid point, normalized to 0..=65535.
fn clut_interp(clut: &[u16], grid: &[usize], outs: usize, t: &[f32], out: &mut [f32; 3]) {
    let dims = grid.len();
    let mut stride = [0usize; MAX_INPUTS];
    let mut acc = outs;
    for i in (0..dims).rev() {
        stride[i] = acc;
        acc *= grid[i];
    }
    let mut base = [0usize; MAX_INPUTS];
    let mut frac = [0.0f32; MAX_INPUTS];
    for i in 0..dims {
        let pos = t[i].clamp(0.0, 1.0) * (grid[i] - 1) as f32;
        let cell = (pos as usize).min(grid[i].saturating_sub(2));
        base[i] = cell;
        frac[i] = if grid[i] > 1 { pos - cell as f32 } else { 0.0 };
    }
    *out = [0.0; 3];
    for corner in 0u32..(1 << dims) {
        let mut w = 1.0f32;
        let mut idx = 0usize;
        for i in 0..dims {
            let hi = (corner >> i) & 1 == 1;
            w *= if hi { frac[i] } else { 1.0 - frac[i] };
            let step = if hi && grid[i] > 1 { 1 } else { 0 };
            idx += (base[i] + step) * stride[i];
        }
        if w == 0.0 {
            continue;
        }
        for (j, slot) in out.iter_mut().enumerate() {
            *slot += w * clut[idx + j] as f32 / 65535.0;
        }
    }
}

/// A lut8Type or lut16Type transform, tables widened to 16 bits.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mft {
    ins: usize,
    grid: usize,
    matrix: Mat3,
    apply_matrix: bool,
    itable: Vec<u16>,
    ilen: usize,
    clut: Vec<u16>,
    otable: Vec<u16>,
    olen: usize,
    pcs: PcsEncoding,
    to_srgb: Mat3,
}

/// A lutAToBType transform.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mab {
    ins: usize,
    acurves: Vec<Curve>,
    grid: Vec<usize>,
    clut: Vec<u16>,
    mcurves: Vec<Curve>,
    matrix: Option<(Mat3, [f32; 3])>,
    bcurves: Vec<Curve>,
    pcs: PcsEncoding,
    to_srgb: Mat3,
}

/// Any A2B0 transform shape.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Lut {
    Mft(Mft),
    Mab(Mab),
}

impl Lut {
    pub(crate) fn parse(
        data: &[u8],
        input_is_xyz: bool,
        pcs_lab: bool,
        to_srgb: Mat3,
    ) -> Result<Lut, IccError> {
        if data.len() < 4 {
            return Err(IccError::Truncated);
        }
        match &data[0..4] {
            b"mft1" => Mft::parse(data, 8, input_is_xyz, pcs_lab, to_srgb).map(Lut::Mft),
            b"mft2" => Mft::parse(data, 16, input_is_xyz, pcs_lab, to_srgb).map(Lut::Mft),
            b"mAB " => Mab::parse(data, pcs_lab, to_srgb).map(Lut::Mab),
            _ => Err(IccError::Unsupported),
        }
    }

    pub(crate) fn inputs(&self) -> usize {
        match self {
            Lut::Mft(m) => m.ins,
            Lut::Mab(m) => m.ins,
        }
    }

    pub(crate) fn eval(&self, input: &[f32]) -> [f32; 3] {
        match self {
            Lut::Mft(m) => m.eval(input),
            Lut::Mab(m) => m.eval(input),
        }
    }
}

impl Mft {
    fn parse(
        data: &[u8],
        depth: u32,
        input_is_xyz: bool,
        pcs_lab: bool,
        to_srgb: Mat3,
    ) -> Result<Mft, IccError> {
        let head = if depth == 16 { 52 } else { 48 };
        if data.len() < head {
            return Err(IccError::Truncated);
        }
        let ins = data[8] as usize;
        let outs = data[9] as usize;
        let grid = data[10] as usize;
        if outs != 3 {
            return Err(IccError::Unsupported);
        }
        if ins == 0 || ins > MAX_INPUTS || grid < 2 {
            return Err(IccError::Unsupported);
        }
        let mut matrix = [[0.0f32; 3]; 3];
        for (r, row) in matrix.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = s15f16(be32(data, 12 + 4 * (3 * r + c)));
            }
        }
        let (ilen, olen) = if depth == 16 {
            let n = be16(data, 48) as usize;
            let m = be16(data, 50) as usize;
            if !(2..=4096).contains(&n) || !(2..=4096).contains(&m) {
                return Err(IccError::Malformed);
            }
            (n, m)
        } else {
            (256, 256)
        };
        let clut_entries = grid
            .checked_pow(ins as u32)
            .and_then(|cells| cells.checked_mul(outs))
            .ok_or(IccError::Malformed)?;
        let unit = if depth == 16 { 2 } else { 1 };
        let need = head + unit * (ins * ilen + clut_entries + outs * olen);
        if data.len() < need {
            return Err(IccError::Truncated);
        }
        let read = |at: usize, count: usize| -> Vec<u16> {
            (0..count)
                .map(|k| {
                    if depth == 16 {
                        be16(data, at + 2 * k)
                    } else {
                        data[at + k] as u16 * 257
                    }
                })
                .collect()
        };
        let itable = read(head, ins * ilen);
        let clut = read(head + unit * ins * ilen, clut_entries);
        let otable = read(head + unit * (ins * ilen + clut_entries), outs * olen);
        let pcs = match (pcs_lab, depth) {
            (false, _) => PcsEncoding::Xyz,
            (true, 16) => PcsEncoding::LabLegacy,
            (true, _) => PcsEncoding::LabV4,
        };
        Ok(Mft {
            ins,
            grid,
            matrix,
            apply_matrix: input_is_xyz,
            itable,
            ilen,
            clut,
            otable,
            olen,
            pcs,
            to_srgb,
        })
    }

    fn eval(&self, input: &[f32]) -> [f32; 3] {
        let mut x = [0.0f32; MAX_INPUTS];
        for (slot, v) in x.iter_mut().take(self.ins).zip(input) {
            *slot = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        if self.apply_matrix && self.ins == 3 {
            let v = mat_apply(&self.matrix, [x[0], x[1], x[2]]);
            for (slot, m) in x.iter_mut().zip(v) {
                *slot = m.clamp(0.0, 1.0);
            }
        }
        for (i, slot) in x.iter_mut().take(self.ins).enumerate() {
            *slot = interp16(&self.itable[i * self.ilen..(i + 1) * self.ilen], *slot);
        }
        let grid = [self.grid; MAX_INPUTS];
        let mut pcs = [0.0f32; 3];
        clut_interp(&self.clut, &grid[..self.ins], 3, &x[..self.ins], &mut pcs);
        for (j, slot) in pcs.iter_mut().enumerate() {
            *slot = interp16(&self.otable[j * self.olen..(j + 1) * self.olen], *slot);
        }
        finish(pcs, self.pcs, &self.to_srgb)
    }
}

impl Mab {
    fn parse(data: &[u8], pcs_lab: bool, to_srgb: Mat3) -> Result<Mab, IccError> {
        if data.len() < 32 {
            return Err(IccError::Truncated);
        }
        let ins = data[8] as usize;
        let outs = data[9] as usize;
        if outs != 3 {
            return Err(IccError::Unsupported);
        }
        if ins == 0 || ins > MAX_INPUTS {
            return Err(IccError::Unsupported);
        }
        let offset = |at: usize| be32(data, at) as usize;
        let (boff, moff, mcoff, coff, aoff) =
            (offset(12), offset(16), offset(20), offset(24), offset(28));
        if boff == 0 {
            return Err(IccError::Malformed);
        }
        let bcurves = parse_curves(data, boff, outs)?;
        let matrix = if moff == 0 {
            None
        } else {
            if data.len() < moff.checked_add(48).ok_or(IccError::Malformed)? {
                return Err(IccError::Truncated);
            }
            let e = |k: usize| s15f16(be32(data, moff + 4 * k));
            let m = [[e(0), e(1), e(2)], [e(3), e(4), e(5)], [e(6), e(7), e(8)]];
            Some((m, [e(9), e(10), e(11)]))
        };
        let mcurves = if mcoff == 0 {
            Vec::new()
        } else {
            if matrix.is_none() {
                return Err(IccError::Malformed);
            }
            parse_curves(data, mcoff, outs)?
        };
        let (grid, clut) = if coff == 0 {
            if ins != 3 {
                return Err(IccError::Unsupported);
            }
            (Vec::new(), Vec::new())
        } else {
            parse_mab_clut(data, coff, ins, outs)?
        };
        let acurves = if aoff == 0 {
            Vec::new()
        } else {
            if clut.is_empty() {
                return Err(IccError::Malformed);
            }
            parse_curves(data, aoff, ins)?
        };
        let pcs = if pcs_lab {
            PcsEncoding::LabV4
        } else {
            PcsEncoding::Xyz
        };
        Ok(Mab {
            ins,
            acurves,
            grid,
            clut,
            mcurves,
            matrix,
            bcurves,
            pcs,
            to_srgb,
        })
    }

    fn eval(&self, input: &[f32]) -> [f32; 3] {
        let mut x = [0.0f32; MAX_INPUTS];
        for (slot, v) in x.iter_mut().take(self.ins).zip(input) {
            *slot = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        for (i, curve) in self.acurves.iter().enumerate() {
            x[i] = curve.eval(x[i]);
        }
        let mut v = [x[0], x[1], x[2]];
        if !self.clut.is_empty() {
            clut_interp(&self.clut, &self.grid, 3, &x[..self.ins], &mut v);
        }
        for (j, curve) in self.mcurves.iter().enumerate() {
            v[j] = curve.eval(v[j]);
        }
        if let Some((m, off)) = &self.matrix {
            let t = mat_apply(m, v);
            for (slot, (t, o)) in v.iter_mut().zip(t.iter().zip(off)) {
                *slot = (t + o).clamp(0.0, 1.0);
            }
        }
        for (j, curve) in self.bcurves.iter().enumerate() {
            v[j] = curve.eval(v[j]);
        }
        finish(v, self.pcs, &self.to_srgb)
    }
}

fn finish(pcs: [f32; 3], encoding: PcsEncoding, to_srgb: &Mat3) -> [f32; 3] {
    let xyz = pcs_to_xyz(pcs, encoding);
    let lin = mat_apply(to_srgb, xyz);
    [
        crate::math::srgb_encode(lin[0]),
        crate::math::srgb_encode(lin[1]),
        crate::math::srgb_encode(lin[2]),
    ]
}

fn parse_curves(data: &[u8], offset: usize, count: usize) -> Result<Vec<Curve>, IccError> {
    let mut at = offset;
    let mut curves = Vec::with_capacity(count);
    for _ in 0..count {
        if at > data.len() {
            return Err(IccError::Truncated);
        }
        let (curve, size) = Curve::parse(&data[at..])?;
        curves.push(curve);
        at += size.div_ceil(4) * 4;
    }
    Ok(curves)
}

fn parse_mab_clut(
    data: &[u8],
    offset: usize,
    ins: usize,
    outs: usize,
) -> Result<(Vec<usize>, Vec<u16>), IccError> {
    let end = offset.checked_add(20).ok_or(IccError::Malformed)?;
    if data.len() < end {
        return Err(IccError::Truncated);
    }
    let grid: Vec<usize> = (0..ins).map(|i| data[offset + i] as usize).collect();
    if grid.iter().any(|&g| g < 2) {
        return Err(IccError::Malformed);
    }
    let precision = data[offset + 16] as usize;
    if precision != 1 && precision != 2 {
        return Err(IccError::Malformed);
    }
    let entries = grid
        .iter()
        .try_fold(outs, |acc, &g| acc.checked_mul(g))
        .ok_or(IccError::Malformed)?;
    let need = entries
        .checked_mul(precision)
        .and_then(|n| n.checked_add(end))
        .ok_or(IccError::Malformed)?;
    if data.len() < need {
        return Err(IccError::Truncated);
    }
    let clut: Vec<u16> = (0..entries)
        .map(|k| {
            if precision == 2 {
                be16(data, end + 2 * k)
            } else {
                data[end + k] as u16 * 257
            }
        })
        .collect();
    Ok((grid, clut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::xyz_to_linear_srgb;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn identity_matrix() -> Vec<u8> {
        let mut out = Vec::new();
        for r in 0..3 {
            for c in 0..3 {
                let v: i32 = if r == c { 0x0001_0000 } else { 0 };
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        out
    }

    /// A 1-input lut16 whose CLUT interpolates linearly between two XYZ
    /// grid points: input 0 maps to black, 1 to the PCS white (D50 after
    /// the 1 + 32 767/32 768 scale), and the midpoint to half of it.
    #[test]
    fn mft2_gray_line_to_xyz() {
        let mut data = Vec::new();
        data.extend_from_slice(b"mft2\0\0\0\0");
        data.push(1);
        data.push(3);
        data.push(2);
        data.push(0);
        data.extend_from_slice(&identity_matrix());
        data.extend_from_slice(&2u16.to_be_bytes());
        data.extend_from_slice(&2u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&65535u16.to_be_bytes());
        let d50_16: [u16; 3] = [0x7B6B, 0x8000, 0x6996];
        data.extend_from_slice(&[0u8; 6]);
        for v in d50_16 {
            data.extend_from_slice(&v.to_be_bytes());
        }
        for _ in 0..3 {
            data.extend_from_slice(&0u16.to_be_bytes());
            data.extend_from_slice(&65535u16.to_be_bytes());
        }
        let lut = Lut::parse(&data, false, false, xyz_to_linear_srgb(D50)).unwrap();
        assert_eq!(lut.inputs(), 1);
        let black = lut.eval(&[0.0]);
        assert!(black.iter().all(|&v| v < 1e-3), "{black:?}");
        let white = lut.eval(&[1.0]);
        assert!(white.iter().all(|&v| near(v, 1.0, 2e-3)), "{white:?}");
        let mid = lut.eval(&[0.5]);
        let want = crate::math::srgb_encode(0.5);
        assert!(
            mid.iter().all(|&v| near(v, want, 2e-3)),
            "{mid:?} want {want}"
        );
        for cut in 0..data.len() {
            let short = Lut::parse(&data[..cut], false, false, xyz_to_linear_srgb(D50));
            assert!(short.is_err(), "cut {cut}");
        }
    }

    /// A lut8 with legacy 8-bit PCSLAB output: the CLUT corner holding
    /// (L*, a*, b*) = (255, 128, 128)/255 decodes to Lab (100, 0, 0), the
    /// PCS white.
    #[test]
    fn mft1_lab_white_corner() {
        let mut data = Vec::new();
        data.extend_from_slice(b"mft1\0\0\0\0");
        data.push(1);
        data.push(3);
        data.push(2);
        data.push(0);
        data.extend_from_slice(&identity_matrix());
        let ramp: Vec<u8> = (0..=255).collect();
        data.extend_from_slice(&ramp);
        data.extend_from_slice(&[0, 128, 128]);
        data.extend_from_slice(&[255, 128, 128]);
        for _ in 0..3 {
            data.extend_from_slice(&ramp);
        }
        let lut = Lut::parse(&data, false, true, xyz_to_linear_srgb(D50)).unwrap();
        let white = lut.eval(&[1.0]);
        assert!(white.iter().all(|&v| near(v, 1.0, 3e-3)), "{white:?}");
        let black = lut.eval(&[0.0]);
        assert!(black.iter().all(|&v| v < 2e-2), "{black:?}");
    }

    /// A 2-input CLUT checks the multilinear corner weights and the
    /// least-rapid ordering of the first dimension: with grid 2x2 and
    /// outputs picked per corner, the blend at (0,75, 0,25) recovers the
    /// hand-computed weighted sum.
    #[test]
    fn clut_multilinear_hand_weights() {
        let corners: [[u16; 3]; 4] = [
            [0, 0, 0],
            [65535, 0, 0],
            [0, 65535, 0],
            [65535, 65535, 65535],
        ];
        let clut: Vec<u16> = corners.iter().flatten().copied().collect();
        let mut out = [0.0f32; 3];
        clut_interp(&clut, &[2, 2], 3, &[0.75, 0.25], &mut out);
        let w = [0.25 * 0.75, 0.25 * 0.25, 0.75 * 0.75, 0.75 * 0.25];
        let want = [w[1] + w[3], w[2] + w[3], w[3]];
        for (got, want) in out.iter().zip(want) {
            assert!(near(*got, want, 1e-5), "{out:?}");
        }
    }

    /// A B-curves-only lutAToBType (3 identity 'curv' entries) with XYZ PCS
    /// behaves like the raw PCSXYZ scale, and truncation at every prefix
    /// fails cleanly.
    #[test]
    fn mab_bcurves_only() {
        let mut data = Vec::new();
        data.extend_from_slice(b"mAB \0\0\0\0");
        data.push(3);
        data.push(3);
        data.extend_from_slice(&[0, 0]);
        data.extend_from_slice(&32u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        for _ in 0..3 {
            data.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
        }
        let lut = Lut::parse(&data, false, false, xyz_to_linear_srgb(D50)).unwrap();
        let white = lut.eval(&[
            D50[0] * 32768.0 / 65535.0,
            D50[1] * 32768.0 / 65535.0,
            D50[2] * 32768.0 / 65535.0,
        ]);
        assert!(white.iter().all(|&v| near(v, 1.0, 2e-3)), "{white:?}");
        for cut in 0..data.len() {
            assert!(
                Lut::parse(&data[..cut], false, false, xyz_to_linear_srgb(D50)).is_err(),
                "cut {cut}"
            );
        }
    }

    /// lutAToBType with A curves, a tiny CLUT, and B curves for a 4-input
    /// device space; the K = 1 corner is black.
    #[test]
    fn mab_cmyk_corners() {
        let mut data = Vec::new();
        data.extend_from_slice(b"mAB \0\0\0\0");
        data.push(4);
        data.push(3);
        data.extend_from_slice(&[0, 0]);
        let boff = 32u32;
        let coff = boff + 12 * 3;
        let aoff = coff + 20 + 16 * 3 * 2;
        data.extend_from_slice(&boff.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&coff.to_be_bytes());
        data.extend_from_slice(&aoff.to_be_bytes());
        for _ in 0..3 {
            data.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
        }
        let mut grid = [0u8; 16];
        grid[..4].copy_from_slice(&[2, 2, 2, 2]);
        data.extend_from_slice(&grid);
        data.push(2);
        data.extend_from_slice(&[0, 0, 0]);
        let scale = 32768.0 / 65535.0;
        let white: [u16; 3] = [
            (D50[0] * scale * 65535.0).round() as u16,
            (D50[1] * scale * 65535.0).round() as u16,
            (D50[2] * scale * 65535.0).round() as u16,
        ];
        for corner in 0..16u32 {
            let dark = corner != 0;
            for w in white {
                let v = if dark { 0 } else { w };
                data.extend_from_slice(&v.to_be_bytes());
            }
        }
        for _ in 0..4 {
            data.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
        }
        let lut = Lut::parse(&data, false, false, xyz_to_linear_srgb(D50)).unwrap();
        assert_eq!(lut.inputs(), 4);
        let paper = lut.eval(&[0.0, 0.0, 0.0, 0.0]);
        assert!(paper.iter().all(|&v| near(v, 1.0, 2e-3)), "{paper:?}");
        let ink = lut.eval(&[0.0, 0.0, 0.0, 1.0]);
        assert!(ink.iter().all(|&v| v < 1e-3), "{ink:?}");
    }
}
