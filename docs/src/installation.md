# Installation

## Python

```bash
pip install pdfboss
```

Prebuilt abi3 wheels for CPython 3.12 and later; no Rust toolchain required.
The wheel compiles in the predefined CJK CMap set, so CJK-coded documents
extract out of the box.

Rendering with `fonts="full"` substitutes replacement faces for fonts the PDF
does not embed. Those faces ship as a separate package, pulled in by the
`full` extra:

```bash
pip install "pdfboss[full]"
```

This installs `pdfboss-fonts` alongside the wheel. Without it, `fonts="full"`
requires a `font_dir` argument pointing at faces of your own. See
[Rendering pages](./guide/rendering.md).

## CLI

```bash
cargo install pdfboss-cli
```

This installs the `pdfboss` binary. Two features are on by default:

- `substitute-fonts`: bundles the OFL Croscore substitute faces (about 4 MB)
  so `render` and `tui` can paint text for PDFs with non-embedded fonts out of
  the box.
- `predefined-cmaps`: compiles in the predefined CJK CMap set of ISO 32000
  Table 118 (about 830 KB) so `text` reads Shift-JIS/EUC/Big5/GBK/UHC-coded
  Type0 fonts.

Opt out of both for a leaner binary:

```bash
cargo install pdfboss-cli --no-default-features
```

## Rust crates

The library crates are on crates.io:

```bash
cargo add pdfboss-core pdfboss-text pdfboss-output pdfboss-render pdfboss-write pdfboss-markdown pdfboss-aio
```

| Crate | Responsibility |
|---|---|
| `pdfboss-core` | Parsing, object model, stream filters, document and page tree |
| `pdfboss-text` | Fonts, encodings, positional text spans |
| `pdfboss-output` | Layout analysis to plain text and Markdown |
| `pdfboss-render` | Rasterization to RGBA pixmaps and PNG, embedded-image extraction |
| `pdfboss-write` | PDF creation |
| `pdfboss-markdown` | CommonMark+GFM composed into themed PDFs (pulls in `pdfboss-style`) |
| `pdfboss-aio` | Async, range-fetching document access |

Add only what you use: `pdfboss-core` alone parses; the others build on it.

The library crates keep their optional features off by default:

- `pdfboss-render` `substitute-fonts`: the bundled substitute faces.
- `pdfboss-core` `predefined-cmaps`: the predefined CJK CMap set.
- `pdfboss-aio` `http`: remote documents over HTTP range requests.
- `pdfboss-aio` `write`: streaming created documents into tokio writers;
  ships `TokioSink`, which presents any `tokio::io::AsyncWrite` to
  `Pdf::write_into_with` (see [Creating PDFs](./guide/creating.md)).

Enable a feature at add time, for example:

```bash
cargo add pdfboss-aio --features http
```

## Building from source

```bash
git clone https://github.com/4thel00z/pdfboss
cd pdfboss
cargo build --release           # the CLI lands at target/release/pdfboss
cargo test --workspace          # Rust test suite
```

The Python extension builds with [maturin](https://www.maturin.rs) into the
active virtualenv:

```bash
maturin develop                 # build the extension into your venv
pytest                          # Python integration tests
```

Next: the [Quickstart](./quickstart.md).
