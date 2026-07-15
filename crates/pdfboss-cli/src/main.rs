//! The `pdfboss` command-line tool: document info, text extraction, page
//! rendering and object inspection.

mod pretty;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use pdfboss_core::{Document, Error, Metadata, ObjRef, Object};

#[derive(Parser)]
#[command(
    name = "pdfboss",
    version,
    about = "PDF parsing, text extraction and rendering"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show version, page count, page sizes and metadata.
    Info {
        /// Path to the PDF file.
        file: PathBuf,
    },
    /// Extract text (all pages separated by form feed unless --page is given).
    Text {
        /// Path to the PDF file.
        file: PathBuf,
        /// 1-based page number.
        #[arg(long)]
        page: Option<usize>,
    },
    /// Render a page to PNG.
    Render {
        /// Path to the PDF file.
        file: PathBuf,
        /// 1-based page number.
        #[arg(long)]
        page: usize,
        /// Output file (default: page-N.png).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Scale factor.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Which fonts to paint: embedded-only, all-embedded, or full.
        #[arg(long, value_enum, default_value_t = FontsArg::AllEmbedded)]
        fonts: FontsArg,
    },
    /// Pretty-print a single object.
    Obj {
        /// Path to the PDF file.
        file: PathBuf,
        /// Object number.
        num: u32,
        /// Generation number (default 0).
        gen: Option<u16>,
    },
}

/// `--fonts` choices for `render`, mapping to `pdfboss_render::GlyphPainting`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum FontsArg {
    /// Only embedded TrueType outlines (fastest).
    EmbeddedOnly,
    /// Every embedded program (default).
    #[default]
    AllEmbedded,
    /// Also substitute bundled faces for non-embedded fonts.
    Full,
}

impl FontsArg {
    fn to_painting(self) -> pdfboss_render::GlyphPainting {
        use pdfboss_render::GlyphPainting;
        match self {
            FontsArg::EmbeddedOnly => GlyphPainting::EmbeddedTrueTypeOnly,
            FontsArg::AllEmbedded => GlyphPainting::AllEmbedded,
            FontsArg::Full => GlyphPainting::Full,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Info { file } => cmd_info(&file),
        Command::Text { file, page } => cmd_text(&file, page),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
        } => cmd_render(&file, page, out, scale, fonts),
        Command::Obj { file, num, gen } => cmd_obj(&file, num, gen.unwrap_or(0)),
    };
    if let Err(msg) = result {
        eprintln!("pdfboss: {msg}");
        std::process::exit(1);
    }
}

