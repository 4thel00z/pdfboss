//! Cleanroom ICC profile parser and colour transform (ICC.1:2010, v2 and v4
//! profiles) for pdfboss.
//!
//! [`parse`] reads a profile's header, tag table, and default device-to-PCS
//! transform: matrix/TRC and grayTRC models (Annex F), or an A2B0 lookup
//! transform (lut8Type, lut16Type, lutAToBType). [`Profile::transform`] then
//! maps device components to non-linear sRGB in 0..=1: PCS values are
//! chromatically adapted from the D50 PCS illuminant to D65 with the linear
//! Bradford matrix (Annex E) and converted through the IEC 61966-2-1 primary
//! matrix and transfer. Everything is combined at parse time; a transform
//! call is curve lookups, one 3x3 multiply, and the sRGB encode, with no
//! allocation.
//!
//! A 3-channel matrix/TRC profile whose composed transform is the identity
//! within [`SRGB_TOLERANCE`] at the probe points reports
//! [`DeviceSpace::Rgb`] from [`Profile::device_equivalent`] (a gray profile
//! analogously reports [`DeviceSpace::Gray`]), so callers can keep painting
//! in device space. Only the profile's default transform is used; rendering
//! intents are not switched.

mod curve;
mod lut;
mod math;

use curve::Curve;
use lut::Lut;

pub use math::{lab_to_xyz, mat_apply, mat_mul, srgb_encode, xyz_to_linear_srgb, Mat3, D50};

/// Why a profile would not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccError {
    /// The data ends before a declared structure does.
    Truncated,
    /// The 'acsp' profile signature is missing.
    Signature,
    /// The profile is well-formed but uses no transform shape supported
    /// here.
    Unsupported,
    /// A structural invariant of the format is violated.
    Malformed,
}

impl std::fmt::Display for IccError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            IccError::Truncated => "truncated ICC profile",
            IccError::Signature => "missing ICC profile signature",
            IccError::Unsupported => "unsupported ICC transform shape",
            IccError::Malformed => "malformed ICC profile structure",
        };
        f.write_str(what)
    }
}

impl std::error::Error for IccError {}

/// The device space a profile's transform is indistinguishable from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceSpace {
    Rgb,
    Gray,
}

/// Per-channel tolerance for the composed-identity probe behind
/// [`Profile::device_equivalent`]: about 2,5 steps of 8-bit output. A pure
/// gamma-2,2 curve misses the sRGB transfer by ~0,03 at 1/16 input, so it
/// honestly fails.
pub const SRGB_TOLERANCE: f32 = 0.01;

const PROBES: [f32; 5] = [1.0 / 16.0, 0.25, 0.5, 0.75, 15.0 / 16.0];

#[derive(Debug, Clone, PartialEq)]
enum Pipeline {
    MatrixTrc { trc: [Curve; 3], m: Mat3 },
    GrayTrc { curve: Curve },
    Lut(Lut),
}

impl Pipeline {
    fn eval(&self, input: &[f32]) -> [f32; 3] {
        let comp = |i: usize| -> f32 {
            let v = input.get(i).copied().unwrap_or(0.0);
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        match self {
            Pipeline::MatrixTrc { trc, m } => {
                let lin = [
                    trc[0].eval(comp(0)),
                    trc[1].eval(comp(1)),
                    trc[2].eval(comp(2)),
                ];
                let rgb = mat_apply(m, lin);
                [
                    srgb_encode(rgb[0]),
                    srgb_encode(rgb[1]),
                    srgb_encode(rgb[2]),
                ]
            }
            Pipeline::GrayTrc { curve } => {
                let v = srgb_encode(curve.eval(comp(0)));
                [v, v, v]
            }
            Pipeline::Lut(lut) => lut.eval(input),
        }
    }
}

/// A parsed profile: its device channel count and compiled transform.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    channels: usize,
    pipeline: Pipeline,
    equivalent: Option<DeviceSpace>,
}

