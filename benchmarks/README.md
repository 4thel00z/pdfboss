# Benchmarks

Three scripts, because opening, rendering and scanned PDFs are different
workloads.

`bench.py` compares pdfboss against other Python PDF libraries on the two
operations they all produce comparable output for:

- **Open + parse** — open the file and read its page count.
- **Text extraction** — extract the text of every page.

`bench_render.py` compares rendering on the same corpus, but only on files it
can prove fair. pdfboss does not yet paint everything (see the top-level
README's Limitations), and timing a renderer that skipped work against full
renderers would credit it for the skipping. So every sampled file is certified
before the stopwatch starts, and the files that fail are excluded with their
reasons printed — never silently.

`bench_scans.py` benchmarks rendering where certification is unnecessary. A
scanned page is a single full-page bilevel image — JBIG2 or CCITT G3/G4 — with
no text operators, so there are no glyphs to paint and every library
rasterizes the same picture.

## Libraries

| Library | Open | Text | Render | Scan | Notes |
|---|:-:|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | ✓ | pdfium bindings; no text API used here |
| pdfminer.six | | ✓ | | | pure Python |
| pikepdf | ✓ | | | | qpdf bindings; no text API |

## Method

- A deterministic, evenly-spaced sample of the corpus (`--sample`, default 40).
- Each file is processed **best-of-N** (`--repeat`, default 3) after one warm-up
  pass, so OS file cache and imports are hot and the minimum time is kept.
- Each operation is aggregated **only over files every library handled**, so the
  reported totals compare the exact same workload.
- The headline metric is **pages per second** = (pages in the compared files) /
  (total time), which is independent of sample size.

## Method — render

- The same deterministic sample as `bench.py`.
- **Certification** — pdfboss renders every page of every sampled file through
  `render_reporting` at the `full` fonts tier, the tier that substitutes
  non-embedded simple fonts the way the other engines do by default. Any page
  reporting dropped or approximated content (an unpainted shading, a masked
  image, an annotation appearance, a glyph a loaded font lacks) excludes the
  file, and the exclusion reasons are printed and counted in the results.
- **Ink agreement** — content a *refused or failed* font would have painted is
  configured behavior, not a reported drop, so a second gate catches it: every
  library renders each file's first page, and a file where any library's ink
  coverage (percentage of dark pixels) falls outside a 2× band around the
  cross-library median is excluded too. A blank page renders instantly and
  means nothing. The band is wide because honest renders disagree: engines
  differ on anti-aliasing weight, and where a non-embedded bold face is
  substituted with a regular-weight one (a documented pdfboss approximation,
  which the other engines also make with their own faces) the same text
  carries visibly less ink.
- What survives is timed like `bench.py`: every page to **PNG bytes** (PNG
  encoding on every side), best-of-`--repeat` per file after one warm-up pass,
  aggregated only over files every library handled, reported as **pages per
  second**.

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

## Method — scan suite

One JBIG2 book is a narrow view of scanned PDFs, and since 0.17.1 the render
scale is an axis in its own right: at scale 1.0 a high-resolution bilevel
scan minifies several-fold (pdfboss averages the source footprint per device
pixel since 0.17.1), while at scale 2.0 it barely minifies — different code
paths, worth timing separately. So `--suite` widens `bench_scans.py` from one
document to a set and from one scale to a sweep:

- `--suite` takes a directory of PDFs or a list file (one path per line, `#`
  comments allowed); `--scales` (default `1.0,1.5,2.0`) sweeps the render
  scale. Every **(file, scale) cell** is timed separately — the same
  evenly-spaced `--pages` sample, one warm-up pass, best-of-`--repeat`.
- **Per-cell ink gate** — within each cell, every library's render of the
  first sampled page is measured for ink coverage, and a library outside the
  2× band around the cross-library median (past 0.15 percentage points of
  slack — `bench_render.py`'s constants) is excluded from that cell with the
  reason recorded. The per-scale totals aggregate only cells **every**
  library passed, so they compare the exact same workload.
- pdfboss's glyph-painting tier defaults to `full` in suite mode (`--fonts`
  overrides): a scanned document in the wild often opens with a typeset title
  page, and `full` substitutes non-embedded faces the way the other engines
  do by default. Single-file mode keeps its historical `all-embedded`
  default — on a pure scan the tier is irrelevant.
- A suggested suite is the 049 corpus's bilevel-and-JPEG-2000 slice: 42
  CCITT-bearing files (for example 049004, 049012, 049031, 049061, 049103,
  049104, 049107, 049109), 2 JPX files (049124, 049359) and 2 JBIG2 files
  (049373, 049396). CCITT-bearing does not always mean scan-shaped — 049004
  is a vector map with a fax inset, and cells like it gate out engines whose
  ink diverges on the vector content. That is the gate doing its job; the
  cell records the exclusion.

Suite results table: **pending a quiet-machine pass.** Suite mode is
smoke-tested (2 files × 2 scales, per-cell gates and per-scale totals
behaving), but the machine that built it was under heavy parallel load, so
no wall-clock numbers are published yet.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
pip install pdfboss-fonts               # substitute faces for bench_render.py's full tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py        /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py  /path/to/scan.pdf --pages 100 --repeat 3
python benchmarks/bench_scans.py  --suite /path/to/scans --scales 1.0,1.5,2.0 --repeat 3
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`, and in
suite mode `results-scans-suite.json` (saved after every cell, so a crash
keeps what finished; cells are keyed by suite position and page count, never
by file name).
Both datasets are local corpora of real-world PDFs and are not committed —
`bench_scans.py` records the document's page count and geometry, never its
name.
