//! The predefined CJK CMaps (ISO 32000-1 Table 118, plus ISO 32000-2's
//! UniAKR-UTF16-H): Adobe's BSD-licensed data files, packed verbatim into
//! one zlib archive per character collection (`assets/cmaps/`, provenance
//! and layout in the NOTICE there), decompressed once per collection and
//! parsed once per CMap on first use. The archives are compiled in only
//! behind the `predefined-cmaps` Cargo feature; without it every name but
//! the two Identity mappings resolves to `None` and callers degrade to the
//! Identity assumption they use today.

use super::CidCmap;
use std::sync::{Arc, OnceLock};

/// The character collections with packed data, in archive order.
#[cfg(feature = "predefined-cmaps")]
#[derive(Clone, Copy)]
enum Collection {
    Japan1,
    Gb1,
    Cns1,
    Korea1,
    Kr,
}

/// Which collection owns a predefined CMap name; `None` for names ISO 32000
/// does not predefine (an `/Encoding` naming one must embed it instead).
#[cfg(feature = "predefined-cmaps")]
fn collection_of(name: &str) -> Option<Collection> {
    match name {
        "83pv-RKSJ-H" | "90ms-RKSJ-H" | "90ms-RKSJ-V" | "90msp-RKSJ-H" | "90msp-RKSJ-V"
        | "90pv-RKSJ-H" | "Add-RKSJ-H" | "Add-RKSJ-V" | "EUC-H" | "EUC-V" | "Ext-RKSJ-H"
        | "Ext-RKSJ-V" | "H" | "V" | "UniJIS-UCS2-H" | "UniJIS-UCS2-V" | "UniJIS-UCS2-HW-H"
        | "UniJIS-UCS2-HW-V" | "UniJIS-UTF16-H" | "UniJIS-UTF16-V" => Some(Collection::Japan1),
        "GB-EUC-H" | "GB-EUC-V" | "GBpc-EUC-H" | "GBpc-EUC-V" | "GBK-EUC-H" | "GBK-EUC-V"
        | "GBKp-EUC-H" | "GBKp-EUC-V" | "GBK2K-H" | "GBK2K-V" | "UniGB-UCS2-H" | "UniGB-UCS2-V"
        | "UniGB-UTF16-H" | "UniGB-UTF16-V" => Some(Collection::Gb1),
        "B5pc-H" | "B5pc-V" | "HKscs-B5-H" | "HKscs-B5-V" | "ETen-B5-H" | "ETen-B5-V"
        | "ETenms-B5-H" | "ETenms-B5-V" | "CNS-EUC-H" | "CNS-EUC-V" | "UniCNS-UCS2-H"
        | "UniCNS-UCS2-V" | "UniCNS-UTF16-H" | "UniCNS-UTF16-V" => Some(Collection::Cns1),
        "KSC-EUC-H" | "KSC-EUC-V" | "KSCms-UHC-H" | "KSCms-UHC-V" | "KSCms-UHC-HW-H"
        | "KSCms-UHC-HW-V" | "KSCpc-EUC-H" | "UniKS-UCS2-H" | "UniKS-UCS2-V" | "UniKS-UTF16-H"
        | "UniKS-UTF16-V" => Some(Collection::Korea1),
        "UniAKR-UTF16-H" => Some(Collection::Kr),
        _ => None,
    }
}

/// The predefined CMap called `name`, or `None` when ISO 32000 does not
/// predefine it (or the `predefined-cmaps` feature left the data out).
/// Parsed once per name; `usecmap` dependencies resolve within the set.
pub fn predefined(name: &str) -> Option<Arc<CidCmap>> {
    match name {
        "Identity-H" => Some(Arc::clone(identity(false))),
        "Identity-V" => Some(Arc::clone(identity(true))),
        _ => load(name, 0),
    }
}

fn identity(vertical: bool) -> &'static Arc<CidCmap> {
    static H: OnceLock<Arc<CidCmap>> = OnceLock::new();
    static V: OnceLock<Arc<CidCmap>> = OnceLock::new();
    let cell = if vertical { &V } else { &H };
    cell.get_or_init(|| Arc::new(CidCmap::identity(vertical)))
}

/// A CID-to-Unicode mapping, inverted from a collection's Uni*-UTF16-H
/// CMap: that file maps UTF-16 code points to CIDs, and running it backward
/// answers what a CID means when no `/ToUnicode` does. Where several code
/// points share a CID the lowest wins — the unified ideograph rather than
/// its compatibility duplicate.
pub struct CidToUnicode {
    map: crate::hash::FastMap<u32, char>,
}

impl CidToUnicode {
    /// The Unicode scalar for `cid`, if the collection maps one.
    pub fn lookup(&self, cid: u32) -> Option<char> {
        self.map.get(&cid).copied()
    }
}

