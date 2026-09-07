//! Predictor post-pass (ISO 32000-1 §7.4.4.4) shared by Flate and LZW: 2 = TIFF horizontal
//! differencing at 1, 2, 4, 8 or 16 bits per component; >= 10 =
//! PNG filters applied per row (None/Sub/Up/Average/Paeth) with
//! `bpp = max(1, colors*bpc/8)` and row length `ceil(colors*bpc*columns/8)`.

use crate::error::Result;
use crate::filters::int_parm;
use crate::object::Dict;

/// Runs the predictor described by a `/DecodeParms` dictionary over freshly
/// decompressed data. Reads `/Predictor` (default 1 = none), `/Colors`
/// (default 1), `/BitsPerComponent` (default 8) and `/Columns` (default 1).
pub(crate) fn post_pass(mut data: Vec<u8>, parms: Option<&Dict>) -> Result<Vec<u8>> {
    let predictor = int_parm(parms, "Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = int_parm(parms, "Colors", 1).clamp(1, 128) as usize;
    let bpc = match int_parm(parms, "BitsPerComponent", 8) {
        v @ (1 | 2 | 4 | 8 | 16) => v as usize,
        _ => 8,
    };
    let columns = int_parm(parms, "Columns", 1).clamp(1, 1 << 24) as usize;
    // TIFF horizontal differencing on 8-bit components can run in place on the
    // buffer we already own, avoiding a full copy of the decompressed data.
    if predictor == 2 && bpc == 8 {
        tiff_horizontal_in_place(&mut data, colors, columns);
        return Ok(data);
    }
    apply(
        &data,
        predictor.min(i64::from(i32::MAX)) as i32,
        colors,
        bpc,
        columns,
    )
}

/// Reverses the predictor transform on decompressed data.
///
/// `predictor` 1 (or any unrecognised value) passes the data through
/// unchanged; 2 applies TIFF horizontal differencing at any of the five
/// component depths; values >= 10 treat the data as PNG filtered rows,
/// each prefixed with its filter-type byte. A truncated final row is
/// reconstructed as far as the data reaches.
///
/// Covers ISO 32000-1 §7.4.4.4.
pub fn apply(
    data: &[u8],
    predictor: i32,
    colors: usize,
    bpc: usize,
    columns: usize,
) -> Result<Vec<u8>> {
    let colors = colors.max(1);
    let columns = columns.max(1);
    let bpc = if matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        bpc
    } else {
        8
    };
    match predictor {
        2 => Ok(tiff_horizontal(data, colors, bpc, columns)),
        p if p >= 10 => Ok(png_rows(data, colors, bpc, columns)),
        _ => Ok(data.to_vec()),
    }
}

/// TIFF predictor 2: each colour component is stored as the difference
/// from the same component of the sample one pixel to the left; undo by
/// cumulative addition per row, modulo 2^bpc. Rows are
/// `ceil(colors * bpc * columns / 8)` bytes; 16-bit components are
/// big-endian, and sub-byte components are packed high bits first with a
/// row's padding bits left as stored.
///
/// Covers ISO 32000-1 §7.4.4.4.
fn tiff_horizontal(data: &[u8], colors: usize, bpc: usize, columns: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    match bpc {
        8 => tiff_horizontal_in_place(&mut out, colors, columns),
        16 => tiff_horizontal_16(&mut out, colors, columns),
        _ => tiff_horizontal_packed(&mut out, colors, bpc, columns),
    }
    out
}

/// Undoes TIFF horizontal differencing (8-bit components) in place.
fn tiff_horizontal_in_place(buf: &mut [u8], colors: usize, columns: usize) {
    let row_len = colors.saturating_mul(columns);
    if row_len == 0 {
        return;
    }
    for row in buf.chunks_mut(row_len) {
        for i in colors..row.len() {
            row[i] = row[i].wrapping_add(row[i - colors]);
        }
    }
}

