//! `pdfboss merge` and `pdfboss split`: combining selected pages from
//! several input files into one fresh document, or cutting one document
//! into consecutive-page parts.

use std::path::Path;

use pdfboss_core::Document;
use pdfboss_write::{merge_documents, split_document, Error as WriteError, WriteOptions};

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

/// Refuses a document carrying an `/Encrypt` entry, naming `path` in the
/// error. Shared by `cmd_merge` and `cmd_split`: neither copies encrypted
/// content into a fresh output.
fn reject_encrypted(doc: &Document, path: &Path) -> Result<(), String> {
    if doc
        .xref()
        .trailer
        .get("Encrypt")
        .is_some_and(|o| !o.is_null())
    {
        return Err(format!("{}: {}", path.display(), WriteError::EncryptedBase));
    }
    Ok(())
}
