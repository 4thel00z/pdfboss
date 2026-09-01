//! `pdfboss merge`: combining selected pages from several input files into
//! one fresh document.

use std::path::Path;

use pdfboss_core::Document;
use pdfboss_write::{merge_documents, Error as WriteError, WriteOptions};

use crate::pages::{parse_ranges, split_input_spec};

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
        if doc
            .xref()
            .trailer
            .get("Encrypt")
            .is_some_and(|o| !o.is_null())
        {
            return Err(format!("{}: {}", path.display(), WriteError::EncryptedBase));
        }
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