/// Undoes TIFF horizontal differencing on big-endian 16-bit components in
/// place; a trailing odd byte is left alone.
fn tiff_horizontal_16(buf: &mut [u8], colors: usize, columns: usize) {
    let stride = colors.saturating_mul(2);
    let row_len = stride.saturating_mul(columns);
    if row_len == 0 {
        return;
    }
    for row in buf.chunks_mut(row_len) {
        for i in (stride..row.len().saturating_sub(1)).step_by(2) {
            let left = u16::from_be_bytes([row[i - stride], row[i - stride + 1]]);
            let cur = u16::from_be_bytes([row[i], row[i + 1]]);
            row[i..i + 2].copy_from_slice(&cur.wrapping_add(left).to_be_bytes());
        }
    }
}

/// Undoes TIFF horizontal differencing on 1-, 2- or 4-bit components in
/// place. Every row starts over: its first `colors` components are stored
/// as they are, and each later one adds the last decoded value of its own
/// colour.
fn tiff_horizontal_packed(buf: &mut [u8], colors: usize, bpc: usize, columns: usize) {
    let samples = colors.saturating_mul(columns);
    let row_len = samples.saturating_mul(bpc).div_ceil(8);
    if row_len == 0 {
        return;
    }
    let per_byte = 8 / bpc;
    let mask = (1u8 << bpc) - 1;
    let mut left = vec![0u8; colors];
    for row in buf.chunks_mut(row_len) {
        let count = samples.min(row.len() * per_byte);
        for s in 0..count {
            let byte = s / per_byte;
            let shift = 8 - bpc * (s % per_byte + 1);
            let stored = (row[byte] >> shift) & mask;
            let colour = s % colors;
            let value = if s < colors {
                stored
            } else {
                stored.wrapping_add(left[colour]) & mask
            };
            left[colour] = value;
            row[byte] = (row[byte] & !(mask << shift)) | (value << shift);
        }
    }
}

/// PNG predictors: every row is prefixed with a filter-type byte
/// (0 None, 1 Sub, 2 Up, 3 Average, 4 Paeth); unknown types are leniently
/// treated as None.
fn png_rows(data: &[u8], colors: usize, bpc: usize, columns: usize) -> Vec<u8> {
    let bpp = (colors.saturating_mul(bpc) / 8).max(1);
    let row_len = colors
        .saturating_mul(bpc)
        .saturating_mul(columns)
        .div_ceil(8)
        // Absurd column counts cannot need more than the data we have.
        .min(data.len());
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_len];
    let mut pos = 0;
    while pos < data.len() {
        let ftype = data[pos];
        pos += 1;
        let end = (pos + row_len).min(data.len());
        let mut row = data[pos..end].to_vec();
        pos = end;
        unfilter_row(ftype, &mut row, &prev, bpp);
        out.extend_from_slice(&row);
        row.resize(row_len, 0);
        prev = row;
    }
    out
}

