//! `pdfboss create`: making new PDFs — blank pages, word-wrapped text
//! files, one-page-per-image albums, and themed Markdown documents — on
//! top of `pdfboss-write` and `pdfboss-markdown`.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use pdfboss_markdown::Theme;
use pdfboss_write::{Canvas, ImageData, Page, PageSize, Pdf, Standard14};

/// The nested subcommands of `pdfboss create`.
#[derive(Subcommand)]
pub enum CreateCommand {
    /// Empty pages.
    Blank {
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Number of pages.
        #[arg(long, default_value_t = 1)]
        pages: usize,
        /// Page size.
        #[arg(long, value_enum, default_value_t = SizeArg::A4)]
        size: SizeArg,
        /// Swap page width and height.
        #[arg(long)]
        landscape: bool,
    },
    /// A UTF-8 text file, word-wrapped into pages.
    Text {
        /// Path to the UTF-8 text file.
        input: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Font face (one of the fourteen standard fonts).
        #[arg(long, value_enum, default_value_t = FontArg::Helvetica)]
        font: FontArg,
        /// Font size in points.
        #[arg(long, default_value_t = 11.0)]
        font_size: f32,
        /// Page size.
        #[arg(long, value_enum, default_value_t = SizeArg::A4)]
        size: SizeArg,
        /// Swap page width and height.
        #[arg(long)]
        landscape: bool,
        /// Page margin in points, all four sides.
        #[arg(long, default_value_t = 72.0)]
        margin: f32,
    },
    /// One page per input image (PNG or JPEG, detected by content).
    Images {
        /// Paths of the image files.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// Page size (default: each page matches its image at 72 dpi).
        #[arg(long, value_enum)]
        size: Option<SizeArg>,
        /// Swap page width and height (requires --size).
        #[arg(long, requires = "size")]
        landscape: bool,
    },
    /// A markdown file composed into a themed document.
    Md {
        /// Path to the markdown file.
        input: PathBuf,
        /// Output PDF file.
        #[arg(short, long)]
        out: PathBuf,
        /// CSS theme file (default: the built-in theme).
        #[arg(long)]
        theme: Option<PathBuf>,
        /// Page size.
        #[arg(long, value_enum, default_value_t = SizeArg::A4)]
        size: SizeArg,
        /// Swap page width and height.
        #[arg(long)]
        landscape: bool,
    },
}

/// `--font` choices for `create text`, mirroring
/// `pdfboss_write::Standard14`.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum FontArg {
    /// Helvetica (default).
    #[default]
    Helvetica,
    /// Helvetica-Bold.
    HelveticaBold,
    /// Helvetica-Oblique.
    HelveticaOblique,
    /// Helvetica-BoldOblique.
    HelveticaBoldOblique,
    /// Times-Roman.
    TimesRoman,
    /// Times-Bold.
    TimesBold,
    /// Times-Italic.
    TimesItalic,
    /// Times-BoldItalic.
    TimesBoldItalic,
    /// Courier.
    Courier,
    /// Courier-Bold.
    CourierBold,
    /// Courier-Oblique.
    CourierOblique,
    /// Courier-BoldOblique.
    CourierBoldOblique,
    /// Symbol.
    Symbol,
    /// ZapfDingbats.
    ZapfDingbats,
}

impl FontArg {
    /// The library font this flag names.
    fn to_standard14(self) -> Standard14 {
        match self {
            FontArg::Helvetica => Standard14::Helvetica,
            FontArg::HelveticaBold => Standard14::HelveticaBold,
            FontArg::HelveticaOblique => Standard14::HelveticaOblique,
            FontArg::HelveticaBoldOblique => Standard14::HelveticaBoldOblique,
            FontArg::TimesRoman => Standard14::TimesRoman,
            FontArg::TimesBold => Standard14::TimesBold,
            FontArg::TimesItalic => Standard14::TimesItalic,
            FontArg::TimesBoldItalic => Standard14::TimesBoldItalic,
            FontArg::Courier => Standard14::Courier,
            FontArg::CourierBold => Standard14::CourierBold,
            FontArg::CourierOblique => Standard14::CourierOblique,
            FontArg::CourierBoldOblique => Standard14::CourierBoldOblique,
            FontArg::Symbol => Standard14::Symbol,
            FontArg::ZapfDingbats => Standard14::ZapfDingbats,
        }
    }
}