/// The CID-to-Unicode mapping for a `/CIDSystemInfo` `/Ordering`, built on
/// first use. `None` for unknown orderings (Identity included: its CIDs are
/// font-private and mean nothing outside the font).
pub fn cid_to_unicode(ordering: &str) -> Option<Arc<CidToUnicode>> {
    let (slot, name) = match ordering {
        "Japan1" => (0, "UniJIS-UTF16-H"),
        "GB1" => (1, "UniGB-UTF16-H"),
        "CNS1" => (2, "UniCNS-UTF16-H"),
        "Korea1" => (3, "UniKS-UTF16-H"),
        "KR" => (4, "UniAKR-UTF16-H"),
        _ => return None,
    };
    static INVERSES: [OnceLock<Option<Arc<CidToUnicode>>>; 5] = [const { OnceLock::new() }; 5];
    INVERSES[slot]
        .get_or_init(|| {
            let cmap = predefined(name)?;
            Some(Arc::new(invert(cmap.as_ref())))
        })
        .clone()
}

/// Runs a code-to-CID CMap backward. Codes arrive lowest first within each
/// layer (see `CidCmap::mappings`), so the first insert for a CID is the
/// lowest code point that reaches it.
fn invert(cmap: &CidCmap) -> CidToUnicode {
    let mut map = crate::hash::FastMap::default();
    cmap.mappings(&mut |len, lo, hi, cid| {
        for offset in 0..=hi.saturating_sub(lo) {
            let Some(c) = unicode_of(lo + offset, len) else {
                continue;
            };
            map.entry(cid.saturating_add(offset)).or_insert(c);
        }
    });
    CidToUnicode { map }
}

/// Reads a code of `len` bytes as UTF-16BE: two bytes are one unit, four a
/// surrogate pair. Unpaired surrogates answer `None`.
fn unicode_of(code: u32, len: u8) -> Option<char> {
    if len != 4 {
        return char::from_u32(code);
    }
    let units = [(code >> 16) as u16, code as u16];
    char::decode_utf16(units).next()?.ok()
}

#[cfg(feature = "predefined-cmaps")]
mod packed {
    use super::{collection_of, Collection};
    use crate::cmap::CidCmap;
    use crate::hash::FastMap;
    use std::io::Read;
    use std::ops::Range;
    use std::sync::{Arc, Mutex, OnceLock};

    static BLOBS: [&[u8]; 5] = [
        include_bytes!("../../assets/cmaps/adobe-japan1.bin"),
        include_bytes!("../../assets/cmaps/adobe-gb1.bin"),
        include_bytes!("../../assets/cmaps/adobe-cns1.bin"),
        include_bytes!("../../assets/cmaps/adobe-korea1.bin"),
        include_bytes!("../../assets/cmaps/adobe-kr.bin"),
    ];

    /// One decompressed collection archive: the concatenated CMap files and
    /// where each lives.
    struct Archive {
        data: Vec<u8>,
        index: FastMap<String, Range<usize>>,
    }

    /// Decompresses and indexes a collection on first use. `None` sticks if
    /// the compiled-in archive will not read, which only a corrupted build
    /// could cause.
    fn archive(collection: Collection) -> Option<&'static Archive> {
        static ARCHIVES: [OnceLock<Option<Archive>>; 5] = [const { OnceLock::new() }; 5];
        ARCHIVES[collection as usize]
            .get_or_init(|| unpack(BLOBS[collection as usize]))
            .as_ref()
    }

    /// Archive layout (documented in `assets/cmaps/NOTICE`): u32le count,
    /// per entry a u16le name length + name + u32le data length, then the
    /// payloads concatenated in entry order.
    fn unpack(blob: &[u8]) -> Option<Archive> {
        let mut data = Vec::new();
        flate2::read::ZlibDecoder::new(blob)
            .read_to_end(&mut data)
            .ok()?;
        let count = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
        let mut pos = 4;
        let mut sizes = Vec::with_capacity(count);
        for _ in 0..count {
            let name_len = u16::from_le_bytes(data.get(pos..pos + 2)?.try_into().ok()?) as usize;
            let name = String::from_utf8(data.get(pos + 2..pos + 2 + name_len)?.to_vec()).ok()?;
            pos += 2 + name_len;
            let size = u32::from_le_bytes(data.get(pos..pos + 4)?.try_into().ok()?) as usize;
            pos += 4;
            sizes.push((name, size));
        }
        let mut index = FastMap::default();
        for (name, size) in sizes {
            let end = pos.checked_add(size)?;
            data.get(pos..end)?;
            index.insert(name, pos..end);
            pos = end;
        }
        Some(Archive { data, index })
    }

    /// Loads and caches one packed CMap, resolving its `usecmap` chain
    /// within the predefined set (bounded depth; the shipped chains are at
    /// most one deep).
    pub fn load(name: &str, depth: usize) -> Option<Arc<CidCmap>> {
        if depth > 4 {
            return None;
        }
        let collection = collection_of(name)?;
        static PARSED: OnceLock<Mutex<FastMap<String, Arc<CidCmap>>>> = OnceLock::new();
        let cache = PARSED.get_or_init(|| Mutex::new(FastMap::default()));
        if let Some(cmap) = cache.lock().unwrap().get(name) {
            return Some(Arc::clone(cmap));
        }
        let archive = archive(collection)?;
        let span = archive.index.get(name)?.clone();
        let mut resolve = |n: &str| super::predefined_at(n, depth + 1);
        let cmap = Arc::new(CidCmap::parse_with(&archive.data[span], None, &mut resolve));
        cache
            .lock()
            .unwrap()
            .insert(name.to_owned(), Arc::clone(&cmap));
        Some(cmap)
    }
}

