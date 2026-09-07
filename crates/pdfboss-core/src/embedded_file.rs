//! File specifications and embedded files (ISO 32000-1 §7.11.2 to §7.11.4):
//! the strings and dictionaries that name a file, and the streams that
//! carry a file's bytes inside the document, listed through the catalog's
//! `/EmbeddedFiles` name tree.

use crate::date::Date;
use crate::error::{Error, Result};
use crate::names::{names_with, NameTree};
use crate::object::{decode_text_string, Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// A file specification (Table 44), from a dictionary or a bare string.
///
/// Covers ISO 32000-1 §7.11.3.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileSpec {
    /// `/F`: the file specification string as stored (§7.11.2), or the
    /// whole specification when it was a bare string.
    pub file: Option<Vec<u8>>,
    /// `/UF`: the Unicode form of the specification, decoded.
    pub unicode_file: Option<String>,
    /// `/Desc`, decoded.
    pub description: Option<String>,
    /// `/FS`: the file system that interprets the specification, `URL`
    /// being the one the standard defines.
    pub file_system: Option<String>,
    /// `/V`: whether the file changes often enough that it must not be
    /// cached.
    pub volatile: bool,
    /// The embedded file stream from `/EF`: its `/UF`, else `/F`, else one
    /// of the obsolescent `/DOS`, `/Mac` and `/Unix` entries.
    pub embedded: Option<ObjRef>,
}

impl FileSpec {
    /// The name to show: `/UF`, else `/F` decoded as a text string, else
    /// empty.
    ///
    /// Covers ISO 32000-1 §7.11.3.
    pub fn name(&self) -> String {
        self.unicode_file
            .clone()
            .or_else(|| self.file.as_deref().map(decode_text_string))
            .unwrap_or_default()
    }
}

/// The components of a file specification string (§7.11.2.1): the string
/// splits on every solidus not preceded by a reverse solidus, and the
/// reverse solidi that escape a solidus or each other are removed. An
/// absolute specification starts with an empty component.
///
/// Covers ISO 32000-1 §7.11.2.
pub fn spec_components(spec: &[u8]) -> Vec<Vec<u8>> {
    let mut components = vec![Vec::new()];
    let mut bytes = spec.iter();
    while let Some(&b) = bytes.next() {
        match b {
            b'\\' => {
                // The escaped byte is kept as is; a trailing reverse
                // solidus has nothing to escape and is dropped.
                if let Some(&escaped) = bytes.next() {
                    components.last_mut().expect("never empty").push(escaped);
                }
            }
            b'/' => components.push(Vec::new()),
            other => components.last_mut().expect("never empty").push(other),
        }
    }
    components
}

/// Reads a file specification from `object`: a dictionary per Table 44, or
/// a string, which is the `/F` of a specification with nothing else.
///
/// Covers ISO 32000-1 §7.11.2 and §7.11.3.
pub async fn file_spec_with<S: AsyncObjectSource>(src: &S, object: &Object) -> Option<FileSpec> {
    let dict = match src.resolve(object).await.ok()? {
        Object::String(bytes) => {
            return Some(FileSpec {
                file: Some(bytes),
                ..FileSpec::default()
            })
        }
        Object::Dict(dict) => dict,
        _ => return None,
    };
    let embedded = match dict.get("EF") {
        Some(ef) => match resolved_dict(src, ef).await {
            Some(ef) => ["UF", "F", "DOS", "Mac", "Unix"]
                .iter()
                .find_map(|key| ef.get(key).and_then(Object::as_ref)),
            None => None,
        },
        None => None,
    };
    Some(FileSpec {
        file: bytes_entry(src, &dict, "F").await,
        unicode_file: text_entry(src, &dict, "UF").await,
        description: text_entry(src, &dict, "Desc").await,
        file_system: match dict.get("FS") {
            Some(fs) => src
                .resolve(fs)
                .await
                .ok()
                .and_then(|fs| fs.as_name().map(|n| n.0.clone())),
            None => None,
        },
        volatile: match dict.get("V") {
            Some(v) => src.resolve(v).await.ok().and_then(|v| v.as_bool()) == Some(true),
            None => false,
        },
        embedded,
    })
}

/// A string entry's bytes, resolving an indirect one.
async fn bytes_entry<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Vec<u8>> {
    let value = src.resolve(dict.get(key)?).await.ok()?;
    value.as_str_bytes().map(<[u8]>::to_vec)
}

/// A text string entry, decoded.
async fn text_entry<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<String> {
    bytes_entry(src, dict, key)
        .await
        .map(|bytes| decode_text_string(&bytes))
}

/// A date entry, parsed.
async fn date_entry<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Date> {
    let bytes = bytes_entry(src, dict, key).await?;
    Date::parse_pdf(std::str::from_utf8(&bytes).ok()?)
}

