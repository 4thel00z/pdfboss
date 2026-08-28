//! The `pdfboss` command-line tool: document info, text extraction, page
//! rendering and object inspection.

mod create;
mod hexdump;
mod input;
mod json;
mod q;

use pdfboss_core::pretty;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use pdfboss_core::{Document, Error, Metadata, ObjRef, Object};
use pdfboss_output::Output as _;

use crate::input::is_url;

/// A fatal CLI failure: message for stderr plus the process exit code.
/// PDF/IO problems exit 1; invalid jq programs exit 2 (mirroring clap's own
/// usage-error code and keeping the two failure kinds distinguishable).
pub struct Failure {
    pub message: String,
    pub code: i32,
}

impl Failure {
    /// A PDF/IO failure (exit code 1).
    pub fn new(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            code: 1,
        }
    }

    /// An invalid-program failure (exit code 2).
    pub fn program(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            code: 2,
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Failure {
        Failure::new(message)
    }
}

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
    /// Create a new PDF: blank pages, word-wrapped text, image pages, or a
    /// themed Markdown document.
    Create {
        #[command(subcommand)]
        command: create::CreateCommand,
    },
    /// Show version, page count, page sizes and metadata.
    Info {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Extract text (all pages separated by form feed unless --page is given).
    Text {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// 1-based page number.
        #[arg(long)]
        page: Option<usize>,
    },
    /// Extract markdown (headings, lists, tables inferred from layout).
    Md {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// 1-based page number (heading sizes are then judged per page,
        /// not across the document).
        #[arg(long)]
        page: Option<usize>,
    },
    /// Render a page to PNG.
    Render {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
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
        /// Defaults to full when substitute faces are available (the
        /// compiled-in OFL set or --font-dir), otherwise all-embedded.
        #[arg(long, value_enum)]
        fonts: Option<FontsArg>,
        /// Directory of substitute faces for `--fonts full`: one file per
        /// face, named like `Arimo[wght].ttf` (the book's rendering chapter
        /// lists all of them), e.g. an installed `pdfboss-fonts` package.
        /// Overrides the compiled-in OFL set.
        #[arg(long)]
        font_dir: Option<PathBuf>,
        /// PNG compression: encode time against file size, same pixels.
        #[arg(long, value_enum, default_value_t = PngCompressionArg::Default)]
        png_compression: PngCompressionArg,
    },
    /// Extract every image a page draws, each as a native-size PNG.
    Images {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// 1-based page number (default: all pages).
        #[arg(long)]
        page: Option<usize>,
        /// Output directory (default: current directory).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// PNG compression: encode time against file size, same pixels.
        #[arg(long, value_enum, default_value_t = PngCompressionArg::Default)]
        png_compression: PngCompressionArg,
    },
    /// Pretty-print a single object.
    Obj {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// Object number.
        num: u32,
        /// Generation number (default 0).
        gen: Option<u16>,
    },
    /// Explore a PDF interactively in the terminal.
    Tui {
        /// Path or http(s) URL of the PDF.
        target: String,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Dump the document as a JSON value tree (for piping to external tools).
    Json {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// Embed raw (still encoded) stream data as base64.
        #[arg(long, conflicts_with = "decode")]
        raw: bool,
        /// Embed decoded stream data as base64.
        #[arg(long)]
        decode: bool,
        /// Restrict logical elements to these 1-based pages (comma separated).
        #[arg(long, value_delimiter = ',')]
        pages: Option<Vec<usize>>,
        /// Skip the logical layer (pages/fonts/images/annotations).
        #[arg(long)]
        no_logical: bool,
        /// Include per-page content-stream operators (high volume).
        #[arg(long)]
        content_ops: bool,
        /// Include per-page layout blocks (headings, paragraphs, lists, tables).
        #[arg(long)]
        layout: bool,
    },
    /// Hexdump the file or a selected element (hexyl-style).
    Hex {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        // Not a real intra-doc link: `[,G]` is the CLI's own bracket
        // notation for an optional generation number, not markdown link
        // syntax, but rustdoc parses it as one.
        #[allow(rustdoc::broken_intra_doc_links)]
        /// obj:N[,G] | header | xref:N | trailer | range:START-END
        /// (offsets decimal or 0x-hex; xref sections indexed in chain
        /// order, newest first). Default: the whole file.
        selector: Option<String>,
        /// Print labeled element boundaries as the dump crosses them.
        #[arg(long)]
        annotate: bool,
        /// Bytes per row.
        #[arg(long, default_value_t = 16)]
        width: usize,
    },
    /// Run a jq program over the document's JSON value tree.
    Q {
        /// Path or http(s) URL of the PDF.
        input: String,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// jq program, e.g. '.objects["12 0"]'.
        program: String,
        /// Embed raw (still encoded) stream data as base64.
        #[arg(long, conflicts_with = "decode")]
        raw: bool,
        /// Embed decoded stream data as base64.
        #[arg(long)]
        decode: bool,
        /// Hexdump results carrying a `_span` instead of printing JSON.
        #[arg(long)]
        hex: bool,
        /// Print string results raw, without quotes (like jq -r).
        #[arg(short = 'r')]
        raw_strings: bool,
        /// Restrict logical elements to these 1-based pages (comma separated).
        #[arg(long, value_delimiter = ',')]
        pages: Option<Vec<usize>>,
        /// Skip the logical layer (pages/fonts/images/annotations).
        #[arg(long)]
        no_logical: bool,
        /// Include per-page content-stream operators (high volume).
        #[arg(long)]
        content_ops: bool,
    },
}

/// `--fonts` choices for `render`, mapping to `pdfboss_render::GlyphPainting`.
#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
enum FontsArg {
    /// Only embedded TrueType outlines (fastest).
    EmbeddedOnly,
    /// Every embedded program.
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

/// Resolves an omitted `--fonts` to a tier: `full` when substitute faces
/// are at hand — an explicit `--font-dir`, or the compiled-in OFL set —
/// and `all-embedded` when neither is, so a default render paints
/// non-embedded fonts wherever it can and never errors over the choice.
fn default_fonts(font_dir: &Option<PathBuf>) -> FontsArg {
    if font_dir.is_some() || pdfboss_render::builtin_fonts_available() {
        FontsArg::Full
    } else {
        FontsArg::AllEmbedded
    }
}

/// `--png-compression` choices for `render`, mapping to
/// `pdfboss_render::PngCompression`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum PngCompressionArg {
    /// Uncompressed: fastest, largest files.
    None,
    /// Very fast with a decent ratio.
    Fast,
    /// Balances encode speed and file size (default).
    #[default]
    Default,
    /// Smallest files, much slower.
    Best,
}

impl PngCompressionArg {
    fn to_compression(self) -> pdfboss_render::PngCompression {
        use pdfboss_render::PngCompression;
        match self {
            PngCompressionArg::None => PngCompression::None,
            PngCompressionArg::Fast => PngCompression::Fast,
            PngCompressionArg::Default => PngCompression::Balanced,
            PngCompressionArg::Best => PngCompression::Best,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Create { command } => create::cmd_create(command).map_err(Failure::from),
        Command::Info { file, password } => cmd_info(&file, &password).map_err(Failure::from),
        Command::Text {
            file,
            page,
            password,
        } => cmd_text(&file, page, &password).map_err(Failure::from),
        Command::Md {
            file,
            page,
            password,
        } => cmd_md(&file, page, &password).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
            password,
            png_compression,
        } => cmd_render(
            &file,
            page,
            out,
            scale,
            fonts,
            font_dir,
            &password,
            png_compression,
        )
        .map_err(Failure::from),
        Command::Images {
            file,
            page,
            out,
            password,
            png_compression,
        } => cmd_images(&file, page, out, &password, png_compression).map_err(Failure::from),
        Command::Obj {
            file,
            num,
            gen,
            password,
        } => cmd_obj(&file, num, gen.unwrap_or(0), &password).map_err(Failure::from),
        Command::Tui { target, password } => cmd_tui(&target, &password).map_err(Failure::from),
        Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
            layout,
            password,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            json::cmd_json(&input, &flags, layout, &password).map_err(Failure::from)
        }
        Command::Hex {
            input,
            selector,
            annotate,
            width,
            password,
        } => hexdump::cmd_hex(&input, selector.as_deref(), annotate, width, &password)
            .map_err(Failure::from),
        Command::Q {
            input,
            program,
            raw,
            decode,
            hex,
            raw_strings,
            pages,
            no_logical,
            content_ops,
            password,
        } => {
            let flags = q::value::TreeFlags {
                raw,
                decode,
                pages,
                no_logical,
                content_ops,
            };
            q::run::cmd_q(&input, &program, &flags, hex, raw_strings, &password)
        }
    };
    if let Err(failure) = result {
        eprintln!("pdfboss: {}", failure.message);
        std::process::exit(failure.code);
    }
}

