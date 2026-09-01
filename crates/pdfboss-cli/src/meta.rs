//! `pdfboss meta`: update document metadata by appending an incremental update
//! to preserve the original PDF bytes.

use std::path::Path;

use pdfboss_core::Document;
use pdfboss_write::{Metadata, Update};

pub fn cmd_meta(file: &Path, out: &Path, set: &[String], password: &str) -> Result<(), String> {
    let meta = parse_assignments(set)?;
    let doc = Document::open_with_password(file, password).map_err(|e| format!("parse: {e}"))?;
    let mut update = Update::new(&doc).map_err(|e| e.to_string())?;
    update.set_metadata(meta).map_err(|e| e.to_string())?;
    update.save_appended(out).map_err(|e| e.to_string())
}

fn parse_assignments(set: &[String]) -> Result<Metadata, String> {
    let mut meta = Metadata::default();
    for pair in set {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("expected KEY=VALUE, got {pair:?}"))?;
        let slot = match key {
            "title" => &mut meta.title,
            "author" => &mut meta.author,
            "subject" => &mut meta.subject,
            "keywords" => &mut meta.keywords,
            "creator" => &mut meta.creator,
            "producer" => &mut meta.producer,
            other => {
                return Err(format!(
                    "unknown metadata key {other:?}: valid keys are title, author, subject, keywords, creator, producer"
                ))
            }
        };
        *slot = Some(value.to_string());
    }
    Ok(meta)
}