/// One entry of the catalog's `/EmbeddedFiles` name tree: the name it is
/// filed under, its file specification, and the embedded file stream's
/// own entries (Tables 45 and 46).
///
/// Covers ISO 32000-1 §7.11.4.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedFile {
    /// The name tree key, decoded as a text string.
    pub name: String,
    /// The file specification the name maps to.
    pub spec: FileSpec,
    /// The stream's `/Subtype`: a MIME type, its name escapes undone.
    pub mime: Option<String>,
    /// `/Params /Size`: the uncompressed size in bytes.
    pub size: Option<u64>,
    /// `/Params /CreationDate`.
    pub created: Option<Date>,
    /// `/Params /ModDate`.
    pub modified: Option<Date>,
    /// `/Params /CheckSum`: the MD5 digest of the uncompressed bytes, as
    /// stored.
    pub checksum: Option<Vec<u8>>,
}

/// Every entry of the catalog's `/EmbeddedFiles` tree, in tree order; an
/// entry whose value is not a file specification is left out.
///
/// Covers ISO 32000-1 §7.11.4.
pub async fn embedded_files_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Vec<EmbeddedFile> {
    let mut files = Vec::new();
    for (key, value) in names_with(src, trailer, NameTree::EmbeddedFiles).await {
        let Some(spec) = file_spec_with(src, &value).await else {
            continue;
        };
        let stream = match spec.embedded {
            Some(r) => match src.get(r).await {
                Ok(Object::Stream(stream)) => Some(stream.dict),
                _ => None,
            },
            None => None,
        };
        let params = match stream.as_ref().and_then(|s| s.get("Params")) {
            Some(p) => resolved_dict(src, p).await,
            None => None,
        };
        let params = params.unwrap_or_default();
        files.push(EmbeddedFile {
            name: decode_text_string(&key),
            spec,
            mime: match stream.as_ref().and_then(|s| s.get("Subtype")) {
                Some(subtype) => src
                    .resolve(subtype)
                    .await
                    .ok()
                    .and_then(|s| s.as_name().map(|n| n.0.clone())),
                None => None,
            },
            size: match params.get("Size") {
                Some(size) => src
                    .resolve(size)
                    .await
                    .ok()
                    .and_then(|s| s.as_int())
                    .and_then(|s| u64::try_from(s).ok()),
                None => None,
            },
            created: date_entry(src, &params, "CreationDate").await,
            modified: date_entry(src, &params, "ModDate").await,
            checksum: bytes_entry(src, &params, "CheckSum").await,
        });
    }
    files
}