/// `pdfboss info`: prints version, encrypted flag, page count, per-page
/// sizes and the metadata table. Encrypted documents still report
/// successfully (with `encrypted: true`) since that is the very thing the
/// user is asking about.
fn cmd_info(file: &Path, password: &str) -> Result<(), String> {
    match Document::open_with_password(file, password) {
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
/// form feed. Extraction is lenient — content that will not read yields
/// no text rather than an error — so anything skipped is surfaced as a
/// stderr warning instead of vanishing.
fn cmd_text(file: &Path, page: Option<usize>, password: &str) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let text = match page {
        Some(n) => {
            let index = page_index(n, doc.page_count())?;
            let page = doc.page(index).map_err(|e| e.to_string())?;
            let (text, report) =
                pdfboss_output::extract_text_reporting(&doc, &page).map_err(|e| e.to_string())?;
            warn_skips(n, &report);
            text
        }
        None => {
            // Fanned out across the cores, one document fork per worker;
            // `map_pages` visits exactly the materializable pages (the
            // flattened tree, not the declared `/Count`, which on a damaged
            // file may not match what the tree yields) and returns them in
            // page order. One font cache serves every worker, so a font
            // loads once per document rather than once per page.
            let fonts = pdfboss_output::FontCache::default();
            let parts = pdfboss_core::map_pages(&doc, |doc, page| {
                pdfboss_output::extract_text_reporting_cached(doc, page, &fonts)
            })
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                let (text, report) = outcome.map_err(|e| e.to_string())?;
                warn_skips(index + 1, &report);
                Ok(text)
            })
            .collect::<Result<Vec<String>, String>>()?;
            parts.join("\u{c}")
        }
    };
    println!("{text}");
    Ok(())
}

