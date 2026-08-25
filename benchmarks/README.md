# Benchmarks

Four scripts, because opening, rendering, scanned PDFs and rendering
*quality* are different workloads.

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

`bench_fidelity.py` scores rendering *quality* instead of speed. The render
benchmark's ink gate proves a page is not blank, not that it is right; this
bench quantifies closeness to a reference rasterizer (pypdfium2) with windowed
SSIM, and scores the other engines against the same reference so pdfboss lands
in a field rather than being judged alone.

## Libraries

| Library | Open | Text | Render | Scan | Fidelity | Notes |
|---|:-:|:-:|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | ✓ | ref | pdfium bindings; no text API used here |
| pdfminer.six | | ✓ | | | | pure Python |
| pikepdf | ✓ | | | | | qpdf bindings; no text API |

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

## Method — fidelity

- The same deterministic, evenly-spaced sample as the other benches; the
  **first page** of each file, rendered by every library at `--scale 2.0` and
  pdfboss's `full` fonts tier.
- **Certification** — pdfboss renders the first page through
  `render_reporting`; a file reporting dropped or approximated content is
  excluded with its reason counted, because comparing a knowingly incomplete
  render against a reference would measure the known gap, not fidelity. A file
  where any library raises is excluded too, so every row scores the exact same
  pages.
- **Reference** — pypdfium2, the fastest C engine in the render benchmark.
  pdfplumber also rasterizes through pdfium, so its row approximates a
  same-engine control; PyMuPDF is the independent cross-engine baseline. Read
  pdfboss's score against those two rows, not against 1.0 — even the
  pdfium-family pair does not score 1.0, because each pipeline resamples and
  anti-aliases on its own.
- **Alignment** — each render is decoded to 8-bit grayscale (alpha composited
  onto white), center-cropped to the common minimum dimensions (absorbing the
  one-pixel size drift of engines that size pages via DPI), then
  Lanczos-downsampled to half resolution, which suppresses engine-specific
  anti-aliasing phase differences.
- **Metrics** — windowed SSIM with a uniform 8×8 window (integral-image box
  filter, K1=0.01, K2=0.03, L=255, mean over fully-inside windows) and mean
  absolute pixel difference on the 0–255 range. The JSON records each engine's
  median and p10 SSIM, median MAD, and the full sorted score distributions —
  never file names.
- SSIM is a quality metric, not a timing one, so the scores are insensitive to
  machine load and published as measured. On a local corpus of real-world
  PDFs (39 of 40 sampled files scored; 1 excluded for a glyph its substituted
  face lacks), two runs produce byte-identical results:

  | Engine | SSIM median | SSIM p10 | MAD median |
  |---|---|---|---|
  | PyMuPDF | 0.9909 | 0.9816 | 1.78 |
  | pdfplumber | 0.9876 | 0.9613 | 2.33 |
  | pdfboss | 0.9830 | 0.9590 | 2.60 |

  pdfboss's distance from the reference sits inside the band spanned by the
  other engines' rows: it disagrees with pdfium about as much as pdfium-based
  and independent C pipelines disagree among themselves.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow numpy matplotlib
pip install pdfboss-fonts               # substitute faces for the full fonts tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py          /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py   /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py    /path/to/scan.pdf --pages 100 --repeat 3
python benchmarks/bench_fidelity.py /path/to/pdfs --sample 40
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`;
`bench_fidelity.py` writes `results-fidelity.json`.
The datasets are local corpora of real-world PDFs and are not committed —
the results record corpus shape and score distributions, never file names
(`bench_scans.py` records the document's page count and geometry).