impl Profile {
    /// Number of device components [`Profile::transform`] reads.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The device space this transform is the identity for, within
    /// [`SRGB_TOLERANCE`], if any.
    pub fn device_equivalent(&self) -> Option<DeviceSpace> {
        self.equivalent
    }

    /// Maps device components (0..=1 each; missing read as 0, non-finite
    /// and out-of-range values clamp) to non-linear sRGB in 0..=1.
    pub fn transform(&self, input: &[f32]) -> [f32; 3] {
        self.pipeline.eval(input)
    }
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn channel_count(sig: &[u8]) -> Option<usize> {
    match sig {
        b"GRAY" => Some(1),
        b"RGB " | b"CMY " | b"XYZ " | b"Lab " | b"Luv " | b"YCbr" | b"Yxy " | b"HSV " | b"HLS " => {
            Some(3)
        }
        b"CMYK" => Some(4),
        [d @ b'2'..=b'9', b'C', b'L', b'R'] => Some((d - b'0') as usize),
        [d @ b'A'..=b'F', b'C', b'L', b'R'] => Some((d - b'A') as usize + 10),
        _ => None,
    }
}

/// The profile's tag table as (signature, data) pairs; entries whose bytes
/// fall outside the data are dropped.
struct Tags<'a> {
    data: &'a [u8],
    count: usize,
}

impl<'a> Tags<'a> {
    fn get(&self, sig: &[u8; 4]) -> Option<&'a [u8]> {
        for k in 0..self.count {
            let at = 132 + 12 * k;
            if &self.data[at..at + 4] != sig {
                continue;
            }
            let offset = be32(self.data, at + 4) as usize;
            let size = be32(self.data, at + 8) as usize;
            let end = offset.checked_add(size)?;
            if end > self.data.len() {
                return None;
            }
            return Some(&self.data[offset..end]);
        }
        None
    }
}

fn xyz_column(tag: &[u8]) -> Option<[f32; 3]> {
    if tag.len() < 20 || &tag[0..4] != b"XYZ " {
        return None;
    }
    Some([
        math::s15f16(be32(tag, 8)),
        math::s15f16(be32(tag, 12)),
        math::s15f16(be32(tag, 16)),
    ])
}

fn matrix_trc(tags: &Tags<'_>, to_srgb: &Mat3) -> Option<Pipeline> {
    let r = xyz_column(tags.get(b"rXYZ")?)?;
    let g = xyz_column(tags.get(b"gXYZ")?)?;
    let b = xyz_column(tags.get(b"bXYZ")?)?;
    let colorants: Mat3 = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let trc = [
        Curve::parse(tags.get(b"rTRC")?).ok()?.0,
        Curve::parse(tags.get(b"gTRC")?).ok()?.0,
        Curve::parse(tags.get(b"bTRC")?).ok()?.0,
    ];
    Some(Pipeline::MatrixTrc {
        trc,
        m: mat_mul(to_srgb, &colorants),
    })
}

fn probes_identity(pipeline: &Pipeline, channels: usize) -> bool {
    for axis in 0..channels {
        for v in PROBES {
            let mut input = [0.0f32; 3];
            input[axis] = v;
            let out = pipeline.eval(&input[..channels]);
            let want = if channels == 1 { [v, v, v] } else { input };
            if out
                .iter()
                .zip(want)
                .any(|(o, w)| (o - w).abs() > SRGB_TOLERANCE)
            {
                return false;
            }
        }
    }
    let input = [1.0f32; 3];
    let out = pipeline.eval(&input[..channels]);
    out.iter().all(|o| (o - 1.0).abs() <= SRGB_TOLERANCE)
}