/// `pdfboss md`: one page (1-based `--page`) or the whole document as
/// Markdown -- headings, lists and pipe/HTML tables inferred from layout.
/// Heading sizes rank against the whole document unless `--page` narrows to
/// one page, whose sizes are then judged only against themselves.
fn cmd_md(file: &Path, page: Option<usize>, password: &str) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let text = match page {
        Some(n) => {
            let index = page_index(n, doc.page_count())?;
            let page = doc.page(index).map_err(|e| e.to_string())?;
            let (spans, rulings, report) =
                pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page)
                    .map_err(|e| e.to_string())?;
            warn_skips(n, &report);
            pdfboss_output::Markdown
                .render(&[pdfboss_output::page_layout_with_rulings(&spans, &rulings)])
        }
        None => {
            let (md, reports) =
                pdfboss_output::extract_markdown_reporting(&doc).map_err(|e| e.to_string())?;
            for (index, report) in reports.iter().enumerate() {
                warn_skips(index + 1, report);
            }
            md
        }
    };
    println!("{text}");
    Ok(())
}

/// One stderr line per skipped stream, 1-based page numbers matching
/// `--page`. Warnings, not errors: the text on stdout is still everything
/// that could be read.
fn warn_skips(page_no: usize, report: &pdfboss_output::ExtractReport) {
    for skip in &report.skipped {
        eprintln!(
            "warning: page {page_no}: skipped {} ({})",
            skip.kind, skip.cause
        );
    }
}

