//! The `pdfboss` command-line tool: document info, text extraction, page
//! rendering and object inspection.

mod assemble;
mod create;
mod hexdump;
mod input;
mod json;
mod manifest;
mod meta;
mod pages;
mod progress;
mod q;
mod skill;

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
    /// Set document metadata by appending an update (original bytes preserved).
    Meta {
        /// Input PDF.
        file: PathBuf,
        /// Output PDF path.
        #[arg(short, long)]
        out: PathBuf,
        /// Metadata assignment, repeatable: title, author, subject, keywords, creator, producer.
        #[arg(long = "set", value_name = "KEY=VALUE", required = true)]
        set: Vec<String>,
        /// Full rewrite instead of an incremental append.
        #[arg(long)]
        rewrite: bool,
        /// Password for encrypted PDFs.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Combine selected pages from several inputs into one fresh document.
    Merge {
        /// Inputs, each optionally FILE:RANGE (1-based, e.g. report.pdf:2-9).
        #[arg(required = true)]
        inputs: Vec<String>,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// One password tried for every encrypted input.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Cut a document into consecutive chunks of pages.
    Split {
        /// Path to the PDF file.
        file: PathBuf,
        /// Output pattern containing %d (1-based part number).
        #[arg(short, long)]
        out: String,
        /// Pages per part.
        #[arg(long, value_parser = parse_every)]
        every: usize,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Rotate selected pages by a quarter-turn multiple, clockwise.
    Rotate {
        /// Path to the PDF file.
        file: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// 1-based pages, e.g. 2,4-9; every page when omitted.
        #[arg(long)]
        pages: Option<String>,
        /// Quarter turns clockwise: 90, 180 or 270.
        #[arg(long, value_parser = ["90", "180", "270"])]
        by: String,
        /// Full rewrite instead of an incremental append.
        #[arg(long)]
        rewrite: bool,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Draw the first page of another PDF onto every page.
    Overlay {
        /// Path to the PDF file.
        file: PathBuf,
        /// PDF whose first page is drawn onto every page.
        overlay: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Draw beneath the page content instead of on top of it.
        #[arg(long)]
        under: bool,
        /// Full rewrite instead of an incremental append.
        #[arg(long)]
        rewrite: bool,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Rewrite a document fresh: recompressed, unreachable objects and
    /// earlier update sections left behind.
    Rewrite {
        /// Path to the PDF file.
        file: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Encrypt a document with AES-256 (a fresh file is always written).
    Encrypt {
        /// Path to the PDF file.
        file: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Password readers must supply to open the file.
        #[arg(long, default_value = "")]
        user_password: String,
        /// Owner password; defaults to the user password when omitted.
        #[arg(long, default_value = "")]
        owner_password: String,
        /// Permissions granted to readers, comma-separated; all when omitted.
        /// Values: print, modify, copy, annotate, fill-forms, accessibility, assemble, print-hires.
        #[arg(long, value_delimiter = ',')]
        allow: Option<Vec<String>>,
        /// Password for reading an input that is itself encrypted.
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Remove encryption (opens with the password, writes a fresh plain file).
    Decrypt {
        /// Path to the PDF file.
        file: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Password for the encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
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
        /// The order lines are read in.
        #[arg(long, value_enum, default_value_t = ReadingOrderArg::Content)]
        reading_order: ReadingOrderArg,
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
        /// The order lines are read in.
        #[arg(long, value_enum, default_value_t = ReadingOrderArg::Content)]
        reading_order: ReadingOrderArg,
    },
    /// Render a page to PNG, PPM, BMP or JPEG.
    Render {
        /// Path to the PDF file.
        file: PathBuf,
        /// Password for an encrypted file (user or owner password).
        #[arg(long, default_value = "")]
        password: String,
        /// 1-based page number.
        #[arg(long)]
        page: usize,
        /// Output file; its extension picks the format, .png, .ppm, .bmp or
        /// .jpg (default: page-N.png).
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
        /// PNG compression: encode time against file size, same pixels
        /// (.ppm and .bmp are never compressed).
        #[arg(long, value_enum, default_value_t = PngCompressionArg::Default)]
        png_compression: PngCompressionArg,
        /// JPEG quality, 1 to 100 (.jpg and .jpeg only).
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u8).range(1..=100))]
        jpeg_quality: u8,
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
        /// Write each page's thumbnail image (page-N-thumb.png) instead of
        /// the images the page draws; pages without one are skipped.
        #[arg(long)]
        thumbnails: bool,
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
    /// Install or print the bundled Claude Code skill for coding agents.
    Skill {
        #[command(subcommand)]
        command: skill::SkillCommand,
    },
}

/// Parses `--every` as a positive page count. `usize` carries no
/// `clap::value_parser!` range support (unlike the fixed-width integers),
/// so the 1.. bound is checked by hand: 0 is rejected here rather than
/// reaching `split_document` as an unrepresentable chunk size.
fn parse_every(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("invalid value '{s}' for --every: not a number"))?;
    if n == 0 {
        return Err("invalid value '0' for --every: 0 is not in 1..".to_string());
    }
    Ok(n)
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

/// `--reading-order` values for `text` and `md`, mapped onto
/// `pdfboss_output::ReadingOrder`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum ReadingOrderArg {
    /// The content stream's order, corrected by geometry (default).
    #[default]
    Content,
    /// The structure tree's order on tagged pages, content order elsewhere.
    StructureTree,
    /// Position alone: lines top to bottom, left to right.
    Geometric,
}

impl ReadingOrderArg {
    fn to_order(self) -> pdfboss_output::ReadingOrder {
        use pdfboss_output::ReadingOrder;
        match self {
            ReadingOrderArg::Content => ReadingOrder::Content,
            ReadingOrderArg::StructureTree => ReadingOrder::StructureTree,
            ReadingOrderArg::Geometric => ReadingOrder::Geometric,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let result: Result<(), Failure> = match cli.command {
        Command::Create { command } => create::cmd_create(command).map_err(Failure::from),
        Command::Skill { command } => skill::cmd_skill(command).map_err(Failure::from),
        Command::Meta {
            file,
            out,
            set,
            rewrite,
            password,
        } => meta::cmd_meta(&file, &out, &set, rewrite, &password).map_err(Failure::from),
        Command::Merge {
            inputs,
            out,
            password,
        } => assemble::cmd_merge(&inputs, &out, &password).map_err(Failure::from),
        Command::Split {
            file,
            out,
            every,
            password,
        } => assemble::cmd_split(&file, &out, every, &password).map_err(Failure::from),
        Command::Rotate {
            file,
            out,
            pages,
            by,
            rewrite,
            password,
        } => assemble::cmd_rotate(&file, &out, pages.as_deref(), &by, rewrite, &password)
            .map_err(Failure::from),
        Command::Overlay {
            file,
            overlay,
            out,
            under,
            rewrite,
            password,
        } => assemble::cmd_overlay(&file, &overlay, &out, under, rewrite, &password)
            .map_err(Failure::from),
        Command::Rewrite {
            file,
            out,
            password,
        } => assemble::cmd_rewrite(&file, &out, &password).map_err(Failure::from),
        Command::Encrypt {
            file,
            out,
            user_password,
            owner_password,
            allow,
            password,
        } => assemble::cmd_encrypt(
            &file,
            &out,
            &user_password,
            &owner_password,
            allow,
            &password,
        ),
        Command::Decrypt {
            file,
            out,
            password,
        } => assemble::cmd_decrypt(&file, &out, &password).map_err(Failure::from),
        Command::Info { file, password } => cmd_info(&file, &password).map_err(Failure::from),
        Command::Text {
            file,
            page,
            password,
            reading_order,
        } => cmd_text(&file, page, &password, reading_order.to_order()).map_err(Failure::from),
        Command::Md {
            file,
            page,
            password,
            reading_order,
        } => cmd_md(&file, page, &password, reading_order.to_order()).map_err(Failure::from),
        Command::Render {
            file,
            page,
            out,
            scale,
            fonts,
            font_dir,
            password,
            png_compression,
            jpeg_quality,
        } => cmd_render(
            &file,
            page,
            out,
            scale,
            fonts,
            font_dir,
            &password,
            png_compression,
            jpeg_quality,
        )
        .map_err(Failure::from),
        Command::Images {
            file,
            page,
            out,
            password,
            png_compression,
            thumbnails,
        } => cmd_images(&file, page, out, &password, png_compression, thumbnails)
            .map_err(Failure::from),
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
            let mut sizes: Vec<Option<(f32, f32)>> = Vec::new();
            let mut pieces: Vec<String> = doc
                .piece_info()
                .into_iter()
                .map(|piece| piece.product)
                .collect();
            let mut thumbnails = 0usize;
            let mut slides = 0usize;
            let mut viewports = (0usize, 0usize);
            let mut separation_pages = 0usize;
            let mut colorants: Vec<String> = Vec::new();
            for index in 0..doc.page_count() {
                let page = doc.page(index).ok();
                sizes.push(page.as_ref().map(|page| page.size()));
                if let Some(page) = &page {
                    pieces.extend(
                        doc.page_piece_info(page)
                            .into_iter()
                            .map(|piece| piece.product),
                    );
                    thumbnails += usize::from(doc.thumbnail(page).is_some());
                    slides += usize::from(doc.presentation(page).is_some());
                    let page_viewports = doc.viewports(page).len();
                    viewports.0 += page_viewports;
                    viewports.1 += usize::from(page_viewports > 0);
                    if let Some(separation) = doc.separation_info(page) {
                        separation_pages += 1;
                        colorants.push(separation.device_colorant);
                    }
                }
            }
            pieces.sort();
            pieces.dedup();
            colorants.sort();
            colorants.dedup();
            let threads = doc.articles();
            let articles = (
                threads.len(),
                threads.iter().map(|thread| thread.beads.len()).sum(),
            );
            let linearization = doc.linearization();
            let handlers = doc.permission_handlers();
            let mut perms = Vec::new();
            if handlers.as_ref().is_some_and(|h| h.doc_mdp.is_some()) {
                perms.push("DocMDP");
            }
            if handlers.as_ref().is_some_and(|h| h.usage_rights.is_some()) {
                perms.push("UR3");
            }
            print!(
                "{}",
                info_text(&Info {
                    version: Some(doc.version()),
                    encrypted: false,
                    sizes: Some(&sizes),
                    meta: Some(&doc.metadata()),
                    extensions: &doc.extensions(),
                    output_intents: &doc.output_intents(),
                    fields: &doc.form_fields(),
                    pieces: &pieces,
                    thumbnails,
                    slides,
                    articles,
                    perms: &perms,
                    requirements: &doc.requirements(),
                    viewports,
                    separation_pages,
                    colorants: &colorants,
                    linearization: linearization
                        .as_ref()
                        .map(|record| (record, doc.bytes().len() as u64)),
                })
            );
            Ok(())
        }
        Err(Error::Encrypted) => {
            let data = std::fs::read(file).map_err(|e| e.to_string())?;
            let linearization = pdfboss_core::linearization_dictionary(&data);
            print!(
                "{}",
                info_text(&Info {
                    version: scan_version(&data),
                    encrypted: true,
                    linearization: linearization
                        .as_ref()
                        .map(|record| (record, data.len() as u64)),
                    ..Info::default()
                })
            );
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// What the `info` report shows. `sizes` is one entry per page (`None` when
/// a page failed to load); `None` for the whole slice means the page count
/// is unknown (encrypted document). `meta` is `None` when the document could
/// not be opened. `fields` are the interactive form's fields, counted by
/// type. `pieces` are the products that left page-piece data on the catalog
/// or a page, sorted and without repeats. `thumbnails` counts the pages
/// that carry a thumbnail image. `slides` counts the pages with a display
/// duration or a transition. `articles` counts the article threads and
/// their beads. `perms` names the permission handlers the catalog's
/// `/Perms` dictionary carries. `requirements` are the catalog's
/// `/Requirements` entries, printed by type. `viewports` counts the `/VP`
/// viewports and the pages that carry them. `separation_pages` counts the
/// pages with a `/SeparationInfo` dictionary and `colorants` names, sorted
/// and without repeats, the colorants they print. `linearization` is the
/// linearization parameter dictionary as written, paired with the file's
/// actual length, so a dictionary an appended update left behind prints as
/// not linearized.
#[derive(Default)]
struct Info<'a> {
    version: Option<(u8, u8)>,
    encrypted: bool,
    sizes: Option<&'a [Option<(f32, f32)>]>,
    meta: Option<&'a Metadata>,
    extensions: &'a [pdfboss_core::DeveloperExtension],
    output_intents: &'a [pdfboss_core::OutputIntent],
    fields: &'a [pdfboss_core::FormField],
    pieces: &'a [String],
    thumbnails: usize,
    slides: usize,
    articles: (usize, usize),
    perms: &'a [&'a str],
    requirements: &'a [pdfboss_core::Requirement],
    viewports: (usize, usize),
    separation_pages: usize,
    colorants: &'a [String],
    linearization: Option<(&'a pdfboss_core::Linearization, u64)>,
}

/// Renders the `info` report.
fn info_text(info: &Info) -> String {
    let mut out = String::new();
    match info.version {
        Some((major, minor)) => {
            let _ = writeln!(out, "version:   {major}.{minor}");
        }
        None => {
            let _ = writeln!(out, "version:   unknown");
        }
    }
    if !info.extensions.is_empty() {
        let _ = writeln!(out, "extensions:");
        for extension in info.extensions {
            let _ = writeln!(
                out,
                "  {:<9} {} level {}",
                extension.prefix, extension.base_version, extension.extension_level
            );
        }
    }
    // An intent names its condition by identifier, else in words, else in
    // its info text (ISO 32000-1 §14.11.5, Table 365).
    if !info.output_intents.is_empty() {
        let _ = writeln!(out, "output intents:");
        for intent in info.output_intents {
            let condition = intent
                .output_condition_identifier
                .as_deref()
                .or(intent.output_condition.as_deref())
                .or(intent.info.as_deref())
                .unwrap_or("");
            let line = format!("  {:<9} {condition}", intent.subtype);
            let _ = writeln!(out, "{}", line.trim_end());
        }
    }
    let _ = writeln!(out, "encrypted: {}", info.encrypted);
    // A file is linearized only while /L names its actual length (ISO
    // 32000-1 Annex F.3, Table F.1).
    if let Some((record, file_length)) = info.linearization {
        if record.is_current(file_length) {
            let _ = writeln!(
                out,
                "linearized: yes (first page object {})",
                record.first_page_object
            );
        } else {
            let _ = writeln!(
                out,
                "linearized: no (/L {} does not match the {file_length}-byte file)",
                record.file_length
            );
        }
    }
    // The permission handlers of the catalog's /Perms dictionary (ISO
    // 32000-1 §12.8.4): read, neither verified nor enforced.
    if !info.perms.is_empty() {
        let _ = writeln!(out, "perms:     {}", info.perms.join(", "));
    }
    // The features the catalog's /Requirements array asks a reader for (ISO
    // 32000-1 §12.10.1), by their /S type.
    if !info.requirements.is_empty() {
        let kinds: Vec<&str> = info
            .requirements
            .iter()
            .map(|requirement| requirement.kind.as_str())
            .collect();
        let _ = writeln!(out, "requirements: {}", kinds.join(", "));
    }
    match info.sizes {
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
    // Pages carrying a /Thumb image (ISO 32000-1 §12.3.4).
    if info.thumbnails > 0 {
        let pages = info.sizes.map_or(0, <[Option<(f32, f32)>]>::len);
        let _ = writeln!(out, "thumbs:    {} of {pages} pages", info.thumbnails);
    }
    // Pages shown as slides: a display duration or a transition (ISO
    // 32000-1 §12.4.4).
    if info.slides > 0 {
        let pages = info.sizes.map_or(0, <[Option<(f32, f32)>]>::len);
        let _ = writeln!(out, "slides:    {} of {pages} pages", info.slides);
    }
    // Viewports with their own measurement scale (ISO 32000-1 §12.9) and
    // the pages that carry them.
    let (viewports, viewport_pages) = info.viewports;
    if viewports > 0 {
        let pages = info.sizes.map_or(0, <[Option<(f32, f32)>]>::len);
        let _ = writeln!(
            out,
            "viewports: {viewports} on {viewport_pages} of {pages} pages"
        );
    }
    // Pre-separated pages (ISO 32000-1 §14.11.4) and the colorants they
    // print.
    if info.separation_pages > 0 {
        let pages = info.sizes.map_or(0, <[Option<(f32, f32)>]>::len);
        let _ = writeln!(
            out,
            "separations: {} of {pages} pages ({})",
            info.separation_pages,
            info.colorants.join(", ")
        );
    }
    // Article threads and the beads they chain (ISO 32000-1 §12.4.3).
    let (threads, beads) = info.articles;
    if threads > 0 {
        let plural = if beads == 1 { "" } else { "s" };
        let _ = writeln!(out, "articles:  {threads} ({beads} bead{plural})");
    }
    // Only terminal fields hold values; a field with child fields is a
    // container for inheritable entries (ISO 32000-1 §12.7.3).
    let terminal: Vec<&pdfboss_core::FormField> = info
        .fields
        .iter()
        .filter(|field| field.kids.is_empty())
        .collect();
    if !terminal.is_empty() {
        use pdfboss_core::FieldType;
        let groups = [
            (Some(FieldType::Button), "Btn"),
            (Some(FieldType::Text), "Tx"),
            (Some(FieldType::Choice), "Ch"),
            (Some(FieldType::Signature), "Sig"),
            (None, "untyped"),
        ];
        let breakdown: Vec<String> = groups
            .iter()
            .map(|(field_type, label)| {
                let count = terminal
                    .iter()
                    .filter(|field| field.field_type == *field_type)
                    .count();
                (count, label)
            })
            .filter(|(count, _)| *count > 0)
            .map(|(count, label)| format!("{label} {count}"))
            .collect();
        let _ = writeln!(
            out,
            "fields:    {} ({})",
            terminal.len(),
            breakdown.join(", ")
        );
    }
    // The products that left private data on the catalog or a page (ISO
    // 32000-1 §14.5).
    if !info.pieces.is_empty() {
        let _ = writeln!(out, "pieces:    {}", info.pieces.join(", "));
    }
    // A date that parses (ISO 32000-1 §7.9.4) prints as ISO 8601; one that
    // does not prints as written.
    let none = Metadata::default();
    let meta = info.meta.unwrap_or(&none);
    let created = meta
        .creation_date_parsed()
        .map(|d| d.to_iso8601())
        .or_else(|| meta.creation_date.clone());
    let modified = meta
        .mod_date_parsed()
        .map(|d| d.to_iso8601())
        .or_else(|| meta.mod_date.clone());
    let rows: [(&str, &Option<String>); 8] = [
        ("title", &meta.title),
        ("author", &meta.author),
        ("subject", &meta.subject),
        ("keywords", &meta.keywords),
        ("creator", &meta.creator),
        ("producer", &meta.producer),
        ("created", &created),
        ("modified", &modified),
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
fn cmd_text(
    file: &Path,
    page: Option<usize>,
    password: &str,
    order: pdfboss_output::ReadingOrder,
) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let text = match page {
        Some(n) => {
            let index = page_index(n, doc.page_count())?;
            let page = doc.page(index).map_err(|e| e.to_string())?;
            let (text, report) = pdfboss_output::extract_text_reporting(&doc, &page, order)
                .map_err(|e| e.to_string())?;
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
                pdfboss_output::extract_text_reporting_cached(doc, page, &fonts, order)
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
fn cmd_md(
    file: &Path,
    page: Option<usize>,
    password: &str,
    order: pdfboss_output::ReadingOrder,
) -> Result<(), String> {
    let doc = Document::open_with_password(file, password).map_err(|e| e.to_string())?;
    let text = match page {
        Some(n) => {
            let index = page_index(n, doc.page_count())?;
            let page = doc.page(index).map_err(|e| e.to_string())?;
            let (spans, rulings, report) =
                pdfboss_text::extract_spans_and_rulings_reporting(&doc, &page, order)
                    .map_err(|e| e.to_string())?;
            warn_skips(n, &report);
            pdfboss_output::Markdown.render(&[pdfboss_output::page_layout_with_rulings(
                &spans,
                &rulings,
                report.order,
            )])
        }
        None => {
            let (md, reports) = pdfboss_output::extract_markdown_reporting(&doc, order)
                .map_err(|e| e.to_string())?;
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

/// `pdfboss render`: rasterizes one page to the image file `out`'s
/// extension names.
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
    jpeg_quality: u8,
) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(format!("invalid scale {scale}: must be a positive number"));
    }
    let out = out.unwrap_or_else(|| default_out(page));
    let format = output_format(&out, png_compression, jpeg_quality)?;
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
    let image = pixmap.encode(format).map_err(|e| e.to_string())?;
    std::fs::write(&out, image).map_err(|e| e.to_string())?;
    // Rendering is lenient, so a page whose content pdfboss could not read
    // still writes the image and still exits 0. Say what was lost, on stderr and
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
    thumbnails: bool,
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
        // A page's thumbnail (ISO 32000-1 §12.3.4) is one image outside the
        // content stream, so the flag swaps what the page contributes.
        let (images, names): (Vec<pdfboss_render::Pixmap>, Vec<String>) = if thumbnails {
            match pdfboss_render::page_thumbnail(&doc, &p) {
                Some(pix) => (vec![pix], vec![format!("page-{}-thumb.png", index + 1)]),
                None => (Vec::new(), Vec::new()),
            }
        } else {
            let images =
                pdfboss_render::extract_page_images(&doc, &p).map_err(|e| e.to_string())?;
            let names = (1..=images.len())
                .map(|i| format!("page-{}-image-{i}.png", index + 1))
                .collect();
            (images, names)
        };
        for (pix, name) in images.iter().zip(names) {
            let path = dir.join(name);
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
    let noun = if thumbnails { "thumbnail" } else { "image" };
    match written {
        1 => println!("extracted 1 {noun}"),
        n => println!("extracted {n} {noun}s"),
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
        pdfboss_tui::run(doc, display_title(target), target.to_string())
            .await
            .map_err(|e| e.to_string())
    })
}

/// Builds the async document: the HTTP backend (with fallback-download
/// progress on stderr) for URLs, the file backend otherwise -- exactly the
/// split `json`/`hex`/`q` already make via `Input::open` (`pdfboss-aio`'s
/// `http` feature is unconditionally on for this crate, so there is no cfg
/// gate to make here).
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
        return crate::progress::open_url_with_progress(target, password)
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

/// The image format `out`'s extension names, PNG carrying the requested
/// compression level and JPEG the requested quality.
fn output_format(
    out: &Path,
    png_compression: PngCompressionArg,
    jpeg_quality: u8,
) -> Result<pdfboss_render::ImageFormat, String> {
    use pdfboss_render::ImageFormat;
    let extension = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ImageFormat::from_name(extension) {
        Some(ImageFormat::Png(_)) => Ok(ImageFormat::Png(png_compression.to_compression())),
        Some(ImageFormat::Jpeg { .. }) => Ok(ImageFormat::Jpeg {
            quality: jpeg_quality,
        }),
        Some(format) => Ok(format),
        None => Err(format!(
            "unsupported output format {extension:?}: use .png, .ppm, .bmp or .jpg"
        )),
    }
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
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            meta: Some(&meta),
            ..Info::default()
        });
        assert!(report.contains("version:   1.7"));
        assert!(report.contains("encrypted: false"));
        assert!(report.contains("pages:     1"));
        assert!(report.contains("page 1: 612 x 792 pt"));
        assert!(report.contains("title"));
        assert!(report.contains("Demo"));
    }

    /// The catalog's developer extensions print after the version, one
    /// per line with the base version and the level; none prints nothing.
    // Covers ISO 32000-1 §7.12.2.
    #[test]
    fn info_text_lists_developer_extensions() {
        let extensions = [pdfboss_core::DeveloperExtension {
            prefix: "ADBE".to_string(),
            base_version: "1.7".to_string(),
            extension_level: 3,
        }];
        let report = info_text(&Info {
            version: Some((1, 7)),
            extensions: &extensions,
            ..Info::default()
        });
        assert!(
            report.contains("version:   1.7\nextensions:\n  ADBE      1.7 level 3\n"),
            "{report}"
        );
        let report = info_text(&Info {
            version: Some((1, 7)),
            ..Info::default()
        });
        assert!(!report.contains("extensions"), "{report}");
    }

    /// The catalog's output intents print after the extensions, one per
    /// line with the subtype and the condition identifier, or the condition
    /// in words when the identifier is missing; none prints no block.
    // Covers ISO 32000-1 §14.11.5.
    #[test]
    fn info_text_lists_output_intents() {
        use pdfboss_core::OutputIntent;
        let intents = [
            OutputIntent {
                subtype: "GTS_PDFA1".to_string(),
                output_condition: None,
                output_condition_identifier: Some("sRGB IEC61966-2.1".to_string()),
                registry_name: None,
                info: None,
                destination_profile: None,
            },
            OutputIntent {
                subtype: "GTS_PDFX".to_string(),
                output_condition: Some("CGATS TR 001 (SWOP)".to_string()),
                output_condition_identifier: None,
                registry_name: None,
                info: None,
                destination_profile: None,
            },
        ];
        let report = info_text(&Info {
            version: Some((1, 7)),
            output_intents: &intents,
            ..Info::default()
        });
        assert!(
            report.contains(
                "version:   1.7\noutput intents:\n  GTS_PDFA1 sRGB IEC61966-2.1\n  GTS_PDFX  CGATS TR 001 (SWOP)\nencrypted: false\n"
            ),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("output intents"));
    }

    /// The products with page-piece data print as one line after the pages
    /// and fields; none prints no line.
    // Covers ISO 32000-1 §14.5.
    #[test]
    fn info_text_lists_page_piece_products() {
        let pieces = ["Illustrator".to_string(), "Photoshop".to_string()];
        let report = info_text(&Info {
            version: Some((1, 7)),
            pieces: &pieces,
            ..Info::default()
        });
        assert!(
            report.contains("pages:     unknown\npieces:    Illustrator, Photoshop\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("pieces"));
    }

    /// Pages with a thumbnail print as one count after the pages; none
    /// prints no line.
    // Covers ISO 32000-1 §12.3.4.
    #[test]
    fn info_text_counts_pages_with_thumbnails() {
        let sizes = [Some((612.0, 792.0)), Some((612.0, 792.0))];
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            thumbnails: 1,
            ..Info::default()
        });
        assert!(
            report.contains("  page 2: 612 x 792 pt\nthumbs:    1 of 2 pages\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("thumbs"));
    }

    /// Article threads print as one count with their bead total after the
    /// pages; none prints no line.
    // Covers ISO 32000-1 §12.4.3.
    #[test]
    fn info_text_counts_article_threads() {
        let report = info_text(&Info {
            version: Some((1, 7)),
            articles: (2, 7),
            ..Info::default()
        });
        assert!(
            report.contains("pages:     unknown\narticles:  2 (7 beads)\n"),
            "{report}"
        );
        let single = info_text(&Info {
            articles: (1, 1),
            ..Info::default()
        });
        assert!(single.contains("articles:  1 (1 bead)\n"), "{single}");
        assert!(!info_text(&Info::default()).contains("articles"));
    }

    /// Pages with a display duration or a transition print as one count
    /// after the pages; none prints no line.
    // Covers ISO 32000-1 §12.4.4.
    #[test]
    fn info_text_counts_slide_pages() {
        let sizes = [Some((612.0, 792.0)); 5];
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            slides: 4,
            ..Info::default()
        });
        assert!(
            report.contains("  page 5: 612 x 792 pt\nslides:    4 of 5 pages\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("slides"));
    }

    /// The permission handlers of the catalog's `/Perms` dictionary print
    /// as one line after the encryption line; none prints no line.
    // Covers ISO 32000-1 §12.8.4.
    #[test]
    fn info_text_lists_permission_handlers() {
        let report = info_text(&Info {
            version: Some((1, 7)),
            perms: &["DocMDP", "UR3"],
            ..Info::default()
        });
        assert!(
            report.contains("encrypted: false\nperms:     DocMDP, UR3\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("perms"));
    }

    /// The catalog's requirements print as one line after the permission
    /// handlers, each by its `/S` name; none prints no line.
    // Covers ISO 32000-1 §12.10.1.
    #[test]
    fn info_text_lists_requirements() {
        let requirements = [
            pdfboss_core::Requirement {
                kind: "EnableJavaScripts".into(),
                handlers: Vec::new(),
            },
            pdfboss_core::Requirement {
                kind: "Custom".into(),
                handlers: Vec::new(),
            },
        ];
        let report = info_text(&Info {
            version: Some((1, 7)),
            requirements: &requirements,
            ..Info::default()
        });
        assert!(
            report.contains("encrypted: false\nrequirements: EnableJavaScripts, Custom\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("requirements"));
    }

    /// Pages carrying `/VP` viewports print as one line with the viewport
    /// count and the pages that hold them; none prints no line.
    // Covers ISO 32000-1 §12.9.
    #[test]
    fn info_text_counts_viewports() {
        let sizes = [Some((612.0, 792.0)); 5];
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            viewports: (3, 2),
            ..Info::default()
        });
        assert!(
            report.contains("  page 5: 612 x 792 pt\nviewports: 3 on 2 of 5 pages\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("viewports"));
    }

    /// Pre-separated pages print as one line with the page count and the
    /// colorants they print; none prints no line.
    // Covers ISO 32000-1 §14.11.4.
    #[test]
    fn info_text_counts_separations() {
        let sizes = [Some((612.0, 792.0)); 4];
        let colorants = ["Black".to_string(), "Cyan".to_string()];
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            separation_pages: 4,
            colorants: &colorants,
            ..Info::default()
        });
        assert!(
            report.contains("  page 4: 612 x 792 pt\nseparations: 4 of 4 pages (Black, Cyan)\n"),
            "{report}"
        );
        assert!(!info_text(&Info::default()).contains("separations"));
    }

    /// A linearized file prints its first page object after the encryption
    /// line; a dictionary whose `/L` no longer names the file's length
    /// prints as not linearized, and a file without one prints no line.
    // Covers ISO 32000-1 Annex F.3.
    #[test]
    fn info_text_reports_linearization() {
        let record = pdfboss_core::Linearization {
            version: 1.0,
            file_length: 12345,
            hint_streams: vec![(500, 200)],
            first_page_object: 45,
            first_page_end: 3000,
            page_count: 3,
            main_xref_offset: 11000,
            first_page: 0,
        };
        let report = |linearization| {
            info_text(&Info {
                version: Some((1, 7)),
                linearization,
                ..Info::default()
            })
        };
        let current = report(Some((&record, 12345)));
        assert!(
            current.contains("encrypted: false\nlinearized: yes (first page object 45)\n"),
            "{current}"
        );
        let updated = report(Some((&record, 13000)));
        assert!(
            updated.contains("linearized: no (/L 12345 does not match the 13000-byte file)\n"),
            "{updated}"
        );
        assert!(!report(None).contains("linearized"));
    }

    /// The interactive form's terminal fields print after the pages as one
    /// count with a breakdown by field type; a non-terminal field is only a
    /// container and is not counted, and a document without fields prints
    /// no line.
    // Covers ISO 32000-1 §12.7.3.
    #[test]
    fn info_text_counts_form_fields_by_type() {
        use pdfboss_core::{FieldFlags, FieldType, FormField, ObjRef, Quadding};
        fn field(field_type: Option<FieldType>, kids: Vec<ObjRef>) -> FormField {
            FormField {
                object: ObjRef { num: 1, gen: 0 },
                parent: None,
                kids,
                widgets: Vec::new(),
                field_type,
                partial_name: None,
                name: String::new(),
                alternate_name: None,
                mapping_name: None,
                flags: FieldFlags::default(),
                value: None,
                default_value: None,
                default_appearance: None,
                quadding: Quadding::Left,
                default_style: None,
                rich_text: None,
                max_len: None,
                options: Vec::new(),
                top_index: 0,
                selected_indices: Vec::new(),
                additional_actions: None,
                lock: None,
                seed_value: None,
            }
        }
        let fields = [
            field(Some(FieldType::Text), vec![ObjRef { num: 2, gen: 0 }]),
            field(Some(FieldType::Text), Vec::new()),
            field(Some(FieldType::Text), Vec::new()),
            field(Some(FieldType::Button), Vec::new()),
            field(Some(FieldType::Choice), Vec::new()),
            field(Some(FieldType::Signature), Vec::new()),
            field(None, Vec::new()),
        ];
        let sizes = [Some((612.0, 792.0))];
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            fields: &fields,
            ..Info::default()
        });
        assert!(
            report.contains(
                "  page 1: 612 x 792 pt
fields:    6 (Btn 1, Tx 2, Ch 1, Sig 1, untyped 1)
"
            ),
            "{report}"
        );
        let report = info_text(&Info {
            version: Some((1, 7)),
            sizes: Some(&sizes),
            ..Info::default()
        });
        assert!(!report.contains("fields"), "{report}");
    }

    #[test]
    fn info_text_encrypted_document() {
        let report = info_text(&Info {
            version: Some((1, 4)),
            encrypted: true,
            ..Info::default()
        });
        assert!(report.contains("encrypted: true"));
        assert!(report.contains("pages:     unknown"));
        assert!(!report.contains("metadata:"));
    }

    #[test]
    fn info_text_unavailable_page() {
        let sizes = [None];
        let report = info_text(&Info {
            sizes: Some(&sizes),
            ..Info::default()
        });
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
    fn create_manifest_parses_input_and_out() {
        let cli = Cli::try_parse_from(["pdfboss", "create", "manifest", "q3.toml", "-o", "q3.pdf"])
            .unwrap();
        let Command::Create {
            command: create::CreateCommand::Manifest { input, out },
        } = cli.command
        else {
            panic!("expected create manifest");
        };
        assert_eq!(input, PathBuf::from("q3.toml"));
        assert_eq!(out, PathBuf::from("q3.pdf"));
    }

    #[test]
    fn create_manifest_requires_out() {
        let outcome = Cli::try_parse_from(["pdfboss", "create", "manifest", "q3.toml"]);
        assert!(outcome.is_err());
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
            false,
        )
        .expect("extract");
        for name in ["page-1-image-1.png", "page-1-image-2.png"] {
            let png = std::fs::read(dir.join(name)).expect(name);
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name} is a PNG");
        }
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    /// With `--thumbnails`, `images` writes each page's thumbnail instead
    /// of the drawn images, skipping pages without one.
    // Covers ISO 32000-1 §12.3.4.
    #[test]
    fn cmd_images_writes_thumbnails_with_the_flag() {
        use pdfboss_testkit::PdfBuilder;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Thumb 5 0 R >>",
        );
        b.stream(
            5,
            "/Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &[255, 0, 0, 0, 0, 255],
        );
        b.object(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let dir = std::env::temp_dir().join(format!("pdfboss-thumbs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pdf = dir.join("thumbs.pdf");
        std::fs::write(&pdf, b.build(1)).expect("fixture");
        cmd_images(
            &pdf,
            None,
            Some(dir.clone()),
            "",
            PngCompressionArg::Default,
            true,
        )
        .expect("extract");
        let png = std::fs::read(dir.join("page-1-thumb.png")).expect("page-1-thumb.png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(!dir.join("page-2-thumb.png").exists());
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