/// The decoded bytes of `file`'s embedded stream.
///
/// # Errors
///
/// `MissingKey("EF")` when the specification embeds no stream, a type
/// mismatch when the reference is not a stream, and the stream's own
/// decoding errors.
///
/// Covers ISO 32000-1 §7.11.4.
pub async fn embedded_file_data_with<S: AsyncObjectSource>(
    src: &S,
    file: &EmbeddedFile,
) -> Result<Vec<u8>> {
    let r = file.spec.embedded.ok_or(Error::MissingKey("EF"))?;
    match src.get(r).await? {
        Object::Stream(stream) => src.stream_data(&stream).await,
        other => Err(Error::TypeMismatch {
            expected: "stream",
            found: crate::document::type_name(&other),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document with `/Names` `/EmbeddedFiles` pointing at
    /// object 10; `objects` supplies 10 and up, `streams` any streams.
    fn doc(objects: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 10 0 R >> >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        for (num, dict, data) in streams {
            b.stream(*num, dict, data);
        }
        Document::load(b.build(1)).expect("load")
    }

    // Covers ISO 32000-1 §7.11.4 and §7.11.3.
    #[test]
    fn embedded_files_come_from_the_name_tree() {
        let doc = doc(
            &[
                (
                    10,
                    "<< /Names [(notes.txt) 11 0 R <FEFF00E90074> 13 0 R (external) 15 0 R (junk) 7] >>",
                ),
                (
                    11,
                    "<< /Type /Filespec /F (notes.txt) /UF (notes.txt) /Desc <FEFF004E006F00740065> \
                     /EF << /F 12 0 R >> >>",
                ),
                (
                    13,
                    "<< /Type /Filespec /F (data/e#t.csv) /UF <FEFF00E9002E006300730076> \
                     /EF << /UF 14 0 R /F 12 0 R >> /V true >>",
                ),
                (15, "<< /Type /Filespec /FS /URL /F (http://example.com/x.pdf) >>"),
            ],
            &[
                (
                    12,
                    "/Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 5 \
                     /ModDate (D:20260907120000Z) /CheckSum <5d41402abc4b2a76b9719d911017c592> >>",
                    b"hello",
                ),
                (
                    14,
                    "/Type /EmbeddedFile /Subtype /text#2Fcsv /Params << /CreationDate (D:2025) >>",
                    b"a,b\n1,2\n",
                ),
            ],
        );
        let files = doc.embedded_files();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["notes.txt", "ét", "external"],
            "one per name, the number skipped"
        );

        let notes = &files[0];
        assert_eq!(notes.spec.name(), "notes.txt");
        assert_eq!(notes.spec.description.as_deref(), Some("Note"));
        assert_eq!(notes.mime.as_deref(), Some("text/plain"));
        assert_eq!(notes.size, Some(5));
        assert_eq!(
            notes.modified.map(|d| d.to_pdf_string()),
            Some("D:20260907120000Z".to_string())
        );
        assert_eq!(
            notes.checksum,
            Some(vec![
                0x5d, 0x41, 0x40, 0x2a, 0xbc, 0x4b, 0x2a, 0x76, 0xb9, 0x71, 0x9d, 0x91, 0x10, 0x17,
                0xc5, 0x92
            ])
        );
        assert!(!notes.spec.volatile);
        assert_eq!(doc.embedded_file_data(notes).unwrap(), b"hello");

        let csv = &files[1];
        assert_eq!(csv.spec.name(), "é.csv", "/UF wins over /F");
        assert_eq!(csv.spec.file.as_deref(), Some(b"data/e#t.csv".as_slice()));
        assert_eq!(
            csv.spec.embedded,
            Some(ObjRef { num: 14, gen: 0 }),
            "/EF /UF wins"
        );
        assert_eq!(csv.mime.as_deref(), Some("text/csv"));
        assert_eq!(csv.size, None);
        assert_eq!(csv.created.map(|d| d.year), Some(2025));
        assert!(csv.spec.volatile);
        assert_eq!(doc.embedded_file_data(csv).unwrap(), b"a,b\n1,2\n");

        let external = &files[2];
        assert_eq!(external.spec.file_system.as_deref(), Some("URL"));
        assert_eq!(external.spec.embedded, None);
        assert_eq!(external.mime, None);
        assert!(matches!(
            doc.embedded_file_data(external),
            Err(Error::MissingKey("EF"))
        ));
    }

    // Covers ISO 32000-1 §7.11.2.
    #[test]
    fn file_specification_strings_split_on_unescaped_solidus() {
        let parts = |s: &[u8]| -> Vec<String> {
            spec_components(s)
                .into_iter()
                .map(|c| String::from_utf8(c).unwrap())
                .collect()
        };
        // The clause's example: a reverse solidus keeps the solidus literal.
        assert_eq!(parts(b"in\\/out"), ["in/out"]);
        assert_eq!(parts(b"ArtFiles/Figure1.pdf"), ["ArtFiles", "Figure1.pdf"]);
        assert_eq!(
            parts(b"/usr/local/x.pdf"),
            ["", "usr", "local", "x.pdf"],
            "absolute"
        );
        assert_eq!(parts(b"a\\\\b"), ["a\\b"], "an escaped reverse solidus");
        assert_eq!(parts(b"dir/"), ["dir", ""], "an empty last component");
        assert_eq!(parts(b""), [""]);
    }

    // Covers ISO 32000-1 §7.11.3 and §7.11.2.
    #[test]
    fn a_file_specification_names_its_file_from_uf_then_f_then_the_legacy_keys() {
        // Objects 20 to 23 are specifications of different forms; the tree
        // itself is not consulted here.
        let doc = doc(
            &[
                (10, "<< /Names [] >>"),
                (20, "(readme.txt)"),
                (21, "<< /Type /Filespec /F <FEFF00E9> >>"),
                (
                    22,
                    "<< /Type /Filespec /DOS (X.EPS) /EF << /DOS 31 0 R >> >>",
                ),
                (23, "42"),
            ],
            &[],
        );
        let read = |num: u32| {
            let obj = Object::Ref(ObjRef { num, gen: 0 });
            crate::block_on(file_spec_with(&crate::Immediate(&doc), &obj))
        };
        // A bare string is the /F of a specification.
        let bare = read(20).unwrap();
        assert_eq!(bare.file.as_deref(), Some(b"readme.txt".as_slice()));
        assert_eq!(bare.name(), "readme.txt");
        assert_eq!(bare.embedded, None);
        // /F decodes as a text string when /UF is absent.
        let f_only = read(21).unwrap();
        assert_eq!(f_only.name(), "é");
        // The obsolescent keys still name an embedded stream.
        let legacy = read(22).unwrap();
        assert_eq!(legacy.embedded, Some(ObjRef { num: 31, gen: 0 }));
        assert_eq!(legacy.name(), "");
        assert_eq!(read(23), None, "not a specification");
    }

    // Covers ISO 32000-1 §7.11.4.
    #[test]
    fn documents_without_embedded_files_have_none() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let plain = Document::load(b.build(1)).unwrap();
        assert!(plain.embedded_files().is_empty());
    }
}