/// Resolves `--fonts`/`--font-dir` into a [`pdfboss_render::SubstituteSource`].
///
/// `embedded-only`/`all-embedded` never substitute. `full` needs a face
/// source: an explicit `--font-dir` always wins; otherwise the compiled-in
/// OFL set is used if this binary was built with the `substitute-fonts`
/// feature. With neither, this is an actionable error rather than a silent
/// no-op -- the caller asked for substitution and would otherwise get a
/// render indistinguishable from `all-embedded` with no explanation why.
fn substitute_source(
    fonts: FontsArg,
    font_dir: Option<PathBuf>,
) -> Result<pdfboss_render::SubstituteSource, String> {
    use pdfboss_render::SubstituteSource;
    match fonts {
        FontsArg::EmbeddedOnly | FontsArg::AllEmbedded => Ok(SubstituteSource::None),
        FontsArg::Full => match font_dir {
            Some(dir) => Ok(SubstituteSource::Dir(dir)),
            None if pdfboss_render::builtin_fonts_available() => Ok(SubstituteSource::Builtin),
            None => Err(
                "--fonts full requested but no substitute faces are available: pass \
                 --font-dir <PATH> (a directory holding the substitute font files), or \
                 rebuild pdfboss with the default `substitute-fonts` feature (this \
                 binary was built without it) to bundle the OFL set."
                    .to_string(),
            ),
        },
    }
}

/// `pdfboss render`: rasterizes one page to a PNG file.
#[allow(clippy::too_many_arguments)]
fn cmd_render(
    file: &Path,
    page: usize,
    out: Option<PathBuf>,
    scale: f32,
    fonts: Option<FontsArg>,
    font_dir: Option<PathBuf>,
    password: &str,
    png_compression: PngCompressionArg,
) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(format!("invalid scale {scale}: must be a positive number"));
    }
    let fonts = fonts.unwrap_or_else(|| default_fonts(&font_dir));
    let substitutes = substitute_source(fonts, font_dir)?;
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let index = page_index(page, doc.page_count())?;
    let p = doc.page(index).map_err(|e| e.to_string())?;
    let opts = pdfboss_render::RenderOptions {
        glyph_painting: fonts.to_painting(),
        substitutes,
        ..Default::default()
    };
    let (pixmap, report) =
        pdfboss_render::render_page_reporting(&doc, &p, scale, &opts).map_err(|e| e.to_string())?;
    let out = out.unwrap_or_else(|| default_out(page));
    let png = pixmap
        .encode_png_with(png_compression.to_compression())
        .map_err(|e| e.to_string())?;
    std::fs::write(&out, png).map_err(|e| e.to_string())?;
    // Rendering is lenient, so a page whose content pdfboss could not read
    // still writes a PNG and still exits 0. Say what was lost, on stderr and
    // in the summary line, rather than reporting a clean render.
    for warning in report.warnings() {
        eprintln!("warning: page {page}: {warning}");
    }
    match report.summary() {
        Some(summary) => println!(
            "wrote {} ({} x {} px) [{}]",
            out.display(),
            pixmap.width,
            pixmap.height,
            summary
        ),
        None => println!(
            "wrote {} ({} x {} px)",
            out.display(),
            pixmap.width,
            pixmap.height
        ),
    }
    Ok(())
}

/// `pdfboss images`: writes every image the selected pages draw as
/// `page-N-image-M.png` (both numbers 1-based, M counting in drawing
/// order). Extraction is lenient like rendering, so a page whose images
/// cannot be decoded writes nothing for them and still exits 0.
fn cmd_images(
    file: &Path,
    page: Option<usize>,
    out: Option<PathBuf>,
    password: &str,
    png_compression: PngCompressionArg,
) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let pages = match page {
        Some(p) => vec![page_index(p, doc.page_count())?],
        None => (0..doc.page_count()).collect(),
    };
    let dir = out.unwrap_or_else(|| PathBuf::from("."));
    let mut written = 0usize;
    for index in pages {
        let p = doc.page(index).map_err(|e| e.to_string())?;
        let images = pdfboss_render::extract_page_images(&doc, &p).map_err(|e| e.to_string())?;
        for (i, pix) in images.iter().enumerate() {
            let path = dir.join(format!("page-{}-image-{}.png", index + 1, i + 1));
            let png = pix
                .encode_png_with(png_compression.to_compression())
                .map_err(|e| e.to_string())?;
            std::fs::write(&path, png).map_err(|e| e.to_string())?;
            println!(
                "wrote {} ({} x {} px)",
                path.display(),
                pix.width,
                pix.height
            );
            written += 1;
        }
    }
    match written {
        1 => println!("extracted 1 image"),
        n => println!("extracted {n} images"),
    }
    Ok(())
}

/// `pdfboss obj`: pretty-prints one indirect object. Stream objects print
/// their dictionary plus a decoded-length note instead of raw bytes.
fn cmd_obj(file: &Path, num: u32, gen: u16, password: &str) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
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