/// Parses an ICC profile and compiles its default device-to-sRGB transform.
///
/// Tag precedence follows clause 8.10 — an A2B0 transform outranks the
/// matrix/TRC tags — with one deliberate exception: a matrix/TRC model that
/// probes as sRGB wins, since for such profiles both models encode the same
/// transform and the device-equivalence report lets callers skip the
/// conversion entirely.
pub fn parse(data: &[u8]) -> Result<Profile, IccError> {
    if data.len() < 132 {
        return Err(IccError::Truncated);
    }
    if &data[36..40] != b"acsp" {
        return Err(IccError::Signature);
    }
    if !(2..=4).contains(&data[8]) {
        return Err(IccError::Unsupported);
    }
    if be32(data, 0) as usize > data.len() {
        return Err(IccError::Truncated);
    }
    let channels = channel_count(&data[16..20]).ok_or(IccError::Unsupported)?;
    let pcs_lab = match &data[20..24] {
        b"XYZ " => false,
        b"Lab " => true,
        _ => return Err(IccError::Unsupported),
    };
    let declared = be32(data, 128) as usize;
    let count = (data.len() - 132) / 12;
    if declared > count {
        return Err(IccError::Malformed);
    }
    let tags = Tags {
        data,
        count: declared,
    };
    let to_srgb = xyz_to_linear_srgb(D50);

    let matrix = if channels == 3 && !pcs_lab {
        matrix_trc(&tags, &to_srgb)
    } else {
        None
    };
    if let Some(pipeline) = &matrix {
        if probes_identity(pipeline, 3) {
            return Ok(Profile {
                channels,
                pipeline: matrix.unwrap(),
                equivalent: Some(DeviceSpace::Rgb),
            });
        }
    }
    let gray = if channels == 1 {
        tags.get(b"kTRC")
            .and_then(|tag| Curve::parse(tag).ok())
            .map(|(curve, _)| Pipeline::GrayTrc { curve })
    } else {
        None
    };
    if let Some(pipeline) = &gray {
        if probes_identity(pipeline, 1) {
            return Ok(Profile {
                channels,
                pipeline: gray.unwrap(),
                equivalent: Some(DeviceSpace::Gray),
            });
        }
    }
    let lut = tags
        .get(b"A2B0")
        .and_then(|tag| Lut::parse(tag, &data[16..20] == b"XYZ ", pcs_lab, to_srgb).ok());
    if let Some(lut) = lut {
        if lut.inputs() != channels {
            return Err(IccError::Malformed);
        }
        return Ok(Profile {
            channels,
            pipeline: Pipeline::Lut(lut),
            equivalent: None,
        });
    }
    let pipeline = matrix.or(gray).ok_or(IccError::Unsupported)?;
    Ok(Profile {
        channels,
        pipeline,
        equivalent: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn fx(v: f64) -> [u8; 4] {
        (((v * 65536.0).round()) as i32).to_be_bytes()
    }

    fn xyz_tag(col: [f64; 3]) -> Vec<u8> {
        let mut out = b"XYZ \0\0\0\0".to_vec();
        for v in col {
            out.extend_from_slice(&fx(v));
        }
        out
    }

    fn para3_srgb() -> Vec<u8> {
        let mut out = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            out.extend_from_slice(&fx(v));
        }
        out
    }

    fn gamma_curv(g: f64) -> Vec<u8> {
        let mut out = b"curv\0\0\0\0\0\0\0\x01".to_vec();
        out.extend_from_slice(&(((g * 256.0).round()) as u16).to_be_bytes());
        out
    }

    fn build(colour: &[u8; 4], pcs: &[u8; 4], tags: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut header = vec![0u8; 128];
        header[8] = 4;
        header[16..20].copy_from_slice(colour);
        header[20..24].copy_from_slice(pcs);
        header[36..40].copy_from_slice(b"acsp");
        let mut table = (tags.len() as u32).to_be_bytes().to_vec();
        let mut body = Vec::new();
        let mut at = 132 + 12 * tags.len();
        for (sig, data) in tags {
            table.extend_from_slice(sig);
            table.extend_from_slice(&(at as u32).to_be_bytes());
            table.extend_from_slice(&(data.len() as u32).to_be_bytes());
            body.extend_from_slice(data);
            let pad = data.len().div_ceil(4) * 4 - data.len();
            body.extend_from_slice(&vec![0u8; pad]);
            at += data.len() + pad;
        }
        let mut out = header;
        out.extend_from_slice(&table);
        out.extend_from_slice(&body);
        let size = (out.len() as u32).to_be_bytes();
        out[0..4].copy_from_slice(&size);
        out
    }

    /// The IEC 61966-2-1 primaries, Bradford-adapted into the D50 PCS: the
    /// colorant columns a real sRGB profile carries.
    const SRGB_D50: [[f64; 3]; 3] = [
        [0.4360, 0.2225, 0.0139],
        [0.3851, 0.7169, 0.0971],
        [0.1431, 0.0606, 0.7139],
    ];

    fn srgb_profile() -> Vec<u8> {
        build(
            b"RGB ",
            b"XYZ ",
            &[
                (*b"rXYZ", xyz_tag(SRGB_D50[0])),
                (*b"gXYZ", xyz_tag(SRGB_D50[1])),
                (*b"bXYZ", xyz_tag(SRGB_D50[2])),
                (*b"rTRC", para3_srgb()),
                (*b"gTRC", para3_srgb()),
                (*b"bTRC", para3_srgb()),
            ],
        )
    }

    /// A byte-built matrix/TRC profile equal to sRGB probes as the
    /// device-RGB identity, and its transform round-trips inputs.
    #[test]
    fn srgb_profile_reports_rgb_equivalence() {
        let profile = parse(&srgb_profile()).unwrap();
        assert_eq!(profile.channels(), 3);
        assert_eq!(profile.device_equivalent(), Some(DeviceSpace::Rgb));
        let out = profile.transform(&[0.2, 0.5, 0.8]);
        for (o, w) in out.iter().zip([0.2, 0.5, 0.8]) {
            assert!(near(*o, w, 0.01), "{out:?}");
        }
    }

    /// Swapping the sRGB transfer for gamma 1,8 keeps the primaries but
    /// breaks the identity: mid-gray comes out lighter by a hand-computed
    /// amount (encode(0,5^1,8) is about 0,576), and the equivalence report
    /// is gone.
    #[test]
    fn gamma_18_profile_is_not_srgb() {
        let data = build(
            b"RGB ",
            b"XYZ ",
            &[
                (*b"rXYZ", xyz_tag(SRGB_D50[0])),
                (*b"gXYZ", xyz_tag(SRGB_D50[1])),
                (*b"bXYZ", xyz_tag(SRGB_D50[2])),
                (*b"rTRC", gamma_curv(1.8)),
                (*b"gTRC", gamma_curv(1.8)),
                (*b"bTRC", gamma_curv(1.8)),
            ],
        );
        let profile = parse(&data).unwrap();
        assert_eq!(profile.device_equivalent(), None);
        let out = profile.transform(&[0.5, 0.5, 0.5]);
        let want = srgb_encode(0.5f32.powf(1.8));
        for o in out {
            assert!(near(o, want, 0.01), "{out:?} want {want}");
        }
        assert!(out[0] > 0.55, "gamma 1.8 renders mid-gray lighter");
    }

    /// A gray profile with the sRGB transfer probes as the device-gray
    /// identity; a linear (gamma 1) gray profile does not, and brightens
    /// mid-gray to encode(0,5).
    #[test]
    fn gray_profiles() {
        let data = build(b"GRAY", b"XYZ ", &[(*b"kTRC", para3_srgb())]);
        let profile = parse(&data).unwrap();
        assert_eq!(profile.channels(), 1);
        assert_eq!(profile.device_equivalent(), Some(DeviceSpace::Gray));

        let linear = build(b"GRAY", b"XYZ ", &[(*b"kTRC", gamma_curv(1.0))]);
        let profile = parse(&linear).unwrap();
        assert_eq!(profile.device_equivalent(), None);
        let out = profile.transform(&[0.5]);
        assert!(near(out[0], srgb_encode(0.5), 1e-4), "{out:?}");
        assert_eq!(out[0], out[1]);
    }

    /// Headers select the parse outcome: a bad signature, an impossible
    /// declared tag count, an unknown colour space, and an unsupported
    /// version all error cleanly.
    #[test]
    fn header_validation() {
        let mut bad_magic = srgb_profile();
        bad_magic[36] = b'x';
        assert_eq!(parse(&bad_magic), Err(IccError::Signature));

        let mut bad_count = srgb_profile();
        bad_count[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse(&bad_count), Err(IccError::Malformed));

        let bad_space = build(b"????", b"XYZ ", &[]);
        assert_eq!(parse(&bad_space), Err(IccError::Unsupported));

        let mut v5 = srgb_profile();
        v5[8] = 5;
        assert_eq!(parse(&v5), Err(IccError::Unsupported));

        let mut v2 = srgb_profile();
        v2[8] = 2;
        assert!(parse(&v2).is_ok(), "v2 headers share the layout");

        let empty = build(b"RGB ", b"XYZ ", &[]);
        assert_eq!(parse(&empty), Err(IccError::Unsupported));
    }

    /// Every prefix of a valid profile errors instead of panicking, and a
    /// tag entry pointing past the end reads as absent.
    #[test]
    fn truncation_and_hostile_offsets() {
        let data = srgb_profile();
        for cut in 0..data.len() {
            let result = parse(&data[..cut]);
            assert!(result.is_err(), "cut {cut}");
        }
        let mut hostile = data.clone();
        hostile[136..140].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse(&hostile).is_err());
    }

    /// An RGB profile whose A2B0 collapses everything to the PCS white
    /// paints white for any input — proof the lookup transform is selected
    /// when the matrix/TRC set is absent.
    #[test]
    fn a2b0_lut_profile() {
        let mut lut = Vec::new();
        lut.extend_from_slice(b"mft2\0\0\0\0");
        lut.push(3);
        lut.push(3);
        lut.push(2);
        lut.push(0);
        for r in 0..3 {
            for c in 0..3 {
                let v: i32 = if r == c { 0x0001_0000 } else { 0 };
                lut.extend_from_slice(&v.to_be_bytes());
            }
        }
        lut.extend_from_slice(&2u16.to_be_bytes());
        lut.extend_from_slice(&2u16.to_be_bytes());
        for _ in 0..3 {
            lut.extend_from_slice(&0u16.to_be_bytes());
            lut.extend_from_slice(&65535u16.to_be_bytes());
        }
        let white: [u16; 3] = [0x7B6B, 0x8000, 0x6996];
        for _ in 0..8 {
            for w in white {
                lut.extend_from_slice(&w.to_be_bytes());
            }
        }
        for _ in 0..3 {
            lut.extend_from_slice(&0u16.to_be_bytes());
            lut.extend_from_slice(&65535u16.to_be_bytes());
        }
        let data = build(b"RGB ", b"XYZ ", &[(*b"A2B0", lut)]);
        let profile = parse(&data).unwrap();
        assert_eq!(profile.device_equivalent(), None);
        let out = profile.transform(&[0.3, 0.9, 0.1]);
        assert!(out.iter().all(|&v| near(v, 1.0, 2e-3)), "{out:?}");
        for cut in 0..data.len() {
            assert!(parse(&data[..cut]).is_err(), "cut {cut}");
        }
    }

    /// xCLR signatures map to their channel counts.
    #[test]
    fn xclr_channel_counts() {
        assert_eq!(channel_count(b"2CLR"), Some(2));
        assert_eq!(channel_count(b"9CLR"), Some(9));
        assert_eq!(channel_count(b"ACLR"), Some(10));
        assert_eq!(channel_count(b"FCLR"), Some(15));
        assert_eq!(channel_count(b"GCLR"), None);
    }
}