/// `--size` choices, mirroring `pdfboss_write::PageSize`'s named sizes.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum SizeArg {
    /// 297 × 420 mm.
    A3,
    /// 210 × 297 mm (default).
    #[default]
    A4,
    /// 148 × 210 mm.
    A5,
    /// 8.5 × 11 in.
    Letter,
    /// 8.5 × 14 in.
    Legal,
}

impl SizeArg {
    /// The library page size this flag names.
    fn to_page_size(self) -> PageSize {
        match self {
            SizeArg::A3 => PageSize::A3,
            SizeArg::A4 => PageSize::A4,
            SizeArg::A5 => PageSize::A5,
            SizeArg::Letter => PageSize::Letter,
            SizeArg::Legal => PageSize::Legal,
        }
    }
}

/// Runs one `create` subcommand.
pub fn cmd_create(command: CreateCommand) -> Result<(), String> {
    let (pages, out) = match command {
        CreateCommand::Blank {
            out,
            pages,
            size,
            landscape,
        } => (blank_pages(pages, resolved_size(size, landscape))?, out),
        CreateCommand::Text {
            input,
            out,
            font,
            font_size,
            size,
            landscape,
            margin,
        } => {
            let text =
                std::fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?;
            let pages = text_pages(
                &text,
                font.to_standard14(),
                font_size,
                resolved_size(size, landscape),
                margin,
            )?;
            (pages, out)
        }
        CreateCommand::Images {
            inputs,
            out,
            size,
            landscape,
        } => {
            let page_size = size.map(|s| resolved_size(s, landscape));
            let mut pages = Vec::with_capacity(inputs.len());
            for input in &inputs {
                let bytes =
                    std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
                let image =
                    decode_image(&bytes).map_err(|e| format!("{}: {e}", input.display()))?;
                pages.push(image_page(image, page_size));
            }
            (pages, out)
        }
        CreateCommand::Md {
            input,
            out,
            theme,
            size,
            landscape,
        } => {
            let markdown =
                std::fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?;
            let theme = match &theme {
                Some(path) => {
                    let css = std::fs::read_to_string(path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    Theme::parse(&css).map_err(|e| format!("{}: {e}", path.display()))?
                }
                None => Theme::default_theme(),
            };
            let base_dir = input.parent().unwrap_or(Path::new(".")).to_path_buf();
            let options = pdfboss_markdown::Options {
                theme,
                page_size: resolved_size(size, landscape),
                base_dir,
            };
            let (pdf, report) =
                pdfboss_markdown::to_pdf(&markdown, &options).map_err(|e| e.to_string())?;
            if !report.is_empty() {
                eprintln!("{}", report.summary());
            }
            let count = pdf.pages.len();
            pdf.save(&out)
                .map_err(|e| format!("{}: {e}", out.display()))?;
            let plural = if count == 1 { "" } else { "s" };
            println!("wrote {} ({count} page{plural})", out.display());
            return Ok(());
        }
    };
    save(pages, &out)
}

/// Assembles `pages` into a document, writes it to `out` and prints a
/// summary line.
fn save(pages: Vec<Page>, out: &Path) -> Result<(), String> {
    let count = pages.len();
    let pdf = Pdf {
        pages,
        ..Pdf::default()
    };
    pdf.save(out)
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let plural = if count == 1 { "" } else { "s" };
    println!("wrote {} ({count} page{plural})", out.display());
    Ok(())
}

/// The library page size for `--size`, swapped by `--landscape`.
fn resolved_size(size: SizeArg, landscape: bool) -> PageSize {
    let size = size.to_page_size();
    if !landscape {
        return size;
    }
    size.landscape()
}

/// `count` empty pages of `size`; zero pages is an error, not an
/// unloadable document.
fn blank_pages(count: usize, size: PageSize) -> Result<Vec<Page>, String> {
    if count == 0 {
        return Err("--pages must be at least 1".to_string());
    }
    Ok((0..count).map(|_| Page::new(size)).collect())
}

/// Lays `text` out into pages: word-wrapped between the margins, line
/// advance 1.2 × font size, a new page whenever the next baseline would
/// drop below the bottom margin.
fn text_pages(
    text: &str,
    face: Standard14,
    font_size: f32,
    page_size: PageSize,
    margin: f32,
) -> Result<Vec<Page>, String> {
    if !font_size.is_finite() || font_size <= 0.0 {
        return Err(format!("font size {font_size} must be a positive number"));
    }
    if !margin.is_finite() || margin < 0.0 {
        return Err(format!("margin {margin} must be zero or more"));
    }
    let (width, height) = page_size.dimensions();
    let max_width = width - 2.0 * margin;
    let top = height - margin - font_size;
    if max_width <= 0.0 || top < margin {
        return Err(format!(
            "margin {margin} and font size {font_size} leave no room for text \
             on a {width} x {height} pt page"
        ));
    }
    let rows = wrap_text(text, face, font_size, max_width)?;
    let advance = 1.2 * font_size;
    let mut pages = Vec::new();
    let mut canvas = Canvas::new();
    let mut y = top;
    for row in rows {
        if y < margin {
            pages.push(page_with(page_size, std::mem::take(&mut canvas)));
            y = top;
        }
        if !row.is_empty() {
            canvas
                .text(&row, margin, y, face, font_size)
                .map_err(|e| e.to_string())?;
        }
        y -= advance;
    }
    pages.push(page_with(page_size, canvas));
    Ok(pages)
}

/// A page of `size` holding an already-painted canvas.
fn page_with(size: PageSize, canvas: Canvas) -> Page {
    Page {
        size,
        rotation: 0,
        canvas,
        ..Page::default()
    }
}

/// Expands tabs to four spaces, splits `text` into lines (CRLF tolerated)
/// and word-wraps each to `max_width`, returning paint-ready rows. Blank
/// source lines yield one empty row so vertical spacing survives.
fn wrap_text(
    text: &str,
    face: Standard14,
    size: f32,
    max_width: f32,
) -> Result<Vec<String>, String> {
    let mut rows = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.replace('\t', "    ");
        wrap_line(&line, face, size, max_width, index + 1, &mut rows)?;
    }
    Ok(rows)
}

