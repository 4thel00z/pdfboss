//! Date strings (ISO 32000-1 §7.9.4): `D:YYYYMMDDHHmmSSOHH'mm'`, read
//! leniently into [`Date`] and written back, or formatted as ISO 8601 for
//! XMP and display.

/// A calendar date and time with a UTC offset: the value of `/CreationDate`
/// and `/ModDate` in the document information dictionary, and of every
/// other date string in a file. The writer never reads a clock; dates
/// appear in output only when a caller provides them, keeping builds
/// reproducible.
///
/// Covers ISO 32000-1 §7.9.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// Four-digit year.
    pub year: u16,
    /// Month, 1–12.
    pub month: u8,
    /// Day of month, 1–31.
    pub day: u8,
    /// Hour, 0–23.
    pub hour: u8,
    /// Minute, 0–59.
    pub minute: u8,
    /// Second, 0–59.
    pub second: u8,
    /// Offset from UTC in minutes (positive east).
    pub utc_offset_minutes: i16,
}

impl Date {
    /// Formats as a PDF date string, `D:YYYYMMDDHHmmSSOHH'mm` — with a
    /// literal `Z` in place of the offset when the date is exactly UTC.
    ///
    /// Covers ISO 32000-1 §7.9.4.
    pub fn to_pdf_string(self) -> String {
        let Date {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes,
        } = self;
        let mut out = format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}");
        if utc_offset_minutes == 0 {
            out.push('Z');
            return out;
        }
        let sign = if utc_offset_minutes < 0 { '-' } else { '+' };
        let magnitude = utc_offset_minutes.unsigned_abs();
        out.push_str(&format!(
            "{sign}{:02}'{:02}",
            magnitude / 60,
            magnitude % 60
        ));
        out
    }

    /// Formats as an ISO-8601 date-time, `YYYY-MM-DDTHH:mm:SS±HH:MM` — with
    /// a literal `Z` in place of the offset when the date is exactly UTC.
    /// The form of the XMP `xmp:CreateDate`/`xmp:ModifyDate` elements and
    /// of the CLI's `info` output.
    ///
    /// Covers ISO 32000-1 §7.9.4.
    pub fn to_iso8601(self) -> String {
        let Date {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes,
        } = self;
        let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
        if utc_offset_minutes == 0 {
            out.push('Z');
            return out;
        }
        let sign = if utc_offset_minutes < 0 { '-' } else { '+' };
        let magnitude = utc_offset_minutes.unsigned_abs();
        out.push_str(&format!(
            "{sign}{:02}:{:02}",
            magnitude / 60,
            magnitude % 60
        ));
        out
    }

    /// Parses a PDF date string (ISO 32000 §7.9.4): the `D:` prefix (taken
    /// as optional, since producers omit it), a required 4-digit year, then
    /// optional 2-digit month, day, hour, minute and second, each present
    /// only if the one before it is (month and day default to 1, the rest
    /// to 0), then an optional UTC offset: `Z`, or `+`/`-HH` with optional
    /// `'mm`. Producers' deviations are taken too: a trailing apostrophe
    /// after the offset minutes, `Z` followed by offset digits, and offset
    /// minutes without the apostrophe. `None` when the year is missing, a
    /// present field is not digits or outside its range (month 1–12, day
    /// 1–31, hour 0–23, minute and second 0–59, offset hour 0–23 and
    /// minute 0–59), or the offset marker is neither `Z` nor a sign.
    ///
    /// Covers ISO 32000-1 §7.9.4.
    pub fn parse_pdf(s: &str) -> Option<Date> {
        let bytes = s.strip_prefix("D:").unwrap_or(s).as_bytes();
        let mut pos = 0;
        let year = take_required_digits(bytes, &mut pos, 4)? as u16;
        let month = take_optional_digits(bytes, &mut pos, 2)?.unwrap_or(1);
        let day = take_optional_digits(bytes, &mut pos, 2)?.unwrap_or(1);
        let hour = take_optional_digits(bytes, &mut pos, 2)?.unwrap_or(0);
        let minute = take_optional_digits(bytes, &mut pos, 2)?.unwrap_or(0);
        let second = take_optional_digits(bytes, &mut pos, 2)?.unwrap_or(0);
        let in_range = (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour <= 23
            && minute <= 59
            && second <= 59;
        if !in_range {
            return None;
        }
        let utc_offset_minutes = parse_pdf_offset(bytes, pos)?;
        Some(Date {
            year,
            month: month as u8,
            day: day as u8,
            hour: hour as u8,
            minute: minute as u8,
            second: second as u8,
            utc_offset_minutes,
        })
    }
}