/// `pdfboss tui`: interactive explorer over a local file or an http(s)
/// URL, on a current-thread tokio runtime (rasterization uses the
/// runtime's blocking pool; the loop itself is single-threaded).
fn cmd_tui(target: &str, password: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let doc = open_async_document(target, password).await?;
        pdfboss_tui::run(doc, display_title(target))
            .await
            .map_err(|e| e.to_string())
    })
}

/// Builds the async document: the HTTP backend for URLs, the file backend
/// otherwise -- exactly the split `json`/`hex`/`q` already make via
/// `Input::open` (`pdfboss-aio`'s `http` feature is unconditionally on for
/// this crate, so there is no cfg gate to make here).
///
/// Both branches wrap the aio error with `target`, the same
/// `format!("{spec}: {err}")` shape `Input::open` uses for its local
/// `std::io::Error` failures: without it, a missing file or bad URL surfaces
/// only the layer-prefixed message ("io: No such file or directory") with
/// no indication of which target failed to open.
async fn open_async_document(
    target: &str,
    password: &str,
) -> Result<pdfboss_aio::AsyncDocument, String> {
    if is_url(target) {
        return pdfboss_aio::AsyncDocument::open_url_with_password(target, password)
            .await
            .map_err(|e| format!("{target}: {e}"));
    }
    pdfboss_aio::AsyncDocument::open_with_password(target, password)
        .await
        .map_err(|e| format!("{target}: {e}"))
}