/// Greedy word wrap of one source line into `rows`: words fill each row up
/// to `max_width`, the space run before a word survives inside a row and is
/// dropped at a break, and a word too wide for a whole row is broken hard,
/// character by character. A line with no words yields one empty row.
fn wrap_line(
    line: &str,
    face: Standard14,
    size: f32,
    max_width: f32,
    line_no: usize,
    rows: &mut Vec<String>,
) -> Result<(), String> {
    let start = rows.len();
    let mut current = String::new();
    let mut current_width = 0.0f32;
    for (spaces, word) in tokens(line) {
        if word.is_empty() {
            continue;
        }
        let at_line_start = rows.len() == start && current.is_empty();
        let glue = if at_line_start || !current.is_empty() {
            spaces
        } else {
            ""
        };
        let addition = format!("{glue}{word}");
        let addition_width = measured(face, line_no, &addition, size)?;
        if current_width + addition_width <= max_width {
            current.push_str(&addition);
            current_width += addition_width;
            continue;
        }
        if !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            current_width = 0.0;
        }
        let word_width = measured(face, line_no, word, size)?;
        if word_width <= max_width {
            current.push_str(word);
            current_width = word_width;
            continue;
        }
        for ch in word.chars() {
            let ch_width = measured(face, line_no, &String::from(ch), size)?;
            if !current.is_empty() && current_width + ch_width > max_width {
                rows.push(std::mem::take(&mut current));
                current_width = 0.0;
            }
            current.push(ch);
            current_width += ch_width;
        }
    }
    if !current.is_empty() || rows.len() == start {
        rows.push(current);
    }
    Ok(())
}

/// Splits a line into `(space run, word)` pairs whose concatenation is the
/// line; a line ending in spaces yields a final pair with an empty word.
fn tokens(line: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let space_len = rest.len() - rest.trim_start_matches(' ').len();
        let (spaces, tail) = rest.split_at(space_len);
        let word_len = tail.find(' ').unwrap_or(tail.len());
        let (word, tail) = tail.split_at(word_len);
        out.push((spaces, word));
        rest = tail;
    }
    out
}

/// Measures `piece` at `size` in `face`, mapping failures to a message
/// carrying the 1-based source line number and, for an unencodable
/// character, the character itself.
fn measured(face: Standard14, line_no: usize, piece: &str, size: f32) -> Result<f32, String> {
    face.text_width(piece, size).map_err(|e| match e {
        pdfboss_write::Error::Unencodable { ch, font } => {
            format!("line {line_no}: character {ch:?} is not encodable in {font}")
        }
        other => format!("line {line_no}: {other}"),
    })
}

