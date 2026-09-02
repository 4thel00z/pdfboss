//! `pdfboss merge` and `pdfboss split`: combining selected pages from
//! several input files into one fresh document, or cutting one document
//! into consecutive-page parts. Also `pdfboss rotate`: turning selected
//! pages by a quarter-turn multiple, by appending an incremental update
//! or writing a full rewrite. Also `pdfboss rewrite`: writing a whole
//! document fresh on its own, with no page change. Also `pdfboss overlay`:
//! drawing one file's first page onto every page of another, over or under
//! the content.

use std::path::Path;

use pdfboss_core::Document;
use pdfboss_write::{
    merge_documents, rewrite_document, rotate_pages, rotate_rewrite, split_document, watermark,
    watermark_under, watermark_under_with, watermark_with, Error as WriteError, Update,
    WriteOptions,
};

use crate::pages::{parse_ranges, pattern_path, split_input_spec};

/// Runs `pdfboss merge`: opens every input (splitting an optional
/// `FILE:RANGE` suffix), resolves each range against its own page count,
/// assembles the selected pages into a fresh document in argument order,
/// and writes it to `out`. `password` is tried against every input.
pub fn cmd_merge(inputs: &[String], out: &Path, password: &str) -> Result<(), String> {
    let mut sources = Vec::with_capacity(inputs.len());
    for spec in inputs {
        let (path, range) = split_input_spec(spec);
        let doc = Document::open_with_password(&path, password)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        reject_encrypted(&doc, &path)?;
        let indices = match range {
            Some(text) => Some(
                parse_ranges(&text, doc.page_count())
                    .map_err(|e| format!("{}: {e}", path.display()))?,
            ),
            None => None,
        };
        sources.push((doc, indices));
    }
    let selection: Vec<(&Document, Option<&[usize]>)> = sources
        .iter()
        .map(|(doc, indices)| (doc, indices.as_deref()))
        .collect();
    let count: usize = sources
        .iter()
        .map(|(doc, indices)| indices.as_ref().map_or(doc.page_count(), Vec::len))
        .sum();
    let bytes = merge_documents(&selection, WriteOptions::default()).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    let plural = if count == 1 { "" } else { "s" };
    println!("wrote {} ({count} page{plural})", out.display());
    Ok(())
}

/// Runs `pdfboss split`: cuts `file` into consecutive chunks of `every`
/// pages and writes each chunk to `out` with its 1-based part number
/// substituted for `%d`. `out` is validated before `file` is opened, so a
/// pattern with no `%d` fails without touching the input.
pub fn cmd_split(file: &Path, out: &str, every: usize, password: &str) -> Result<(), String> {
    pattern_path(out, 1)?;
    let doc = Document::open_with_password(file, password)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    reject_encrypted(&doc, file)?;
    let total = doc.page_count();
    let parts = split_document(&doc, every, WriteOptions::default()).map_err(|e| e.to_string())?;
    for (i, bytes) in parts.iter().enumerate() {
        let path = pattern_path(out, i + 1)?;
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let start = i * every;
        let count = (start + every).min(total) - start;
        let plural = if count == 1 { "" } else { "s" };
        println!("wrote {} ({count} page{plural})", path.display());
    }
    Ok(())
}

/// Runs `pdfboss rotate`: rotates `pages` (1-based, e.g. `2,4-9`; every
/// page when omitted) of `file` by `by` degrees clockwise (`"90"`,
/// `"180"` or `"270"`, already validated by clap's `--by` choices), and
/// writes the result to `out`. Appends an incremental update by default;
/// `rewrite` asks for a full rewrite instead. Either mode refuses a page
/// inlined directly into `/Kids` with no object of its own: pdfboss does
/// not yet restructure such a page to rotate it.
pub fn cmd_rotate(
    file: &Path,
    out: &Path,
    pages: Option<&str>,
    by: &str,
    rewrite: bool,
    password: &str,
) -> Result<(), String> {
    let by: i32 = by
        .parse()
        .map_err(|_| format!("invalid --by value: {by}"))?;
    let doc = Document::open_with_password(file, password)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    reject_encrypted(&doc, file)?;
    let indices = match pages {
        Some(text) => {
            parse_ranges(text, doc.page_count()).map_err(|e| format!("{}: {e}", file.display()))?
        }
        None => (0..doc.page_count()).collect(),
    };
    let count = indices.len();
    if rewrite {
        let bytes = rotate_rewrite(&doc, &indices, by, WriteOptions::default())
            .map_err(|e| e.to_string())?;
        std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    } else {
        let mut update = Update::new(&doc).map_err(|e| e.to_string())?;
        rotate_pages(&mut update, &indices, by).map_err(|e| e.to_string())?;
        update
            .save(out)
            .map_err(|e| format!("{}: {e}", out.display()))?;
    }
    let plural = if count == 1 { "" } else { "s" };
    println!("wrote {} ({count} page{plural} rotated)", out.display());
    Ok(())
}

/// Runs `pdfboss rewrite`: rewrites `file` fresh through the `Writer`,
/// recompressing streams and dropping unreachable objects and earlier
/// update sections, and writes the result to `out`.
pub fn cmd_rewrite(file: &Path, out: &Path, password: &str) -> Result<(), String> {
    let doc = Document::open_with_password(file, password)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    reject_encrypted(&doc, file)?;
    let bytes = rewrite_document(&doc, WriteOptions::default()).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Runs `pdfboss overlay`: draws the first page of `overlay` onto every
/// page of `file` and writes the result to `out`. On top of the content
/// by default; `under` draws beneath it instead. Appends an incremental
/// update by default; `rewrite` asks for a full rewrite. Both inputs are
/// refused when encrypted, each error naming its own file.
pub fn cmd_overlay(
    file: &Path,
    overlay: &Path,
    out: &Path,
    under: bool,
    rewrite: bool,
    password: &str,
) -> Result<(), String> {
    let doc = Document::open_with_password(file, password)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    reject_encrypted(&doc, file)?;
    let mark = Document::open_with_password(overlay, password)
        .map_err(|e| format!("{}: {e}", overlay.display()))?;
    reject_encrypted(&mark, overlay)?;
    let bytes = match (under, rewrite) {
        (false, false) => watermark(&doc, &mark),
        (true, false) => watermark_under(&doc, &mark),
        (false, true) => watermark_with(&doc, &mark, WriteOptions::default()),
        (true, true) => watermark_under_with(&doc, &mark, WriteOptions::default()),
    }
    .map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Refuses a document carrying an `/Encrypt` entry, naming `path` in the
/// error. Shared by `cmd_merge`, `cmd_split`, `cmd_rotate`, `cmd_rewrite`
/// and `cmd_overlay`: none of them copies encrypted content into a fresh
/// output. `cmd_overlay` calls this once per input rather than relying on
/// the library's own check, so each refusal names its own file: the
/// library would refuse an encrypted overlay too, but its error would not
/// say which file was encrypted.
fn reject_encrypted(doc: &Document, path: &Path) -> Result<(), String> {
    if doc.is_encrypted() {
        return Err(format!("{}: {}", path.display(), WriteError::EncryptedBase));
    }
    Ok(())
}