fn unfilter_row(ftype: u8, row: &mut [u8], prev: &[u8], bpp: usize) {
    match ftype {
        1 => {
            // Sub: add the byte `bpp` to the left.
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        2 => {
            // Up: add the byte directly above.
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            // Average of left and above (floor).
            for i in 0..row.len() {
                let left = if i >= bpp { u16::from(row[i - bpp]) } else { 0 };
                let up = u16::from(prev[i]);
                row[i] = row[i].wrapping_add(((left + up) / 2) as u8);
            }
        }
        4 => {
            // Paeth: nearest of left, above and upper-left.
            for i in 0..row.len() {
                let left = if i >= bpp { row[i - bpp] } else { 0 };
                let up = prev[i];
                let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
                row[i] = row[i].wrapping_add(paeth(left, up, up_left));
            }
        }
        // 0 = None; unknown filter types leniently leave the row as-is.
        _ => {}
    }
}

/// The Paeth predictor: picks whichever of left/up/up-left is closest to
/// `left + up - up_left`, breaking ties in that order.
fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let p = i32::from(left) + i32::from(up) - i32::from(up_left);
    let pa = (p - i32::from(left)).abs();
    let pb = (p - i32::from(up)).abs();
    let pc = (p - i32::from(up_left)).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        up
    } else {
        up_left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward PNG filter (the inverse of `unfilter_row`) used to build
    /// test vectors from known raw rows.
    fn png_encode(rows: &[&[u8]], types: &[u8], bpp: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev = vec![0u8; rows[0].len()];
        for (row, &t) in rows.iter().zip(types) {
            out.push(t);
            for i in 0..row.len() {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                let pred = match t {
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    4 => paeth(a, b, c),
                    _ => 0,
                };
                out.push(row[i].wrapping_sub(pred));
            }
            prev = row.to_vec();
        }
        out
    }

    #[test]
    fn paeth_basic_and_tie_breaking() {
        assert_eq!(paeth(0, 0, 0), 0);
        // pa == pb: left wins over up.
        assert_eq!(paeth(5, 5, 3), 5);
        // pb == pc (both smaller than pa): up wins over up-left.
        assert_eq!(paeth(5, 2, 4), 2);
        // pc strictly smallest: up-left wins.
        assert_eq!(paeth(2, 4, 3), 3);
        // pa strictly smallest: left wins.
        assert_eq!(paeth(10, 100, 100), 10);
    }

    // The PNG and TIFF predictors of ISO 32000-1 §7.4.4.4, driven by the
    // `/Predictor`, `/Colors`, `/BitsPerComponent` and `/Columns` parameters.
    #[test]
    fn png_every_row_type_round_trips() {
        // colors=3, bpc=8, columns=2 -> row length 6, bpp 3.
        let rows: Vec<Vec<u8>> = vec![
            vec![1, 2, 3, 4, 5, 6],
            vec![10, 9, 8, 7, 6, 5],
            vec![100, 120, 140, 160, 180, 200],
            vec![0, 255, 1, 254, 2, 253],
            vec![50, 50, 50, 51, 51, 51],
        ];
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let types = [0u8, 1, 2, 3, 4];
        let encoded = png_encode(&refs, &types, 3);
        let expected: Vec<u8> = rows.concat();
        assert_eq!(apply(&encoded, 12, 3, 8, 2).unwrap(), expected);
    }

    #[test]
    fn png_paeth_only_rows() {
        let rows: Vec<Vec<u8>> = vec![vec![7, 200, 3, 90], vec![8, 201, 5, 89], vec![0, 0, 0, 0]];
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let encoded = png_encode(&refs, &[4, 4, 4], 1);
        assert_eq!(apply(&encoded, 15, 1, 8, 4).unwrap(), rows.concat());
    }

    // Covers ISO 32000-1 §7.4.4.4.
    #[test]
    fn png_row_length_rounds_up_for_sub_byte_samples() {
        // colors=1, bpc=1, columns=10 -> row length ceil(10/8) = 2, bpp 1.
        let rows: Vec<Vec<u8>> = vec![
            vec![0b1010_1010, 0b1100_0000],
            vec![0b0101_0101, 0b0100_0000],
        ];
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let encoded = png_encode(&refs, &[1, 2], 1);
        assert_eq!(apply(&encoded, 10, 1, 1, 10).unwrap(), rows.concat());
    }

    #[test]
    fn png_unknown_filter_type_passes_row_through() {
        let encoded = [7u8, 1, 2, 3];
        assert_eq!(apply(&encoded, 10, 1, 8, 3).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn png_truncated_final_row_decodes_prefix() {
        // Full Sub row then a partial Up row of one byte.
        let encoded = [1u8, 10, 10, 10, 2, 5];
        assert_eq!(apply(&encoded, 12, 1, 8, 3).unwrap(), vec![10, 20, 30, 15]);
    }

    // Covers ISO 32000-1 §7.4.4.4.
    #[test]
    fn tiff_horizontal_diff_round_trips() {
        let raw = [
            10u8, 20, 30, 15, 25, 35, 5, 0, 40, // row 1
            1, 2, 3, 4, 5, 6, 7, 8, 9, // row 2
        ];
        let encoded = [
            10u8, 20, 30, 5, 5, 5, 246, 231, 5, // row 1 diffed
            1, 2, 3, 3, 3, 3, 3, 3, 3, // row 2 diffed
        ];
        assert_eq!(apply(&encoded, 2, 3, 8, 3).unwrap(), raw);
    }

    // Covers ISO 32000-1 §7.4.4.4: 16-bit samples are big-endian and add
    // modulo 2^16, per row.
    #[test]
    fn tiff_16bit_samples_add_big_endian() {
        // colors=1, columns=3, two rows. Row 1 raw 0x0100 0x0203 0xFFFF;
        // row 2 raw 0x8000 0x8001 0x0000.
        let encoded = [
            0x01u8, 0x00, 0x01, 0x03, 0xFD, 0xFC, // row 1 diffed
            0x80, 0x00, 0x00, 0x01, 0x7F, 0xFF, // row 2 diffed
        ];
        let raw = [
            0x01u8, 0x00, 0x02, 0x03, 0xFF, 0xFF, //
            0x80, 0x00, 0x80, 0x01, 0x00, 0x00,
        ];
        assert_eq!(apply(&encoded, 2, 1, 16, 3).unwrap(), raw);
    }

    // Covers ISO 32000-1 §7.4.4.4: 4-bit samples add modulo 16 inside their
    // packed bytes, and each row starts over.
    #[test]
    fn tiff_4bit_samples_add_within_packed_bytes() {
        // colors=1, columns=4: raw samples 1 3 15 2 then 8 8 8 8.
        let encoded = [0x12u8, 0xC3, 0x80, 0x00];
        assert_eq!(
            apply(&encoded, 2, 1, 4, 4).unwrap(),
            [0x13, 0xF2, 0x88, 0x88]
        );
    }

    // Covers ISO 32000-1 §7.4.4.4: with several colours each sample adds the
    // same colour of the pixel to the left, sub-byte samples included.
    #[test]
    fn tiff_2bit_samples_add_per_colour() {
        // colors=2, columns=3: raw pixels (1,2) (3,0) (2,1) -> 01 10 11 00 10 01.
        let encoded = [0x6Au8, 0xD0];
        assert_eq!(apply(&encoded, 2, 2, 2, 3).unwrap(), [0x6C, 0x90]);
    }

    // Covers ISO 32000-1 §7.4.4.4: 1-bit samples add modulo 2, the row's
    // padding bits staying as stored.
    #[test]
    fn tiff_1bit_samples_add_modulo_two() {
        // colors=1, columns=10: raw bits 1010101011, padded to two bytes.
        let encoded = [0xFFu8, 0x80];
        assert_eq!(apply(&encoded, 2, 1, 1, 10).unwrap(), [0xAA, 0xC0]);
    }

    #[test]
    fn predictor_one_and_unknown_values_pass_through() {
        let data = [9u8, 8, 7];
        assert_eq!(apply(&data, 1, 1, 8, 3).unwrap(), data);
        assert_eq!(apply(&data, 5, 1, 8, 3).unwrap(), data);
        assert_eq!(apply(&data, 0, 1, 8, 3).unwrap(), data);
    }

    #[test]
    fn empty_data_stays_empty() {
        assert!(apply(&[], 12, 1, 8, 4).unwrap().is_empty());
        assert!(apply(&[], 2, 1, 8, 4).unwrap().is_empty());
    }

    #[test]
    fn degenerate_parameters_do_not_panic() {
        // colors/columns of zero are normalised to 1.
        let data = [0u8, 1, 2];
        assert_eq!(apply(&data, 10, 0, 8, 0).unwrap(), vec![1]);
    }

    // Covers ISO 32000-1 §7.4.4.3.
    #[test]
    fn post_pass_reads_parms_with_defaults() {
        use crate::object::{Name, Object};
        let mut d = Dict::new();
        d.insert(Name("Predictor".into()), Object::Int(2));
        d.insert(Name("Columns".into()), Object::Int(4));
        let data = vec![5u8, 2, 2, 2];
        assert_eq!(post_pass(data, Some(&d)).unwrap(), vec![5, 7, 9, 11]);
        // No parms: identity.
        assert_eq!(post_pass(vec![1, 2, 3], None).unwrap(), vec![1, 2, 3]);
    }
}
