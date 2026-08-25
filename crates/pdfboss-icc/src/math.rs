//! Fixed-point decoding, PCS value encodings, chromatic adaptation, and the
//! XYZ-to-sRGB conversion shared by every transform pipeline.

/// A row-major 3x3 matrix.
pub type Mat3 = [[f32; 3]; 3];

/// The nCIEXYZ values of the PCS illuminant (D50), ICC.1:2010 clause 7.2.16.
pub const D50: [f32; 3] = [0.9642, 1.0, 0.8249];

/// Decodes an s15Fixed16Number (clause 4.6): signed, 16 fractional bits.
pub(crate) fn s15f16(raw: u32) -> f32 {
    (raw as i32) as f32 / 65536.0
}

/// Decodes a u8Fixed8Number (clause 4.9): unsigned, 8 fractional bits.
pub(crate) fn u8f8(raw: u16) -> f32 {
    raw as f32 / 256.0
}

/// The 16-bit PCSXYZ value of a table entry in 0..=1 range: PCSXYZ is
/// encoded as u1Fixed15Number (clauses 4.8 and 6.3.4.2, 8000h is 1,0), so
/// curve and CLUT range 1,0 maps to 1 + (32 767/32 768) (clauses 10.5,
/// 10.16, and the lutAToBType note in Annex F.3).
pub(crate) fn pcs_xyz_scale(v: f32) -> f32 {
    v * (65535.0 / 32768.0)
}

/// Legacy 16-bit PCSLAB decoding used by lut16Type (clause 10.8, Tables 39
/// and 40): L* spans 0..=FF00h for 0..=100, a*/b* span 0..=FF00h for
/// -128..=127.
pub(crate) fn lab_legacy16(l: f32, a: f32, b: f32) -> [f32; 3] {
    [
        l * 65535.0 * 100.0 / 65280.0,
        a * 65535.0 / 256.0 - 128.0,
        b * 65535.0 / 256.0 - 128.0,
    ]
}

/// 16-bit PCSLAB decoding (clause 6.3.4.2, Tables 12 and 13): L* spans the
/// full 0..=FFFFh for 0..=100, a*/b* for -128..=127. Inputs are table values
/// already normalized to 0..=1.
pub(crate) fn lab_v4(l: f32, a: f32, b: f32) -> [f32; 3] {
    [l * 100.0, a * 255.0 - 128.0, b * 255.0 - 128.0]
}

/// The linearized Bradford cone-response matrix, ICC.1:2010 Equation (E.1).
const BRADFORD: [[f64; 3]; 3] = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

/// D65 tristimulus values derived from the IEC 61966-2-1 white chromaticity
/// (x = 0,3127, y = 0,3290) at Y = 1.
const D65_XY: (f64, f64) = (0.3127, 0.3290);

/// The IEC 61966-2-1 (sRGB) primary chromaticities.
const SRGB_XY: [(f64, f64); 3] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];

fn xyy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

fn mul64(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0f64; 3]; 3];
    for (row, ar) in out.iter_mut().zip(a) {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| ar[k] * b[k][j]).sum();
        }
    }
    out
}

