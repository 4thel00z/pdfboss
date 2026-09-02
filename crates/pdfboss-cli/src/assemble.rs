//! `pdfboss merge` and `pdfboss split`: combining selected pages from
//! several input files into one fresh document, or cutting one document
//! into consecutive-page parts. Also `pdfboss rotate`: turning selected
//! pages by a quarter-turn multiple, by appending an incremental update
//! or writing a full rewrite. Also `pdfboss rewrite`: writing a whole
//! document fresh on its own, with no page change. Also `pdfboss overlay`:
//! drawing one file's first page onto every page of another, over or under
//! the content. Also `pdfboss encrypt` and `pdfboss decrypt`: writing a
//! fresh AES-256 protected copy of a document, or a fresh plain copy of one
//! that was encrypted.

use std::path::Path;

use pdfboss_core::{Document, Permissions};
use pdfboss_write::{
    decrypt_document, encrypt_document, merge_documents, rewrite_document, rotate_pages,
    rotate_rewrite, split_document, watermark, watermark_under, watermark_under_with,
    watermark_with, Error as WriteError, Update, WriteOptions,
};

use crate::pages::{parse_ranges, pattern_path, split_input_spec};
use crate::Failure;

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

/// Runs `pdfboss encrypt`: AES-256 protects `file` under `user_password`
/// and/or `owner_password` (ISO 32000-2 §7.6.4.3) and writes a fresh output
/// to `out`, restricted by `allow`. `password` opens `file` first, so an
/// already-encrypted input is re-encrypted under the new passwords once its
/// content reads as plaintext. At least one of `user_password`/
/// `owner_password` must be non-empty; that refusal is raised here, with
/// its own message, ahead of the library's own coarser one.
pub fn cmd_encrypt(
    file: &Path,
    out: &Path,
    user_password: &str,
    owner_password: &str,
    allow: Option<Vec<String>>,
    password: &str,
) -> Result<(), Failure> {
    if user_password.is_empty() && owner_password.is_empty() {
        return Err(Failure::new(
            "at least one of --user-password or --owner-password must be set",
        ));
    }
    let permissions = parse_allow(allow)?;
    let doc = Document::open_with_password(file, password)
        .map_err(|e| Failure::new(format!("{}: {e}", file.display())))?;
    let bytes = encrypt_document(
        &doc,
        user_password,
        owner_password,
        permissions,
        WriteOptions::default(),
    )
    .map_err(|e| Failure::new(e.to_string()))?;
    std::fs::write(out, bytes).map_err(|e| Failure::new(format!("{}: {e}", out.display())))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// Runs `pdfboss decrypt`: opens `file` under `password` (user or owner)
/// and writes a fresh, unencrypted output to `out`. A wrong or missing
/// password fails with the open error, naming `file` (the same error
/// format every other command in this file uses).
pub fn cmd_decrypt(file: &Path, out: &Path, password: &str) -> Result<(), String> {
    let doc = Document::open_with_password(file, password)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let bytes = decrypt_document(&doc, WriteOptions::default()).map_err(|e| e.to_string())?;
    std::fs::write(out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// The full list of `--allow` values, in the order named in its help text.
const ALLOW_VALUES: [&str; 8] = [
    "print",
    "modify",
    "copy",
    "annotate",
    "fill-forms",
    "accessibility",
    "assemble",
    "print-hires",
];

/// Parses `--allow` into a [`Permissions`]: every permission when `values`
/// is `None`, otherwise only the named ones. An unknown value fails with
/// exit code 2, naming both the offending value and the full accepted list.
fn parse_allow(values: Option<Vec<String>>) -> Result<Permissions, Failure> {
    let Some(values) = values else {
        return Ok(Permissions::all());
    };
    let mut permissions = Permissions {
        print: false,
        modify: false,
        copy: false,
        annotate: false,
        fill_forms: false,
        accessibility: false,
        assemble: false,
        print_hires: false,
    };
    for value in values {
        match value.as_str() {
            "print" => permissions.print = true,
            "modify" => permissions.modify = true,
            "copy" => permissions.copy = true,
            "annotate" => permissions.annotate = true,
            "fill-forms" => permissions.fill_forms = true,
            "accessibility" => permissions.accessibility = true,
            "assemble" => permissions.assemble = true,
            "print-hires" => permissions.print_hires = true,
            other => {
                return Err(Failure::program(format!(
                    "invalid value '{other}' for --allow: expected one of {}",
                    ALLOW_VALUES.join(", ")
                )))
            }
        }
    }
    Ok(permissions)
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
/// and `cmd_overlay`: the CLI refuses every encrypted input on every
/// assembly command, password-opened or not, regardless of the library's
/// own finer predicate underneath (some library entry points now accept a
/// password-opened source; this gate runs first and stays coarser on
/// purpose). `cmd_overlay` calls this once per input rather than relying
/// on a library check, so each refusal names its own file.
fn reject_encrypted(doc: &Document, path: &Path) -> Result<(), String> {
    if doc.is_encrypted() {
        return Err(format!("{}: {}", path.display(), WriteError::EncryptedBase));
    }
    Ok(())
}