/// The status-bar title: the last path/URL segment, or the whole target.
fn display_title(target: &str) -> String {
    target
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(target)
        .to_string()
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
    fn omitted_fonts_flag_resolves_by_face_availability() {
        let cli = Cli::parse_from(["pdfboss", "render", "in.pdf", "--page", "1"]);
        let Command::Render {
            fonts, font_dir, ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert!(fonts.is_none(), "no flag parses as no explicit tier");
        let expected = if pdfboss_render::builtin_fonts_available() {
            FontsArg::Full
        } else {
            FontsArg::AllEmbedded
        };
        assert_eq!(default_fonts(&font_dir), expected);
        assert_eq!(
            default_fonts(&Some(PathBuf::from("/faces"))),
            FontsArg::Full,
            "a --font-dir alone asks for substitution"
        );
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
        assert!(matches!(fonts, Some(FontsArg::EmbeddedOnly)));
    }

    #[test]
    fn fonts_full_with_font_dir_parses_to_dir_source() {
        let cli = Cli::parse_from([
            "pdfboss",
            "render",
            "in.pdf",
            "--page",
            "1",
            "--fonts",
            "full",
            "--font-dir",
            "X",
        ]);
        let Command::Render {
            fonts, font_dir, ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert!(matches!(fonts, Some(FontsArg::Full)));
        assert_eq!(font_dir, Some(PathBuf::from("X")));

        let source =
            substitute_source(FontsArg::Full, font_dir).expect("--font-dir given, always Ok");
        assert!(matches!(source, pdfboss_render::SubstituteSource::Dir(p) if p == Path::new("X")));
    }

    #[test]
    fn png_compression_flag_defaults_to_default_level() {
        let cli = Cli::parse_from(["pdfboss", "render", "in.pdf", "--page", "1"]);
        let Command::Render {
            png_compression, ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert!(matches!(png_compression, PngCompressionArg::Default));
    }

    #[test]
    fn png_compression_flag_parses_every_level() {
        for (value, expected) in [
            ("none", pdfboss_render::PngCompression::None),
            ("fast", pdfboss_render::PngCompression::Fast),
            ("default", pdfboss_render::PngCompression::Balanced),
            ("best", pdfboss_render::PngCompression::Best),
        ] {
            let cli = Cli::parse_from([
                "pdfboss",
                "render",
                "in.pdf",
                "--page",
                "1",
                "--png-compression",
                value,
            ]);
            let Command::Render {
                png_compression, ..
            } = cli.command
            else {
                panic!("expected render command");
            };
            assert_eq!(png_compression.to_compression(), expected, "{value}");
        }
    }

    #[test]
    fn png_compression_flag_rejects_unknown_levels() {
        let outcome = Cli::try_parse_from([
            "pdfboss",
            "render",
            "in.pdf",
            "--page",
            "1",
            "--png-compression",
            "bogus",
        ]);
        assert!(outcome.is_err());
    }

    #[test]
    fn font_dir_defaults_to_none() {
        let cli = Cli::parse_from(["pdfboss", "render", "in.pdf", "--page", "1"]);
        let Command::Render { font_dir, .. } = cli.command else {
            panic!("expected render command");
        };
        assert_eq!(font_dir, None);
    }

    #[test]
    fn embedded_only_and_all_embedded_never_substitute() {
        assert!(matches!(
            substitute_source(FontsArg::EmbeddedOnly, None),
            Ok(pdfboss_render::SubstituteSource::None)
        ));
        assert!(matches!(
            substitute_source(FontsArg::AllEmbedded, None),
            Ok(pdfboss_render::SubstituteSource::None)
        ));
        // Even if a --font-dir happens to be set, embedded-only/all-embedded
        // ignore it.
        assert!(matches!(
            substitute_source(FontsArg::AllEmbedded, Some(PathBuf::from("X"))),
            Ok(pdfboss_render::SubstituteSource::None)
        ));
    }

    /// Without `--font-dir`, `full`'s fallback depends on whether this binary
    /// was built with the `substitute-fonts` feature (a default feature, so
    /// this is the path `cargo install pdfboss-cli` users get).
    #[cfg(feature = "substitute-fonts")]
    #[test]
    fn full_without_font_dir_falls_back_to_builtin_faces() {
        assert!(matches!(
            substitute_source(FontsArg::Full, None),
            Ok(pdfboss_render::SubstituteSource::Builtin)
        ));
    }

    /// A `--no-default-features` build has no bundled faces, so `full` without
    /// `--font-dir` is the actionable-error path, naming both escape hatches.
    #[cfg(not(feature = "substitute-fonts"))]
    #[test]
    fn full_without_font_dir_or_feature_is_actionable_error() {
        let err = substitute_source(FontsArg::Full, None).expect_err("no dir, no feature");
        assert!(err.contains("--font-dir"));
        assert!(err.contains("substitute-fonts"));
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

    #[test]
    fn failure_from_string_exits_one() {
        let failure = Failure::from("boom".to_string());
        assert_eq!(failure.code, 1);
        assert_eq!(failure.message, "boom");
    }

    #[test]
    fn failure_program_exits_two() {
        let failure = Failure::program("bad program");
        assert_eq!(failure.code, 2);
        assert_eq!(failure.message, "bad program");
    }

    #[test]
    fn json_flags_parse() {
        let cli = Cli::parse_from([
            "pdfboss",
            "json",
            "in.pdf",
            "--raw",
            "--pages",
            "1,3",
            "--no-logical",
            "--content-ops",
            "--layout",
        ]);
        let Command::Json {
            input,
            raw,
            decode,
            pages,
            no_logical,
            content_ops,
            layout,
            password: _,
        } = cli.command
        else {
            panic!("expected json command");
        };
        assert_eq!(input, "in.pdf");
        assert!(raw && !decode && no_logical && content_ops && layout);
        assert_eq!(pages, Some(vec![1, 3]));
    }

    #[test]
    fn json_layout_flag_defaults_to_false() {
        let cli = Cli::parse_from(["pdfboss", "json", "in.pdf"]);
        let Command::Json { layout, .. } = cli.command else {
            panic!("expected json command");
        };
        assert!(!layout);
    }

    #[test]
    fn md_subcommand_parses_page_flag() {
        let cli = Cli::parse_from(["pdfboss", "md", "in.pdf", "--page", "2"]);
        let Command::Md { file, page, .. } = cli.command else {
            panic!("expected md command");
        };
        assert_eq!(file, PathBuf::from("in.pdf"));
        assert_eq!(page, Some(2));
    }

    #[test]
    fn md_subcommand_page_defaults_to_none() {
        let cli = Cli::parse_from(["pdfboss", "md", "in.pdf"]);
        let Command::Md { page, .. } = cli.command else {
            panic!("expected md command");
        };
        assert_eq!(page, None);
    }

    #[test]
    fn create_md_parses_theme_and_size() {
        let cli = Cli::try_parse_from([
            "pdfboss",
            "create",
            "md",
            "in.md",
            "-o",
            "out.pdf",
            "--theme",
            "dark.css",
            "--size",
            "letter",
            "--landscape",
        ])
        .unwrap();
        let Command::Create {
            command:
                create::CreateCommand::Md {
                    input,
                    out,
                    theme,
                    landscape,
                    ..
                },
        } = cli.command
        else {
            panic!("expected create md");
        };
        assert_eq!(input, PathBuf::from("in.md"));
        assert_eq!(out, PathBuf::from("out.pdf"));
        assert_eq!(theme, Some(PathBuf::from("dark.css")));
        assert!(landscape);
    }

    #[test]
    fn create_md_theme_defaults_to_none() {
        let cli =
            Cli::try_parse_from(["pdfboss", "create", "md", "in.md", "-o", "out.pdf"]).unwrap();
        let Command::Create {
            command: create::CreateCommand::Md { theme, .. },
        } = cli.command
        else {
            panic!("expected create md");
        };
        assert!(theme.is_none());
    }

    #[test]
    fn hex_flags_parse() {
        let cli = Cli::parse_from([
            "pdfboss",
            "hex",
            "in.pdf",
            "obj:12",
            "--annotate",
            "--width",
            "8",
        ]);
        let Command::Hex {
            input,
            selector,
            annotate,
            width,
            password: _,
        } = cli.command
        else {
            panic!("expected hex command");
        };
        assert_eq!(input, "in.pdf");
        assert_eq!(selector.as_deref(), Some("obj:12"));
        assert!(annotate);
        assert_eq!(width, 8);
    }

    #[test]
    fn q_flags_parse() {
        let cli = Cli::parse_from(["pdfboss", "q", "in.pdf", ".header", "--hex", "-r"]);
        let Command::Q {
            input,
            program,
            raw,
            decode,
            hex,
            raw_strings,
            ..
        } = cli.command
        else {
            panic!("expected q command");
        };
        assert_eq!(input, "in.pdf");
        assert_eq!(program, ".header");
        assert!(hex && raw_strings);
        assert!(!raw && !decode);
    }

    #[test]
    fn tui_subcommand_parses() {
        let cli = Cli::parse_from(["pdfboss", "tui", "in.pdf"]);
        let Command::Tui { target, .. } = cli.command else {
            panic!("expected tui command");
        };
        assert_eq!(target, "in.pdf");
    }

    #[test]
    fn url_detection() {
        assert!(is_url("https://example.com/a.pdf"));
        assert!(is_url("http://example.com/a.pdf"));
        assert!(!is_url("plain.pdf"));
        assert!(!is_url("dir/httpish.pdf"));
    }

    #[test]
    fn display_title_takes_last_segment() {
        assert_eq!(display_title("dir/sub/file.pdf"), "file.pdf");
        assert_eq!(display_title("file.pdf"), "file.pdf");
        assert_eq!(
            display_title("https://example.com/docs/spec.pdf"),
            "spec.pdf"
        );
        assert_eq!(display_title("trailing/"), "trailing/");
    }

    #[test]
    fn cmd_images_writes_each_drawn_image_as_png() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(
            4,
            "",
            b"q 50 0 0 50 0 0 cm /Im1 Do Q q 50 0 0 50 50 50 cm /Im1 Do Q",
        );
        b.stream(
            5,
            "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
             /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
        );
        let dir = std::env::temp_dir().join(format!("pdfboss-images-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pdf = dir.join("two-draws.pdf");
        std::fs::write(&pdf, b.build(1)).expect("fixture");
        cmd_images(
            &pdf,
            None,
            Some(dir.clone()),
            "",
            PngCompressionArg::Default,
        )
        .expect("extract");
        for name in ["page-1-image-1.png", "page-1-image-2.png"] {
            let png = std::fs::read(dir.join(name)).expect(name);
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name} is a PNG");
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn images_subcommand_parses_with_defaults() {
        let cli = Cli::parse_from(["pdfboss", "images", "in.pdf"]);
        let Command::Images {
            file, page, out, ..
        } = cli.command
        else {
            panic!("expected images command");
        };
        assert_eq!(file, PathBuf::from("in.pdf"));
        assert_eq!(page, None);
        assert_eq!(out, None);
    }
}