fn apply64(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn inv64(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c = |r: usize, s: usize| -> f64 {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (s1, s2) = ((s + 1) % 3, (s + 2) % 3);
        m[r1][s1] * m[r2][s2] - m[r1][s2] * m[r2][s1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    let mut out = [[0.0f64; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (s, cell) in row.iter_mut().enumerate() {
            *cell = c(s, r) / det;
        }
    }
    out
}

/// The linear-Bradford chromatic adaptation matrix taking XYZ relative to
/// `white` to XYZ relative to D65 (ICC.1:2010 Equation (E.2)).
fn bradford_to_d65(white: [f32; 3]) -> [[f64; 3]; 3] {
    let src = apply64(
        &BRADFORD,
        [white[0] as f64, white[1] as f64, white[2] as f64],
    );
    let dst = apply64(&BRADFORD, xyy_to_xyz(D65_XY.0, D65_XY.1));
    let scale = [
        [dst[0] / src[0], 0.0, 0.0],
        [0.0, dst[1] / src[1], 0.0],
        [0.0, 0.0, dst[2] / src[2]],
    ];
    mul64(&inv64(&BRADFORD), &mul64(&scale, &BRADFORD))
}

/// XYZ(D65) to linear-sRGB matrix, derived from the IEC 61966-2-1 primary
/// and white chromaticities.
fn srgb_from_xyz_d65() -> [[f64; 3]; 3] {
    let p = [
        xyy_to_xyz(SRGB_XY[0].0, SRGB_XY[0].1),
        xyy_to_xyz(SRGB_XY[1].0, SRGB_XY[1].1),
        xyy_to_xyz(SRGB_XY[2].0, SRGB_XY[2].1),
    ];
    let cols = [
        [p[0][0], p[1][0], p[2][0]],
        [p[0][1], p[1][1], p[2][1]],
        [p[0][2], p[1][2], p[2][2]],
    ];
    let s = apply64(&inv64(&cols), xyy_to_xyz(D65_XY.0, D65_XY.1));
    let m = [
        [cols[0][0] * s[0], cols[0][1] * s[1], cols[0][2] * s[2]],
        [cols[1][0] * s[0], cols[1][1] * s[1], cols[1][2] * s[2]],
        [cols[2][0] * s[0], cols[2][1] * s[1], cols[2][2] * s[2]],
    ];
    inv64(&m)
}

/// Matrix taking XYZ relative to `white` to linear sRGB: Bradford adaptation
/// to D65 followed by the sRGB primary matrix.
pub fn xyz_to_linear_srgb(white: [f32; 3]) -> Mat3 {
    let m = mul64(&srgb_from_xyz_d65(), &bradford_to_d65(white));
    let mut out = [[0.0f32; 3]; 3];
    for (row, mr) in out.iter_mut().zip(&m) {
        for (cell, v) in row.iter_mut().zip(mr) {
            *cell = *v as f32;
        }
    }
    out
}

/// Row-major 3x3 matrix product.
pub fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0f32; 3]; 3];
    for (row, ar) in out.iter_mut().zip(a) {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| ar[k] * b[k][j]).sum();
        }
    }
    out
}

/// Applies a 3x3 matrix to a column vector.
pub fn mat_apply(m: &Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The IEC 61966-2-1 sRGB opto-electronic transfer: linear 0..=1 in,
/// non-linear 0..=1 out. Out-of-range and non-finite inputs clamp.
pub fn srgb_encode(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        return 12.92 * v;
    }
    1.055 * v.powf(1.0 / 2.4) - 0.055
}

