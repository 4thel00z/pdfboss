# Benchmarks

Two scripts, because scanned PDFs and text PDFs are different workloads.

`bench.py` compares pdfboss against other Python PDF libraries on the two
operations they all produce comparable output for:

- **Open + parse** — open the file and read its page count.
- **Text extraction** — extract the text of every page.

Rendering is **not** benchmarked there: pdfboss's rasterizer does not yet paint
every glyph, so timing its incomplete output against full renderers would be
misleading.

`bench_scans.py` benchmarks exactly what `bench.py` leaves out, on the one
corpus where it is fair. A scanned page is a single full-page bilevel image —
JBIG2 or CCITT G3/G4 — with no text operators, so there are no glyphs to paint
and every library rasterizes the same picture.

## Libraries

| Library | Open | Text | Scan | Notes |
|---|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | pdfium bindings; no text API used here |
| pdfminer.six | | ✓ | | pure Python |
| pikepdf | ✓ | | | qpdf bindings; no text API |

## Method

- A deterministic, evenly-spaced sample of the corpus (`--sample`, default 40).
- Each file is processed **best-of-N** (`--repeat`, default 3) after one warm-up
  pass, so OS file cache and imports are hot and the minimum time is kept.
- Each operation is aggregated **only over files every library handled**, so the
  reported totals compare the exact same workload.
- The headline metric is **pages per second** = (pages in the compared files) /
  (total time), which is independent of sample size.

## Method — scans

- One scanned document, sampled at `--pages` evenly spaced pages (default 50).
- Every library renders those pages to **PNG bytes** at `--scale`, so PNG
  encoding is on every side of the comparison.
- Best-of-`--repeat` for the whole pass, after one warm-up pass.
- Before timing, each library's render of the first page is measured for **ink
  coverage** (percentage of dark pixels). A library that cannot decode the
  scan's codec usually returns a blank page rather than raising — that renders
  instantly and means nothing, and disagreeing coverage is what catches it.
  The renders are not pixel-identical: each library downsamples the scan onto
  the page with its own resampling.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py       /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py /path/to/scan.pdf --pages 100 --repeat 3
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_scans.py` writes `results-scans.json`.
Both datasets are local corpora of real-world PDFs and are not committed —
`bench_scans.py` records the document's page count and geometry, never its
name.