/// Reads exactly `width` ASCII digits at `bytes[*pos..]` as a decimal
/// value, advancing `pos` past them. `None` when fewer than `width` bytes
/// remain or one of them is not a digit.
fn take_required_digits(bytes: &[u8], pos: &mut usize, width: usize) -> Option<u32> {
    let end = *pos + width;
    let digits = bytes.get(*pos..end)?;
    if !digits.iter().all(|d| d.is_ascii_digit()) {
        return None;
    }
    *pos = end;
    Some(
        digits
            .iter()
            .fold(0u32, |acc, &d| acc * 10 + u32::from(d - b'0')),
    )
}

/// [`take_required_digits`], but a field that does not start with a digit
/// (nothing left to read, or the offset marker next) is absent
/// (`Some(None)`) rather than malformed.
fn take_optional_digits(bytes: &[u8], pos: &mut usize, width: usize) -> Option<Option<u32>> {
    if !bytes.get(*pos).is_some_and(u8::is_ascii_digit) {
        return Some(None);
    }
    take_required_digits(bytes, pos, width).map(Some)
}

/// Parses the UTC offset trailing a PDF date string's digits, in minutes:
/// zero when nothing is left to read, zero for `Z` (whatever follows it),
/// or the signed magnitude of `+`/`-HH'mm`, the `'mm` half optional and
/// also taken without its apostrophe. `None` when what remains is neither,
/// or the hours or minutes are out of range.
fn parse_pdf_offset(bytes: &[u8], pos: usize) -> Option<i16> {
    if pos >= bytes.len() || bytes[pos] == b'Z' {
        return Some(0);
    }
    let sign = match bytes[pos] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let mut cursor = pos + 1;
    let hours = take_required_digits(bytes, &mut cursor, 2)?;
    if bytes.get(cursor) == Some(&b'\'') {
        cursor += 1;
    }
    let minutes = take_optional_digits(bytes, &mut cursor, 2)?.unwrap_or(0);
    if hours > 23 || minutes > 59 {
        return None;
    }
    let magnitude = i16::try_from(hours * 60 + minutes).ok()?;
    Some(sign * magnitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        utc_offset_minutes: i16,
    ) -> Date {
        Date {
            year,
            month,
            day,
            hour,
            minute,
            second,
            utc_offset_minutes,
        }
    }

    // Covers ISO 32000-1 §7.9.4.
    #[test]
    fn date_utc_formats_with_z() {
        assert_eq!(
            date(2026, 8, 27, 12, 30, 15, 0).to_pdf_string(),
            "D:20260827123015Z"
        );
    }

    // Covers ISO 32000-1 §7.9.4.
    #[test]
    fn date_positive_offset_pads_single_digits() {
        assert_eq!(
            date(987, 1, 2, 3, 4, 5, 120).to_pdf_string(),
            "D:09870102030405+02'00"
        );
    }

    // Covers ISO 32000-1 §7.9.4.
    #[test]
    fn date_negative_offset_keeps_minutes() {
        assert_eq!(
            date(1999, 12, 31, 23, 59, 58, -330).to_pdf_string(),
            "D:19991231235958-05'30"
        );
    }

    #[test]
    fn iso8601_utc_formats_with_z() {
        assert_eq!(
            date(2026, 8, 27, 12, 30, 15, 0).to_iso8601(),
            "2026-08-27T12:30:15Z"
        );
    }

    #[test]
    fn iso8601_positive_offset_pads_single_digits() {
        assert_eq!(
            date(987, 1, 2, 3, 4, 5, 120).to_iso8601(),
            "0987-01-02T03:04:05+02:00"
        );
    }

    #[test]
    fn iso8601_negative_offset_keeps_minutes() {
        assert_eq!(
            date(1999, 12, 31, 23, 59, 58, -330).to_iso8601(),
            "1999-12-31T23:59:58-05:30"
        );
    }

    // Covers ISO 32000-1 §7.9.4: the clause's own example.
    #[test]
    fn parse_pdf_the_clause_example() {
        assert_eq!(
            Date::parse_pdf("D:199812231952-08'00"),
            Some(date(1998, 12, 23, 19, 52, 0, -480))
        );
    }

    // Covers ISO 32000-1 §7.9.4: every field after the year is optional and
    // takes its default.
    #[test]
    fn parse_pdf_partial_fields_take_their_defaults() {
        assert_eq!(
            Date::parse_pdf("D:20260901"),
            Some(date(2026, 9, 1, 0, 0, 0, 0))
        );
        assert_eq!(
            Date::parse_pdf("D:2026"),
            Some(date(2026, 1, 1, 0, 0, 0, 0))
        );
        assert_eq!(
            Date::parse_pdf("D:20050321140532-05"),
            Some(date(2005, 3, 21, 14, 5, 32, -300))
        );
    }

    // Covers ISO 32000-1 §7.9.4: the forms producers actually write around
    // the grammar, a trailing apostrophe after the offset minutes, `Z`
    // followed by zero offset digits, an offset without apostrophes, and no
    // `D:` prefix.
    #[test]
    fn parse_pdf_accepts_the_common_deviations() {
        assert_eq!(
            Date::parse_pdf("D:20030326135148+01'00'"),
            Some(date(2003, 3, 26, 13, 51, 48, 60))
        );
        assert_eq!(
            Date::parse_pdf("D:20050407135940Z00'00'"),
            Some(date(2005, 4, 7, 13, 59, 40, 0))
        );
        assert_eq!(
            Date::parse_pdf("D:20100316021332-0130"),
            Some(date(2010, 3, 16, 2, 13, 32, -90))
        );
        assert_eq!(
            Date::parse_pdf("20260901120000Z"),
            Some(date(2026, 9, 1, 12, 0, 0, 0))
        );
    }

    // Covers ISO 32000-1 §7.9.4: a field outside its range is not a date.
    #[test]
    fn parse_pdf_refuses_out_of_range_fields() {
        assert_eq!(Date::parse_pdf("D:191020717014604"), None, "month 20");
        assert_eq!(Date::parse_pdf("D:20261301"), None, "month 13");
        assert_eq!(Date::parse_pdf("D:20260900"), None, "day 0");
        assert_eq!(Date::parse_pdf("D:2026090124"), None, "hour 24");
        assert_eq!(Date::parse_pdf("D:202609011260"), None, "minute 60");
        assert_eq!(
            Date::parse_pdf("D:20260901120000+24'00"),
            None,
            "offset hour 24"
        );
        assert_eq!(
            Date::parse_pdf("D:20260901120000+02'60"),
            None,
            "offset minute 60"
        );
    }

    #[test]
    fn parse_pdf_rejects_garbage() {
        assert!(Date::parse_pdf("yesterday").is_none());
        assert!(Date::parse_pdf("02/04/1998 01:11:16 PM").is_none());
        assert!(Date::parse_pdf("D:2026090112000X").is_none());
        assert!(Date::parse_pdf("").is_none());
    }

    #[test]
    fn parse_roundtrips_to_pdf_string() {
        let date = date(2026, 8, 27, 12, 30, 15, -330);
        assert_eq!(Date::parse_pdf(&date.to_pdf_string()), Some(date));
    }
}