/// CIE L*a*b* to XYZ relative to `white`, per the CIE definition used by
/// ISO 32000-1 clause 8.6.5.4: the inverse transfer g(t) = t^3 above 6/29
/// continues linearly with slope 108/841 below it.
pub fn lab_to_xyz(lab: [f32; 3], white: [f32; 3]) -> [f32; 3] {
    let finv = |t: f32| -> f32 {
        if t > 6.0 / 29.0 {
            return t * t * t;
        }
        (108.0 / 841.0) * (t - 4.0 / 29.0)
    };
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    [
        white[0] * finv(fx),
        white[1] * finv(fy),
        white[2] * finv(fz),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    /// Clause 4.6 Table 4, hand-transcribed.
    #[test]
    fn s15fixed16_spec_values() {
        assert_eq!(s15f16(0x8000_0000), -32768.0);
        assert_eq!(s15f16(0), 0.0);
        assert_eq!(s15f16(0x0001_0000), 1.0);
        assert!(near(s15f16(0x7FFF_FFFF), 32767.0 + 65535.0 / 65536.0, 1e-3));
        assert_eq!(s15f16(0xFFFF_0000), -1.0);
    }

    /// Clause 4.9 Table 7 and clause 4.8 Table 6, hand-transcribed; the
    /// u1Fixed15 facts read through the table-fraction scale.
    #[test]
    fn u8fixed8_and_u1fixed15_spec_values() {
        assert_eq!(u8f8(0x0000), 0.0);
        assert_eq!(u8f8(0x0100), 1.0);
        assert_eq!(u8f8(0xFFFF), 255.0 + 255.0 / 256.0);
        assert_eq!(u8f8(0x0180), 1.5);
        assert_eq!(pcs_xyz_scale(0.0), 0.0);
        assert!(near(pcs_xyz_scale(32768.0 / 65535.0), 1.0, 1e-6));
        assert!(near(pcs_xyz_scale(1.0), 1.0 + 32767.0 / 32768.0, 1e-4));
    }

    /// Clause 10.8 Tables 39 and 40: legacy PCSLAB. Raw 16-bit values enter
    /// as table fractions (raw / 65 535).
    #[test]
    fn legacy_lab16_spec_values() {
        let f = |raw: u16| raw as f32 / 65535.0;
        let [l, a, b] = lab_legacy16(f(0x0000), f(0x0000), f(0x8000));
        assert_eq!(l, 0.0);
        assert_eq!(a, -128.0);
        assert!(near(b, 0.0, 1e-4));
        let [l, a, b] = lab_legacy16(f(0xFF00), f(0xFF00), f(0xFFFF));
        assert!(near(l, 100.0, 1e-3));
        assert!(near(a, 127.0, 1e-3));
        assert!(near(b, 127.0 + 255.0 / 256.0, 1e-3));
        let [l, _, _] = lab_legacy16(f(0xFFFF), 0.0, 0.0);
        assert!(near(l, 100.0 + 25500.0 / 65280.0, 1e-3));
    }

    /// Clause 6.3.4.2 Tables 12 and 13: the 16-bit PCSLAB encoding.
    #[test]
    fn v4_lab16_spec_values() {
        let f = |raw: u16| raw as f32 / 65535.0;
        let [l, a, b] = lab_v4(f(0x0000), f(0x0000), f(0x8080));
        assert_eq!(l, 0.0);
        assert_eq!(a, -128.0);
        assert!(near(b, 0.0, 1e-4));
        let [l, a, _] = lab_v4(f(0xFFFF), f(0xFFFF), 0.0);
        assert_eq!(l, 100.0);
        assert_eq!(a, 127.0);
    }

    /// Bradford adaptation is exact on the white points it is built from:
    /// D50 maps to D65, so PCS white lands on linear sRGB (1, 1, 1).
    #[test]
    fn d50_white_adapts_to_srgb_white() {
        let m = xyz_to_linear_srgb(D50);
        let rgb = mat_apply(&m, D50);
        for c in rgb {
            assert!(near(c, 1.0, 1e-4), "white -> {rgb:?}");
        }
    }

    /// An identity adaptation (white already D65) followed by the primary
    /// matrix sends the D65 white to (1, 1, 1) and the red primary's XYZ to
    /// pure red.
    #[test]
    fn srgb_matrix_maps_primaries_to_axes() {
        let d65 = [
            (0.3127 / 0.3290) as f32,
            1.0,
            ((1.0 - 0.3127 - 0.3290) / 0.3290) as f32,
        ];
        let m = xyz_to_linear_srgb(d65);
        let white = mat_apply(&m, d65);
        for c in white {
            assert!(near(c, 1.0, 1e-4), "white -> {white:?}");
        }
        let red_xyz = [0.64 / 0.33, 1.0, (1.0 - 0.64 - 0.33) / 0.33];
        let sum: f32 = red_xyz.iter().sum::<f32>();
        let rgb = mat_apply(&m, red_xyz);
        assert!(rgb[0] > 0.0, "red scales positive");
        assert!(near(rgb[1] / sum, 0.0, 1e-4));
        assert!(near(rgb[2] / sum, 0.0, 1e-4));
    }

    /// IEC 61966-2-1 transfer: the linear toe below 0,003 130 8 and the
    /// power branch, hand-computed at 0,5.
    #[test]
    fn srgb_encode_hand_values() {
        assert_eq!(srgb_encode(0.0), 0.0);
        assert!(near(srgb_encode(1.0), 1.0, 1e-6));
        assert!(near(srgb_encode(0.001), 0.01292, 1e-6));
        assert!(near(srgb_encode(0.5), 0.735357, 1e-4));
        assert_eq!(srgb_encode(f32::NAN), 0.0);
        assert_eq!(srgb_encode(-2.0), 0.0);
    }

    /// L* = 100 with zero a*, b* reproduces the white point exactly; L* = 50
    /// gives Y = ((66/116))^3, hand-computed.
    #[test]
    fn lab_to_xyz_hand_values() {
        let xyz = lab_to_xyz([100.0, 0.0, 0.0], D50);
        for (got, want) in xyz.iter().zip(D50) {
            assert!(near(*got, want, 1e-4), "{xyz:?}");
        }
        let y = lab_to_xyz([50.0, 0.0, 0.0], [1.0, 1.0, 1.0])[1];
        let fy = 66.0f32 / 116.0;
        assert!(near(y, fy * fy * fy, 1e-5));
        let low = lab_to_xyz([4.0, 0.0, 0.0], [1.0, 1.0, 1.0])[1];
        assert!(near(
            low,
            (108.0 / 841.0) * (20.0 / 116.0 - 4.0 / 29.0),
            1e-5
        ));
    }
}
