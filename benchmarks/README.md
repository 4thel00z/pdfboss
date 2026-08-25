# Benchmarks

Three scripts, because opening, rendering and scanned PDFs are different
workloads — plus a quality benchmark under `parsebench/`, because speed
means nothing if the output is wrong.

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

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
pip install pdfboss-fonts               # substitute faces for bench_render.py's full tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py        /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py  /path/to/scan.pdf --pages 100 --repeat 3
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`.
Both datasets are local corpora of real-world PDFs and are not committed —
`bench_scans.py` records the document's page count and geometry, never its
name.

## Method — ParseBench (quality)

`parsebench/` wires pdfboss into
[run-llama/ParseBench](https://github.com/run-llama/ParseBench): 2,078
human-verified pages, five quality dimensions, ~169k deterministic rules,
no LLM judge. `parsebench/pdfboss_provider.py` is a drop-in provider for
their tree (per-page markdown, pipe tables converted to HTML for the table
metrics the same way their pdf_inspector provider does it); the wiring
steps, full method and score interpretation live in
[`parsebench/README.md`](parsebench/README.md). Scores are rule-based and
deterministic, so unlike the timing benchmarks they are load-insensitive
and published as measured: **28.00 overall** for pdfboss 0.17.1 (Content
Faithfulness 61.50, Semantic Formatting 33.78, Tables 28.89, Visual
Grounding 10.77, Charts 5.04), from a full 2,078-example run at ParseBench
commit 34b7345. Raw aggregates land in `results-parsebench.json`.
