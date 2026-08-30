# CLI reference

One binary, `pdfboss`, with a subcommand per job. This chapter is the flag inventory; worked examples live in the guide chapters linked from each entry.

Shared behavior:

- Every subcommand that reads a PDF takes `--password <PASSWORD>` for encrypted files: user or owner password; the empty user password opens transparently. See [Encrypted documents](../guide/encryption.md).
- The explorer subcommands (`tui`, `json`, `hex`, `q`) accept either a local path or an `http(s)://` URL as input; URLs are range-fetched, and a server that ignores `Range` costs one full download instead, reported with a progress bar on stderr when stderr is a terminal. The other subcommands take a local path.
- Exit codes: 0 on success, 1 for PDF and I/O problems, 2 for an invalid jq program (mirroring clap's own usage-error code). `render` and `images` are lenient: content that cannot be read is skipped with a warning on stderr, and the exit code stays 0.

## info

Show version, page count, page sizes and metadata.

```text
pdfboss info [OPTIONS] <FILE>
```

```bash
pdfboss info report.pdf
```

## text

Extract text; all pages separated by form feed unless `--page` (1-based) is given. See [Extracting text](../guide/text.md).

```text
pdfboss text [OPTIONS] <FILE>
```

```bash
pdfboss text report.pdf --page 1
```

## md

Extract markdown, with headings, lists and tables inferred from layout. `--page <PAGE>` restricts to one 1-based page: heading sizes are then judged per page, not across the document. See [Markdown output](../guide/markdown.md).

```text
pdfboss md [OPTIONS] <FILE>
```

```bash
pdfboss md report.pdf > report.md
```

## render

Render a page to PNG, PPM or BMP. See [Rendering pages](../guide/rendering.md).

```text
pdfboss render [OPTIONS] --page <PAGE> <FILE>
```

- `--page <PAGE>`: 1-based page number (required)
- `-o, --out <OUT>`: output file; its extension picks the format, `.png`, `.ppm` or `.bmp` (default: `page-N.png`)
- `--scale <SCALE>`: scale factor (default: 1)
- `--fonts <FONTS>`: which fonts to paint, one of `embedded-only` (only embedded TrueType outlines, fastest), `all-embedded` (every embedded program), `full` (also substitute bundled faces for non-embedded fonts); the default resolves to `full` when substitute faces are available (the compiled-in OFL set or `--font-dir`), otherwise `all-embedded`
- `--font-dir <FONT_DIR>`: directory of substitute faces for `--fonts full`; overrides the compiled-in OFL set
- `--png-compression <PNG_COMPRESSION>`: `none`, `fast`, `default` or `best` (encode time against file size, same pixels; PNG only)

```bash
pdfboss render --page 1 --scale 2 -o page-1.png report.pdf
pdfboss render --page 1 --scale 2 -o page-1.ppm report.pdf
```

## images

Extract every image a page draws, each as a native-size PNG. See [Extracting images](../guide/images.md).

```text
pdfboss images [OPTIONS] <FILE>
```

- `--page <PAGE>`: 1-based page number (default: all pages)
- `-o, --out <OUT>`: output directory, which must already exist (default: current directory)
- `--png-compression <PNG_COMPRESSION>`: as in `render`

```bash
pdfboss images report.pdf --page 1 -o out
```

## obj

Pretty-print a single object by number, with an optional generation number (default 0).

```text
pdfboss obj [OPTIONS] <FILE> <NUM> [GEN]
```

```bash
pdfboss obj report.pdf 1
```

## tui

Explore a PDF interactively in the terminal: element tree, object inspector, hex view, page preview and Markdown preview. Takes a path or `http(s)` URL and requires an interactive terminal. See [Exploring PDF internals](../guide/explorer.md).

```text
pdfboss tui [OPTIONS] <TARGET>
```

```bash
pdfboss tui report.pdf
```

## json

Dump the document as a JSON value tree, for piping to external tools.

```text
pdfboss json [OPTIONS] <INPUT>
```

- `--raw`: embed raw (still encoded) stream data as base64; combining it with `--decode` is a usage error
- `--decode`: embed decoded stream data as base64
- `--pages <PAGES>`: restrict logical elements to these 1-based pages (comma separated)
- `--no-logical`: skip the logical layer (pages/fonts/images/annotations)
- `--content-ops`: include per-page content-stream operators (high volume)
- `--layout`: include per-page layout blocks (headings, paragraphs, lists, tables)

```bash
pdfboss json report.pdf --no-logical > tree.json
```

## hex

Hexdump the file or a selected element, hexyl-style.

```text
pdfboss hex [OPTIONS] <INPUT> [SELECTOR]
```

The selector is one of `obj:N[,G]`, `header`, `xref:N`, `trailer` or `range:START-END` (offsets decimal or `0x`-hex; xref sections indexed in chain order, newest first); without one, the whole file is dumped.

- `--annotate`: print labeled element boundaries as the dump crosses them
- `--width <WIDTH>`: bytes per row (default: 16)

The dump is colorized with ANSI escapes when stdout is a tty; setting the
`NO_COLOR` environment variable (any value) disables color.

```bash
pdfboss hex report.pdf header
```

## q

Run a jq program over the document's JSON value tree (the same tree `json` prints).

```text
pdfboss q [OPTIONS] <INPUT> <PROGRAM>
```

- `--raw`, `--decode`, `--pages <PAGES>`, `--no-logical`, `--content-ops`: as in `json`, including the `--raw`/`--decode` usage error when combined
- `--hex`: hexdump results carrying a `_span` instead of printing JSON; colorized on a tty like `hex`, with `NO_COLOR` honored
- `-r`: print string results raw, without quotes (like `jq -r`)

```bash
pdfboss q report.pdf -r '.pages[].fonts[].base_font'
```

## create

Create a new PDF: blank pages, word-wrapped text, image pages, a themed Markdown document, or a TOML manifest of composed pages.

```text
pdfboss create <COMMAND>
```

Five subcommands, each writing to `-o, --out <OUT>`. The first four share `--size a3|a4|a5|letter|legal` and `--landscape` (swap page width and height); `manifest` takes neither, since page size and orientation live per page inside the TOML. See [Creating PDFs](../guide/creating.md) and [Markdown to PDF](../guide/md-to-pdf.md).

### create blank

Empty pages. `--pages <PAGES>` sets the page count (default: 1); `--size` defaults to `a4`.

```text
pdfboss create blank [OPTIONS] --out <OUT>
```

```bash
pdfboss create blank -o blank.pdf --pages 3 --size letter
```

### create text

A UTF-8 text file, word-wrapped into pages.

```text
pdfboss create text [OPTIONS] --out <OUT> <INPUT>
```

- `--font <FONT>`: one of the fourteen standard fonts, `helvetica` (default), `helvetica-bold`, `helvetica-oblique`, `helvetica-bold-oblique`, `times-roman`, `times-bold`, `times-italic`, `times-bold-italic`, `courier`, `courier-bold`, `courier-oblique`, `courier-bold-oblique`, `symbol`, `zapf-dingbats`
- `--font-size <FONT_SIZE>`: font size in points (default: 11)
- `--margin <MARGIN>`: page margin in points, all four sides (default: 72)

```bash
pdfboss create text notes.txt -o notes.pdf --font times-roman --font-size 12
```

### create images

One page per input image (PNG or JPEG, detected by content). Without `--size`, each page matches its image at 72 dpi. `--landscape` requires `--size`: passing it alone is a usage error, not a no-op.

```text
pdfboss create images [OPTIONS] --out <OUT> <INPUTS>...
```

```bash
pdfboss create images scan-1.png scan-2.png -o scans.pdf
```

### create md

A markdown file composed into a themed document. See [Markdown to PDF](../guide/md-to-pdf.md).

```text
pdfboss create md [OPTIONS] --out <OUT> <INPUT>
```

- `--theme <THEME>`: CSS theme file (default: the built-in theme)
- `--size <SIZE>`: as above; `--landscape` swaps width and height

Relative image paths in the markdown resolve against the input file's directory.

```bash
pdfboss create md notes.md -o notes.pdf --theme theme.css --size letter
```

### create manifest

A TOML manifest describing metadata and pages: text, paragraphs, images and links mapped onto the compose vocabulary of `pdfboss-write`. See [Creating PDFs](../guide/creating.md#composing-pages) for that vocabulary.

```text
pdfboss create manifest --out <OUT> <INPUT>
```

The manifest's tables:

- `[meta]`: optional document information, mapped onto `/Info`: `title`, `author`, `subject`, `keywords`, `creator`, `producer`, each a string.
- `[[page]]`: one table per page, in reading order. `size` names a page size case-insensitively (`a3`, `a4`, `a5`, `letter`, `legal`; absent defaults to `a4`) and `landscape` (boolean) swaps width and height, both per page.
- `[[page.text]]`: one line of text: `value`, `at = [x, y]` (the baseline origin), optional `font` and `size`.
- `[[page.paragraph]]`: wrapped text: `value`, `rect = [x0, y0, x1, y1]`, optional `font`, `size`, `leading` and `align` (`left`, `center`, `right`, `justify`).
- `[[page.image]]`: a placed raster: `path` (resolved relative to the manifest's directory, decoded by content as PNG or JPEG), `at = [x, y]`, optional `width` and `height`.
- `[[page.link]]`: a clickable rectangle: `rect` plus exactly one of `url` or `page` (a 0-based page index in the same document).

Font names are PostScript base names (`Helvetica`, `Helvetica-Bold`, `Times-Roman`, `Courier-Oblique`, …), unlike the kebab-case values of `create text --font`; an unknown name errors listing the valid set, and an absent one defaults to `Helvetica`. Unknown TOML keys are rejected, and every error message is prefixed with the manifest's path. Within a page, content lowers in schema order (text, then paragraphs, then images, then links) regardless of how the tables interleave in the file; TOML's separate arrays of tables carry no cross-type order.

```toml
[meta]
title  = "Q3 Report"
author = "pdfboss"

[[page]]
size = "a4"

  [[page.text]]
  value = "Q3 Report"
  at    = [72, 770]
  font  = "Helvetica-Bold"
  size  = 28

  [[page.paragraph]]
  value   = "Body copy for the quarter."
  rect    = [72, 380, 523, 720]
  size    = 11
  leading = 15
  align   = "left"

  [[page.image]]
  path  = "chart.png"
  at    = [72, 96]
  width = 200

  [[page.link]]
  rect = [72, 88, 523, 380]
  url  = "https://example.com/q3"
```

```bash
pdfboss create manifest q3.toml -o q3.pdf
```
