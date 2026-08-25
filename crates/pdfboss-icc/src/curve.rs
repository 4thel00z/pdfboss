//! One-dimensional tone curves: curveType ('curv', clause 10.5) and
//! parametricCurveType ('para', clause 10.16).

use crate::IccError;

/// A parsed tone curve. Parametric types 1 to 4 all canonicalize to
/// y = (ax + b)^g + e for x >= d, y = cx + f below, which each type's
/// parameter list fills in.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Curve {
    Identity,
    Gamma(f32),
    Table(Vec<u16>),
    Para {
        g: f32,
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        e: f32,
        f: f32,
    },
}

fn be16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

impl Curve {
    /// Parses an embedded curve starting at the beginning of `data` and
    /// returns it with its encoded size in bytes (unpadded).
    pub(crate) fn parse(data: &[u8]) -> Result<(Curve, usize), IccError> {
        if data.len() < 12 {
            return Err(IccError::Truncated);
        }
        match &data[0..4] {
            b"curv" => Self::parse_curv(data),
            b"para" => Self::parse_para(data),
            _ => Err(IccError::Malformed),
        }
    }

    fn parse_curv(data: &[u8]) -> Result<(Curve, usize), IccError> {
        let n = be32(data, 8) as usize;
        if n == 0 {
            return Ok((Curve::Identity, 12));
        }
        if data.len() < 12 + 2 * n {
            return Err(IccError::Truncated);
        }
        if n == 1 {
            return Ok((Curve::Gamma(crate::math::u8f8(be16(data, 12))), 14));
        }
        let table: Vec<u16> = (0..n).map(|i| be16(data, 12 + 2 * i)).collect();
        Ok((Curve::Table(table), 12 + 2 * n))
    }

    fn parse_para(data: &[u8]) -> Result<(Curve, usize), IccError> {
        let kind = be16(data, 8);
        let count = match kind {
            0 => 1,
            1 => 3,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => return Err(IccError::Malformed),
        };
        if data.len() < 12 + 4 * count {
            return Err(IccError::Truncated);
        }
        let mut p = [0.0f32; 7];
        for (i, slot) in p.iter_mut().take(count).enumerate() {
            *slot = crate::math::s15f16(be32(data, 12 + 4 * i));
        }
        let size = 12 + 4 * count;
        let threshold = |a: f32, b: f32| if a == 0.0 { 0.0 } else { -b / a };
        let curve = match kind {
            0 => Curve::Gamma(p[0]),
            1 => Curve::Para {
                g: p[0],
                a: p[1],
                b: p[2],
                c: 0.0,
                d: threshold(p[1], p[2]),
                e: 0.0,
                f: 0.0,
            },
            2 => Curve::Para {
                g: p[0],
                a: p[1],
                b: p[2],
                c: 0.0,
                d: threshold(p[1], p[2]),
                e: p[3],
                f: p[3],
            },
            3 => Curve::Para {
                g: p[0],
                a: p[1],
                b: p[2],
                c: p[3],
                d: p[4],
                e: 0.0,
                f: 0.0,
            },
            _ => Curve::Para {
                g: p[0],
                a: p[1],
                b: p[2],
                c: p[3],
                d: p[4],
                e: p[5],
                f: p[6],
            },
        };
        Ok((curve, size))
    }

    /// Evaluates the curve at `x`. Domain and range are both clamped to
    /// 0..=1 (clauses 10.5 and 10.16); non-finite results read as 0.
    pub(crate) fn eval(&self, x: f32) -> f32 {
        let x = if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let y = match self {
            Curve::Identity => x,
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(t) => {
                let last = t.len() - 1;
                let pos = x * last as f32;
                let i = (pos as usize).min(last - 1);
                let frac = pos - i as f32;
                let lo = t[i] as f32;
                let hi = t[i + 1] as f32;
                (lo + (hi - lo) * frac) / 65535.0
            }
            Curve::Para {
                g,
                a,
                b,
                c,
                d,
                e,
                f,
            } => {
                if x >= *d {
                    (a * x + b).max(0.0).powf(*g) + e
                } else {
                    c * x + f
                }
            }
        };
        if !y.is_finite() {
            return 0.0;
        }
        y.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn para(kind: u16, params: &[i32]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"para");
        data.extend_from_slice(&[0; 4]);
        data.extend_from_slice(&kind.to_be_bytes());
        data.extend_from_slice(&[0; 2]);
        for p in params {
            data.extend_from_slice(&p.to_be_bytes());
        }
        data
    }

    const ONE: i32 = 0x0001_0000;