/// Sniffs PNG or JPEG by content — never by file extension — and imports
/// accordingly.
fn decode_image(bytes: &[u8]) -> Result<ImageData, String> {
    ImageData::decode(bytes).map_err(|e| e.to_string())
}

/// One page holding `image`: sized to the pixels at 72 dpi when `size` is
/// `None`, else the given size with the image fit inside it, centered.
fn image_page(image: ImageData, size: Option<PageSize>) -> Page {
    let (iw, ih) = (image.width() as f32, image.height() as f32);
    let size = size.unwrap_or(PageSize::Custom {
        width: iw,
        height: ih,
    });
    let (pw, ph) = size.dimensions();
    let (x, y, w, h) = fit_box(iw, ih, pw, ph);
    let mut page = Page::new(size);
    let handle = page.canvas.add_image(image);
    page.canvas.draw_image(handle, x, y, w, h);
    page
}

/// The largest box of `iw : ih` aspect inside `pw × ph`, centered:
/// `(x, y, width, height)`.
fn fit_box(iw: f32, ih: f32, pw: f32, ph: f32) -> (f32, f32, f32, f32) {
    let scale = (pw / iw).min(ph / ih);
    let (w, h) = (iw * scale, ih * scale);
    ((pw - w) / 2.0, (ph - h) / 2.0, w, h)
}

#[cfg(test)]
mod tests {
    use clap::{Parser, ValueEnum};
    use pdfboss_core::content::Op;
    use pdfboss_core::Matrix;

    use super::*;
    use crate::{Cli, Command};

    fn parse_create(args: &[&str]) -> CreateCommand {
        let mut full = vec!["pdfboss", "create"];
        full.extend_from_slice(args);
        let cli = Cli::parse_from(full);
        let Command::Create { command } = cli.command else {
            panic!("expected create command");
        };
        command
    }

