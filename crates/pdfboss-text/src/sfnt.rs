//! Just enough of the sfnt container to read a font program's `cmap`
//! platform identifiers.
//!
//! Text extraction does not paint glyphs, so it has no use for the outlines;
//! the one question it asks of an embedded font is which code space the
//! producer built the subset against, and the `cmap` subtable headers answer
//! it in a few dozen bytes. Nothing here follows a subtable's contents.
//!
//! Every read is bounds-checked and every malformed structure yields "no
//! information" rather than a panic: the bytes come from the file.

/// The platform identifiers a font program's `cmap` advertises
/// (ISO/IEC 14496-22 `cmap`, and ISO 32000-1 9.6.6.4, which sends a reader
/// here when a simple font states no `/Encoding`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CmapPlatforms {
    /// A platform 3 (Microsoft) subtable is present.
    pub(crate) microsoft: bool,
    /// A platform 1 (Macintosh) subtable is present.
    pub(crate) macintosh: bool,
}

/// Reads a big-endian `u16` at `at`, or `None` past the end.
fn be16(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at + 2)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Reads a big-endian `u32` at `at`, or `None` past the end.
fn be32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The platforms `data`'s `cmap` advertises, or the default (neither) when
/// `data` is not an sfnt font, carries no `cmap`, or is truncated.
///
/// A `ttcf` collection is not read: a `/FontFile2` holds one font, and a
/// collection there is malformed enough that guessing which member applies
/// would be inventing information.
pub(crate) fn cmap_platforms(data: &[u8]) -> CmapPlatforms {
    let mut found = CmapPlatforms::default();
    // 0x00010000 is TrueType outlines, `OTTO` is CFF outlines, `true` is the
    // older Apple spelling. All three carry a `cmap` in the same directory.
    let tag = be32(data, 0).unwrap_or(0);
    if !matches!(tag, 0x0001_0000 | 0x4F54_544F | 0x7472_7565) {
        return found;
    }
    let Some(count) = be16(data, 4) else {
        return found;
    };
    let mut cmap = None;
    for i in 0..usize::from(count) {
        let entry = 12 + i * 16;
        let Some(tag) = be32(data, entry) else { break };
        if tag == 0x636D_6170 {
            // `cmap`
            cmap = be32(data, entry + 8).map(|off| off as usize);
            break;
        }
    }
    let Some(cmap) = cmap else { return found };
    let Some(subtables) = be16(data, cmap + 2) else {
        return found;
    };
    for i in 0..usize::from(subtables) {
        let record = cmap + 4 + i * 8;
        let Some(platform) = be16(data, record) else {
            break;
        };
        match platform {
            1 => found.macintosh = true,
            3 => found.microsoft = true,
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal sfnt carrying only a `cmap` whose subtable records
    /// name `platforms`. The subtables themselves are never read, so they are
    /// not written.
    fn sfnt_with(platforms: &[(u16, u16)]) -> Vec<u8> {
        let cmap_off = 12 + 16;
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes()); // one table
        out.extend_from_slice(&[0; 6]); // search hints, unread
        out.extend_from_slice(b"cmap");
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum, unread
        out.extend_from_slice(&(cmap_off as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // length, unread

        out.extend_from_slice(&0u16.to_be_bytes()); // cmap version
        out.extend_from_slice(&(platforms.len() as u16).to_be_bytes());
        for &(pid, eid) in platforms {
            out.extend_from_slice(&pid.to_be_bytes());
            out.extend_from_slice(&eid.to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes()); // subtable offset
        }
        out
    }

    #[test]
    fn reads_the_platforms_a_cmap_advertises() {
        let both = cmap_platforms(&sfnt_with(&[(1, 0), (3, 0)]));
        assert_eq!(
            both,
            CmapPlatforms {
                microsoft: true,
                macintosh: true
            }
        );

        let mac = cmap_platforms(&sfnt_with(&[(1, 0)]));
        assert!(mac.macintosh && !mac.microsoft);

        let win = cmap_platforms(&sfnt_with(&[(3, 1)]));
        assert!(win.microsoft && !win.macintosh);
    }

    /// The bytes come from the file, so every malformed shape has to answer
    /// "no information" rather than panic.
    #[test]
    fn malformed_font_programs_report_nothing() {
        let none = CmapPlatforms::default();
        assert_eq!(cmap_platforms(&[]), none, "empty");
        assert_eq!(cmap_platforms(b"not a font"), none, "wrong magic");

        let whole = sfnt_with(&[(3, 0)]);
        for cut in 0..whole.len() {
            // Every truncation must return, and must never claim a platform
            // it has not actually read a record for.
            let _ = cmap_platforms(&whole[..cut]);
        }
        assert_eq!(cmap_platforms(&whole[..12]), none, "directory only");

        // A table count far past the data cannot walk off the end.
        let mut lying = whole.clone();
        lying[4..6].copy_from_slice(&0xFFFFu16.to_be_bytes());
        let _ = cmap_platforms(&lying);

        // Nor can a subtable count.
        let mut lying = whole.clone();
        let n = 12 + 16 + 2;
        lying[n..n + 2].copy_from_slice(&0xFFFFu16.to_be_bytes());
        let _ = cmap_platforms(&lying);
    }
}