    /// Clause 10.5: n = 0 is identity, n = 1 a u8Fixed8 gamma, larger n a
    /// linearly interpolated table.
    #[test]
    fn curv_forms() {
        let mut data = b"curv\0\0\0\0\0\0\0\0".to_vec();
        let (c, size) = Curve::parse(&data).unwrap();
        assert_eq!(c, Curve::Identity);
        assert_eq!(size, 12);

        data[11] = 1;
        data.extend_from_slice(&[0x02, 0x00]);
        let (c, size) = Curve::parse(&data).unwrap();
        assert_eq!(c, Curve::Gamma(2.0));
        assert_eq!(size, 14);
        assert!(near(c.eval(0.5), 0.25, 1e-5));

        let mut table = b"curv\0\0\0\0\0\0\0\x03".to_vec();
        table.extend_from_slice(&0u16.to_be_bytes());
        table.extend_from_slice(&13107u16.to_be_bytes());
        table.extend_from_slice(&65535u16.to_be_bytes());
        let (c, size) = Curve::parse(&table).unwrap();
        assert_eq!(size, 18);
        assert!(near(c.eval(0.0), 0.0, 1e-6));
        assert!(near(c.eval(0.5), 0.2, 1e-4));
        assert!(near(c.eval(1.0), 1.0, 1e-6));
        assert!(near(c.eval(0.25), 0.1, 1e-4), "midpoint of first segment");
    }

    /// Table 65 type 0000h: y = x^g, hand-computed at g = 2,4.
    #[test]
    fn para_type0() {
        let data = para(0, &[(2.4 * 65536.0) as i32]);
        let (c, size) = Curve::parse(&data).unwrap();
        assert_eq!(size, 16);
        assert!(near(c.eval(0.5), 0.5f32.powf(2.4), 1e-4));
        assert!(near(c.eval(1.0), 1.0, 1e-5));
    }

    /// Table 65 type 0001h: y = (ax + b)^g above -b/a, 0 below. With a = 2,
    /// b = -1 (CIE 122 form), the toe cuts off below x = 0,5.
    #[test]
    fn para_type1() {
        let data = para(1, &[2 * ONE, 2 * ONE, -ONE]);
        let (c, _) = Curve::parse(&data).unwrap();
        assert_eq!(c.eval(0.25), 0.0);
        assert!(near(c.eval(0.75), 0.25, 1e-4), "(2*0.75-1)^2");
        assert!(near(c.eval(1.0), 1.0, 1e-4));
    }

    /// Table 65 type 0002h: y = (ax + b)^g + c above -b/a, c below.
    #[test]
    fn para_type2() {
        let quarter = ONE / 4;
        let data = para(2, &[2 * ONE, 2 * ONE, -ONE, quarter]);
        let (c, _) = Curve::parse(&data).unwrap();
        assert!(near(c.eval(0.1), 0.25, 1e-4), "below -b/a the value is c");
        assert!(near(c.eval(0.75), 0.5, 1e-4), "(2*0.75-1)^2 + 0.25");
    }

    /// Table 65 type 0003h (IEC 61966-2-1): the exact sRGB parameters
    /// reproduce the sRGB decode at hand-computed points.
    #[test]
    fn para_type3_srgb() {
        let fx = |v: f64| (v * 65536.0).round() as i32;
        let data = para(
            3,
            &[
                fx(2.4),
                fx(1.0 / 1.055),
                fx(0.055 / 1.055),
                fx(1.0 / 12.92),
                fx(0.04045),
            ],
        );
        let (c, size) = Curve::parse(&data).unwrap();
        assert_eq!(size, 32);
        assert!(near(c.eval(0.02), 0.02 / 12.92, 1e-4));
        let want = ((0.5 + 0.055) / 1.055f32).powf(2.4);
        assert!(near(c.eval(0.5), want, 1e-4));
    }

    /// Table 65 type 0004h, read as y = (ax + b)^g + e above d and
    /// y = cx + f below. The printed table says "+ c" in the power branch,
    /// but that reading leaves parameter e (present in the 28-byte, 7-value
    /// parameter list "g a b c d e f") referenced by no formula, and the
    /// type stops generalizing type 3 (e = f = 0). The same table row
    /// prints both branch conditions as "X > d" — they must be
    /// complementary — so the row demonstrably carries glyph errors, and
    /// the seven-parameter reading is the only self-consistent one.
    #[test]
    fn para_type4() {
        let tenth = (0.1 * 65536.0) as i32;
        let data = para(4, &[2 * ONE, ONE, 0, ONE / 2, ONE / 2, tenth, tenth]);
        let (c, size) = Curve::parse(&data).unwrap();
        assert_eq!(size, 40);
        assert!(near(c.eval(0.2), 0.2 * 0.5 + 0.1, 1e-4), "cx + f below d");
        assert!(near(c.eval(0.8), 0.64 + 0.1, 1e-4), "(x)^2 + e above d");
    }

    /// Hostile parameters produce clamped, finite output, never a panic.
    #[test]
    fn hostile_curves_stay_finite() {
        let (c, _) = Curve::parse(&para(0, &[-ONE])).unwrap();
        let v = c.eval(0.0);
        assert!(v.is_finite());
        let (c, _) = Curve::parse(&para(1, &[ONE, 0, 0])).unwrap();
        assert!(c.eval(0.5).is_finite());
        for bad in [
            &b"para\0\0\0\0\0\x05\0\0"[..],
            &b"para\0\0\0\0\0\0\0\0"[..],
            &b"curv\0\0\0\0\0\0\xFF\xFF"[..],
            &b"cccc\0\0\0\0\0\0\0\0"[..],
            &b"cu"[..],
        ] {
            assert!(Curve::parse(bad).is_err());
        }
        let ident = Curve::Identity;
        assert_eq!(ident.eval(f32::NAN), 0.0);
        assert_eq!(ident.eval(7.0), 1.0);
    }
}