/// `pdfboss info`: prints version, encrypted flag, page count, per-page
/// sizes and the metadata table. Encrypted documents still report
/// successfully (with `encrypted: true`) since that is the very thing the
/// user is asking about.
fn cmd_info(file: &Path) -> Result<(), String> {
    match Document::open(file) {
        Ok(doc) => {
            let sizes: Vec<Option<(f32, f32)>> = (0..doc.page_count())
                .map(|i| doc.page(i).ok().map(|p| p.size()))
                .collect();
            print!(
                "{}",
                info_text(Some(doc.version()), false, Some(&sizes), &doc.metadata())
            );
            Ok(())
        }
        Err(Error::Encrypted) => {
            let data = std::fs::read(file).map_err(|e| e.to_string())?;
            print!(
                "{}",
                info_text(scan_version(&data), true, None, &Metadata::default())
            );
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Renders the `info` report. `sizes` is one entry per page (`None` when a
/// page failed to load); `None` for the whole slice means the page count is
/// unknown (encrypted document).
fn info_text(
    version: Option<(u8, u8)>,
    encrypted: bool,
    sizes: Option<&[Option<(f32, f32)>]>,
    meta: &Metadata,
) -> String {
    let mut out = String::new();
    match version {
        Some((major, minor)) => {
            let _ = writeln!(out, "version:   {major}.{minor}");
        }
        None => {
            let _ = writeln!(out, "version:   unknown");
        }
    }
    let _ = writeln!(out, "encrypted: {encrypted}");
    match sizes {
        Some(sizes) => {
            let _ = writeln!(out, "pages:     {}", sizes.len());
            for (i, size) in sizes.iter().enumerate() {
                match size {
                    Some((w, h)) => {
                        let _ = writeln!(out, "  page {}: {w} x {h} pt", i + 1);
                    }
                    None => {
                        let _ = writeln!(out, "  page {}: (unavailable)", i + 1);
                    }
                }
            }
        }
        None => {
            let _ = writeln!(out, "pages:     unknown");
        }
    }
    let rows: [(&str, &Option<String>); 8] = [
        ("title", &meta.title),
        ("author", &meta.author),
        ("subject", &meta.subject),
        ("keywords", &meta.keywords),
        ("creator", &meta.creator),
        ("producer", &meta.producer),
        ("created", &meta.creation_date),
        ("modified", &meta.mod_date),
    ];
    if rows.iter().any(|(_, v)| v.is_some()) {
        let _ = writeln!(out, "metadata:");
        for (label, value) in rows {
            if let Some(value) = value {
                let _ = writeln!(out, "  {label:<9} {value}");
            }
        }
    }
    out
}

/// Finds `%PDF-x.y` in the first KiB of `data` without loading the
/// document (used when the document is encrypted and cannot be opened).
fn scan_version(data: &[u8]) -> Option<(u8, u8)> {
    let window = &data[..data.len().min(1024)];
    let pos = window.windows(5).position(|w| w == b"%PDF-")?;
    let rest = &window[pos + 5..];
    let major = (*rest.first()? as char).to_digit(10)? as u8;
    if rest.get(1) != Some(&b'.') {
        return None;
    }
    let minor = (*rest.get(2)? as char).to_digit(10)? as u8;
    Some((major, minor))
}

/// `pdfboss text`: one page (1-based `--page`) or all pages joined by
/// form feed.
fn cmd_text(file: &Path, page: Option<usize>) -> Result<(), String> {
    let doc = Document::open(file).map_err(|e| e.to_string())?;
    let text = match page {
        Some(n) => {
            let index = page_index(n, doc.page_count())?;
            let page = doc.page(index).map_err(|e| e.to_string())?;
            pdfboss_text::extract_text(&doc, &page).map_err(|e| e.to_string())?
        }
        None => {
            // Drive iteration by successful page lookups: `page_count()` is
            // the declared `/Count`, which on a damaged file may not match the
            // pages the tree yields. `page(index)` fails only past the last
            // real page.
            let mut parts = Vec::new();
            let mut index = 0;
            while let Ok(page) = doc.page(index) {
                parts.push(pdfboss_text::extract_text(&doc, &page).map_err(|e| e.to_string())?);
                index += 1;
            }
            parts.join("\u{c}")
        }
    };
    println!("{text}");
    Ok(())
}

/// `pdfboss render`: rasterizes one page to a PNG file.
fn cmd_render(
    file: &Path,
    page: usize,
    out: Option<PathBuf>,
    scale: f32,
    fonts: FontsArg,
) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(format!("invalid scale {scale}: must be a positive number"));
    }
    let doc = Document::open(file).map_err(|e| e.to_string())?;
    let index = page_index(page, doc.page_count())?;
    let p = doc.page(index).map_err(|e| e.to_string())?;
    let opts = pdfboss_render::RenderOptions {
        glyph_painting: fonts.to_painting(),
        ..Default::default()
    };
    let pixmap = pdfboss_render::render_page_with_options(&doc, &p, scale, &opts)
        .map_err(|e| e.to_string())?;
    let out = out.unwrap_or_else(|| default_out(page));
    pixmap.save_png(&out).map_err(|e| e.to_string())?;
    println!(
        "wrote {} ({} x {} px)",
        out.display(),
        pixmap.width,
        pixmap.height
    );
    Ok(())
}

/// `pdfboss obj`: pretty-prints one indirect object. Stream objects print
/// their dictionary plus a decoded-length note instead of raw bytes.
fn cmd_obj(file: &Path, num: u32, gen: u16) -> Result<(), String> {
    let doc = Document::open(file).map_err(|e| e.to_string())?;
    let obj = doc.get(ObjRef { num, gen }).map_err(|e| e.to_string())?;
    match &obj {
        Object::Stream(s) => {
            println!("{}", pretty::format_dict(&s.dict));
            match doc.stream_data(s) {
                Ok(data) => println!("stream <{} bytes decoded>", data.len()),
                Err(e) => println!("stream <decode failed: {e}>"),
            }
        }
        other => println!("{}", pretty::format_object(other)),
    }
    Ok(())
}

/// Converts a 1-based page number into a 0-based index, validating range.
fn page_index(page: usize, count: usize) -> Result<usize, String> {
    if page == 0 || page > count {
        let plural = if count == 1 { "" } else { "s" };
        Err(format!(
            "page {page} out of range (document has {count} page{plural})"
        ))
    } else {
        Ok(page - 1)
    }
}

/// Default output path for `render`: `page-N.png`.
fn default_out(page: usize) -> PathBuf {
    PathBuf::from(format!("page-{page}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn fonts_flag_defaults_to_all_embedded() {
        let cli = Cli::parse_from(["pdfboss", "render", "in.pdf", "--page", "1"]);
        let Command::Render { fonts, .. } = cli.command else {
            panic!("expected render command");
        };
        assert!(matches!(fonts, FontsArg::AllEmbedded));
    }

    #[test]
    fn fonts_flag_parses_embedded_only() {
        let cli = Cli::parse_from([
            "pdfboss",
            "render",
            "in.pdf",
            "--page",
            "1",
            "--fonts",
            "embedded-only",
        ]);
        let Command::Render { fonts, .. } = cli.command else {
            panic!("expected render command");
        };
        assert!(matches!(fonts, FontsArg::EmbeddedOnly));
    }

    #[test]
    fn fonts_arg_maps_to_painting() {
        assert_eq!(
            FontsArg::EmbeddedOnly.to_painting(),
            pdfboss_render::GlyphPainting::EmbeddedTrueTypeOnly
        );
        assert_eq!(
            FontsArg::AllEmbedded.to_painting(),
            pdfboss_render::GlyphPainting::AllEmbedded
        );
        assert_eq!(
            FontsArg::Full.to_painting(),
            pdfboss_render::GlyphPainting::Full
        );
    }

    #[test]
    fn info_text_normal_document() {
        let sizes = [Some((612.0, 792.0))];
        let meta = Metadata {
            title: Some("Demo".to_string()),
            ..Metadata::default()
        };
        let report = info_text(Some((1, 7)), false, Some(&sizes), &meta);
        assert!(report.contains("version:   1.7"));
        assert!(report.contains("encrypted: false"));
        assert!(report.contains("pages:     1"));
        assert!(report.contains("page 1: 612 x 792 pt"));
        assert!(report.contains("title"));
        assert!(report.contains("Demo"));
    }

    #[test]
    fn info_text_encrypted_document() {
        let report = info_text(Some((1, 4)), true, None, &Metadata::default());
        assert!(report.contains("encrypted: true"));
        assert!(report.contains("pages:     unknown"));
        assert!(!report.contains("metadata:"));
    }

    #[test]
    fn info_text_unavailable_page() {
        let sizes = [None];
        let report = info_text(None, false, Some(&sizes), &Metadata::default());
        assert!(report.contains("version:   unknown"));
        assert!(report.contains("page 1: (unavailable)"));
    }

    #[test]
    fn scan_version_finds_header() {
        assert_eq!(scan_version(b"%PDF-1.7\n..."), Some((1, 7)));
        assert_eq!(scan_version(b"junk\n%PDF-2.0\n"), Some((2, 0)));
        assert_eq!(scan_version(b"no header here"), None);
        assert_eq!(scan_version(b"%PDF-x.y"), None);
        assert_eq!(scan_version(b""), None);
    }

    #[test]
    fn page_index_validates_range() {
        assert_eq!(page_index(1, 3), Ok(0));
        assert_eq!(page_index(3, 3), Ok(2));
        assert!(page_index(0, 3).is_err());
        assert!(page_index(4, 3).is_err());
        assert!(page_index(1, 0).is_err());
    }

    #[test]
    fn default_out_names_by_page() {
        assert_eq!(default_out(2), PathBuf::from("page-2.png"));
    }
}