    fn tiny_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 3, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[10u8; 18]).unwrap();
        writer.finish().unwrap();
        bytes
    }

    /// SOI, then a baseline SOF0 declaring 8-bit 3 × 2 grayscale — the
    /// minimum a passthrough import needs to sniff.
    fn tiny_jpeg() -> Vec<u8> {
        let (soi, sof0) = ([0xFF, 0xD8], [0xFF, 0xC0]);
        let (len, precision, height, width, components) = (11u16, 8u8, 2u16, 3u16, 1u8);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&soi);
        bytes.extend_from_slice(&sof0);
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.push(precision);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.push(components);
        bytes
    }

    #[test]
    fn blank_parses_with_defaults() {
        let CreateCommand::Blank {
            out,
            pages,
            size,
            landscape,
        } = parse_create(&["blank", "-o", "out.pdf"])
        else {
            panic!("expected blank command");
        };
        assert_eq!(out, PathBuf::from("out.pdf"));
        assert_eq!(pages, 1);
        assert!(matches!(size, SizeArg::A4));
        assert!(!landscape);
    }

    #[test]
    fn blank_requires_out() {
        let outcome = Cli::try_parse_from(["pdfboss", "create", "blank"]);
        assert!(outcome.is_err());
    }

    #[test]
    fn text_parses_every_flag() {
        let CreateCommand::Text {
            input,
            out,
            font,
            font_size,
            size,
            landscape,
            margin,
        } = parse_create(&[
            "text",
            "in.txt",
            "-o",
            "o.pdf",
            "--font",
            "courier-bold",
            "--font-size",
            "9.5",
            "--size",
            "a5",
            "--landscape",
            "--margin",
            "36",
        ])
        else {
            panic!("expected text command");
        };
        assert_eq!(input, PathBuf::from("in.txt"));
        assert_eq!(out, PathBuf::from("o.pdf"));
        assert_eq!(font.to_standard14(), Standard14::CourierBold);
        assert_eq!(font_size, 9.5);
        assert!(matches!(size, SizeArg::A5));
        assert!(landscape);
        assert_eq!(margin, 36.0);
    }

    #[test]
    fn images_parses_multiple_inputs() {
        let CreateCommand::Images {
            inputs,
            out,
            size,
            landscape,
        } = parse_create(&[
            "images",
            "a.png",
            "b.jpg",
            "-o",
            "o.pdf",
            "--size",
            "letter",
            "--landscape",
        ])
        else {
            panic!("expected images command");
        };
        assert_eq!(inputs, [PathBuf::from("a.png"), PathBuf::from("b.jpg")]);
        assert_eq!(out, PathBuf::from("o.pdf"));
        assert!(matches!(size, Some(SizeArg::Letter)));
        assert!(landscape);
    }

    #[test]
    fn images_require_at_least_one_input() {
        let outcome = Cli::try_parse_from(["pdfboss", "create", "images", "-o", "o.pdf"]);
        assert!(outcome.is_err());
    }

    #[test]
    fn images_landscape_needs_size() {
        let outcome = Cli::try_parse_from([
            "pdfboss",
            "create",
            "images",
            "a.png",
            "-o",
            "o.pdf",
            "--landscape",
        ]);
        assert!(outcome.is_err());
    }

    #[test]
    fn font_arg_mirrors_all_fourteen_in_kebab_case() {
        let expected_names = [
            "helvetica",
            "helvetica-bold",
            "helvetica-oblique",
            "helvetica-bold-oblique",
            "times-roman",
            "times-bold",
            "times-italic",
            "times-bold-italic",
            "courier",
            "courier-bold",
            "courier-oblique",
            "courier-bold-oblique",
            "symbol",
            "zapf-dingbats",
        ];
        let variants = FontArg::value_variants();
        assert_eq!(variants.len(), 14);
        for ((variant, name), face) in variants.iter().zip(expected_names).zip(Standard14::ALL) {
            assert_eq!(variant.to_possible_value().unwrap().get_name(), name);
            assert_eq!(variant.to_standard14(), face);
            let parsed = FontArg::from_str(name, false).unwrap();
            assert_eq!(parsed.to_standard14(), face);
        }
    }

    #[test]
    fn size_arg_maps_to_page_sizes() {
        for (name, expected) in [
            ("a3", PageSize::A3),
            ("a4", PageSize::A4),
            ("a5", PageSize::A5),
            ("letter", PageSize::Letter),
            ("legal", PageSize::Legal),
        ] {
            let parsed = SizeArg::from_str(name, false).unwrap();
            assert_eq!(parsed.to_page_size(), expected, "{name}");
        }
    }

    #[test]
    fn resolved_size_swaps_under_landscape() {
        assert_eq!(
            resolved_size(SizeArg::Letter, false).dimensions(),
            (612.0, 792.0)
        );
        assert_eq!(
            resolved_size(SizeArg::Letter, true).dimensions(),
            (792.0, 612.0)
        );
    }

    #[test]
    fn blank_pages_builds_the_count() {
        let pages = blank_pages(3, PageSize::A5).unwrap();
        assert_eq!(pages.len(), 3);
        assert!(pages.iter().all(|p| p.size == PageSize::A5));
        assert!(pages.iter().all(|p| p.canvas.ops().is_empty()));
    }

    #[test]
    fn blank_pages_rejects_zero() {
        let err = blank_pages(0, PageSize::A4).unwrap_err();
        assert!(err.contains("--pages"), "unexpected message: {err}");
    }

    #[test]
    fn wrap_wraps_greedily_at_word_boundaries() {
        let face = Standard14::Helvetica;
        let two_words = face.text_width("aa bb", 10.0).unwrap();
        let rows = wrap_text("aa bb cc", face, 10.0, two_words + 0.01).unwrap();
        assert_eq!(rows, ["aa bb", "cc"]);
        let whole = face.text_width("aa bb cc", 10.0).unwrap();
        let rows = wrap_text("aa bb cc", face, 10.0, whole + 0.01).unwrap();
        assert_eq!(rows, ["aa bb cc"]);
    }

    #[test]
    fn wrap_hard_breaks_unbreakable_words() {
        let face = Standard14::Helvetica;
        let three = face.text_width("aaa", 10.0).unwrap();
        let rows = wrap_text("aaaaaaaa", face, 10.0, three + 0.01).unwrap();
        assert_eq!(rows, ["aaa", "aaa", "aa"]);
        let rows = wrap_text("hi aaaaaaaa", face, 10.0, three + 0.01).unwrap();
        assert_eq!(rows, ["hi", "aaa", "aaa", "aa"]);
    }

    #[test]
    fn wrap_expands_tabs_and_handles_crlf() {
        let face = Standard14::Helvetica;
        let rows = wrap_text("a\tb", face, 10.0, 500.0).unwrap();
        assert_eq!(rows, ["a    b"]);
        let rows = wrap_text("one\r\ntwo", face, 10.0, 500.0).unwrap();
        assert_eq!(rows, ["one", "two"]);
    }

    #[test]
    fn wrap_keeps_blank_lines_and_indentation() {
        let face = Standard14::Helvetica;
        let rows = wrap_text("a\n\nb", face, 10.0, 500.0).unwrap();
        assert_eq!(rows, ["a", "", "b"]);
        let rows = wrap_text("  in", face, 10.0, 500.0).unwrap();
        assert_eq!(rows, ["  in"]);
    }

    #[test]
    fn wrap_names_unencodable_char_and_line() {
        let face = Standard14::Helvetica;
        let err = wrap_text("ok\nbad \u{2318} here", face, 10.0, 500.0).unwrap_err();
        assert!(err.contains("line 2"), "no line number in: {err}");
        assert!(err.contains('\u{2318}'), "no character in: {err}");
    }

    #[test]
    fn text_pages_break_at_the_bottom_margin() {
        let size = PageSize::Custom {
            width: 200.0,
            height: 56.0,
        };
        let pages = text_pages("l1\nl2\nl3\nl4", Standard14::Helvetica, 10.0, size, 10.0).unwrap();
        assert_eq!(pages.len(), 2);
        let shows = |page: &Page| {
            page.canvas
                .ops()
                .iter()
                .filter(|op| matches!(op, Op::ShowText(..)))
                .count()
        };
        assert_eq!(shows(&pages[0]), 3);
        assert_eq!(shows(&pages[1]), 1);
        assert!(pages[0].canvas.ops().contains(&Op::TextMove(10.0, 36.0)));
    }

    #[test]
    fn text_pages_empty_input_is_one_empty_page() {
        let pages = text_pages("", Standard14::Helvetica, 10.0, PageSize::A4, 72.0).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages[0].canvas.ops().is_empty());
    }

    #[test]
    fn text_pages_reject_hostile_geometry() {
        let err = text_pages("x", Standard14::Helvetica, 10.0, PageSize::A5, 300.0).unwrap_err();
        assert!(err.contains("margin"), "unexpected message: {err}");
        let err = text_pages("x", Standard14::Helvetica, 0.0, PageSize::A4, 72.0).unwrap_err();
        assert!(err.contains("font size"), "unexpected message: {err}");
    }

    #[test]
    fn fit_box_scales_and_centers() {
        assert_eq!(
            fit_box(100.0, 50.0, 200.0, 200.0),
            (0.0, 50.0, 200.0, 100.0)
        );
        assert_eq!(fit_box(50.0, 100.0, 200.0, 100.0), (75.0, 0.0, 50.0, 100.0));
    }

    #[test]
    fn image_page_without_size_matches_the_pixels() {
        let page = image_page(ImageData::gray8(3, 2, vec![0; 6]).unwrap(), None);
        assert_eq!(
            page.size,
            PageSize::Custom {
                width: 3.0,
                height: 2.0
            }
        );
        let ops = page.canvas.ops();
        assert!(ops.contains(&Op::Concat(Matrix {
            a: 3.0,
            b: 0.0,
            c: 0.0,
            d: 2.0,
            e: 0.0,
            f: 0.0,
        })));
        assert!(ops
            .iter()
            .any(|op| matches!(op, Op::XObject(name) if name.0 == "Im1")));
    }

    #[test]
    fn image_page_with_size_fits_and_centers() {
        let image = ImageData::gray8(100, 50, vec![0; 5000]).unwrap();
        let page = image_page(image, Some(PageSize::A4));
        assert_eq!(page.size, PageSize::A4);
        let (x, y, w, h) = fit_box(100.0, 50.0, 595.28, 841.89);
        assert!(page.canvas.ops().contains(&Op::Concat(Matrix {
            a: w,
            b: 0.0,
            c: 0.0,
            d: h,
            e: x,
            f: y,
        })));
    }

    #[test]
    fn decode_image_sniffs_magic_bytes() {
        let image = decode_image(&tiny_png()).unwrap();
        assert_eq!((image.width(), image.height()), (3, 2));
        let image = decode_image(&tiny_jpeg()).unwrap();
        assert_eq!((image.width(), image.height()), (3, 2));
        let err = decode_image(b"GIF89a not really").unwrap_err();
        assert!(
            err.contains("not a png or jpeg"),
            "unexpected message: {err}"
        );
    }
}