#[cfg(feature = "predefined-cmaps")]
use packed::load;

#[cfg(not(feature = "predefined-cmaps"))]
fn load(_name: &str, _depth: usize) -> Option<Arc<CidCmap>> {
    None
}

/// [`predefined`] with the `usecmap` recursion depth threaded through.
#[cfg(feature = "predefined-cmaps")]
fn predefined_at(name: &str, depth: usize) -> Option<Arc<CidCmap>> {
    match name {
        "Identity-H" => Some(Arc::clone(identity(false))),
        "Identity-V" => Some(Arc::clone(identity(true))),
        _ => load(name, depth),
    }
}

#[cfg(all(test, feature = "predefined-cmaps"))]
mod tests {
    use super::*;

    #[test]
    fn rksj_h_splits_and_maps_hiragana() {
        let c = predefined("90ms-RKSJ-H").unwrap();
        assert!(!c.vertical());
        // あ is Shift-JIS <82A0>, inside the data file's range
        // `<829f> <82f1> 842`, and 1-byte ASCII splits stay 1 byte.
        let bytes = [0x41, 0x82, 0xA0];
        assert_eq!(c.code_at(&bytes, 0), (0x41, 1));
        assert_eq!(c.code_at(&bytes, 1), (0x82A0, 2));
        assert_eq!(c.cid(0x82A0, 2), Some(843));
        assert!(c.single_byte(0x20));
    }

    #[test]
    fn rksj_v_layers_vertical_variants_over_the_h_base() {
        let v = predefined("90ms-RKSJ-V").unwrap();
        assert!(v.vertical());
        // Its own `<8141> <8142> 7887` beats the inherited horizontal 、.
        assert_eq!(v.cid(0x8141, 2), Some(7887));
        // Untouched codes fall through to 90ms-RKSJ-H.
        assert_eq!(v.cid(0x82A0, 2), Some(843));
        let h = v.parent().expect("usecmap parent");
        assert_eq!(h.cid(0x8141, 2), Some(634));
    }

    #[test]
    fn every_shipped_name_loads_and_identity_needs_no_data() {
        for name in [
            "83pv-RKSJ-H",
            "90msp-RKSJ-V",
            "90pv-RKSJ-H",
            "Add-RKSJ-V",
            "EUC-H",
            "Ext-RKSJ-V",
            "H",
            "V",
            "UniJIS-UCS2-H",
            "UniJIS-UCS2-HW-V",
            "UniJIS-UTF16-V",
            "GB-EUC-V",
            "GBpc-EUC-H",
            "GBK-EUC-H",
            "GBKp-EUC-V",
            "GBK2K-H",
            "UniGB-UCS2-V",
            "UniGB-UTF16-H",
            "B5pc-V",
            "HKscs-B5-H",
            "ETen-B5-V",
            "ETenms-B5-H",
            "CNS-EUC-H",
            "UniCNS-UCS2-V",
            "UniCNS-UTF16-H",
            "KSC-EUC-H",
            "KSCms-UHC-V",
            "KSCms-UHC-HW-H",
            "KSCpc-EUC-H",
            "UniKS-UCS2-V",
            "UniKS-UTF16-H",
            "UniAKR-UTF16-H",
        ] {
            let c = predefined(name).unwrap_or_else(|| panic!("{name} must load"));
            assert!(!c.is_empty(), "{name} parsed to nothing");
            assert_eq!(c.vertical(), name.ends_with("-V") || name == "V", "{name}");
        }
        assert!(predefined("Identity-H").is_some());
        assert!(predefined("Identity-V").unwrap().vertical());
        assert!(predefined("UniJIS2004-UTF16-H").is_none()); // not Table 118
        assert!(predefined("WinAnsiEncoding").is_none());
    }

    #[test]
    fn japan1_cids_read_back_as_unicode() {
        let inv = cid_to_unicode("Japan1").unwrap();
        assert_eq!(inv.lookup(843), Some('あ'));
        // CID 1 is the space in every Adobe collection.
        assert_eq!(inv.lookup(1), Some(' '));
        assert!(cid_to_unicode("Identity").is_none());
        assert!(cid_to_unicode("Unknown").is_none());
    }
}
