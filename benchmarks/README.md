# Benchmarks

Four scripts, because opening, rendering, scanned PDFs and non-Latin
corpora are different workloads.

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

`bench_diversity.py` measures quality — never timing — on corpora the other
benches never see: CJK, RTL/Arabic and academic PDFs. Everything above runs
on Latin-script corpora, and encoding behavior does not transfer across
scripts, so this is the regression harness for that blind spot.

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

## Method — diversity

- Per corpus directory, every engine (pdfboss, PyMuPDF, pypdfium2,
  pdfplumber — each import-guarded, missing ones skipped) is scored on:
  **open rate** (the file opens and reports a page count),
  **text-extraction non-crash rate**, **U+FFFD replacement-character rate**
  per doc, **markdown-extraction non-crash rate** (pdfboss only; the other
  engines have no comparable API), and **render page-1 non-blank rate**
  (more than 0.1% dark pixels, pdfboss at the `full` fonts tier so a blank
  page measures an encoding gap, not the embedded-only refusal).
- Text and markdown run over the first `--max-pages` pages (default 20),
  capped identically across engines, so a 400-page book weighs the same as
  an article and the per-character U+FFFD proxy is unchanged.
- The U+FFFD rate is the honest proxy for encoding gaps: replacement
  characters per extracted character. A doc that extracts *zero* characters
  is worse than one full of U+FFFD, so zero-text docs are tallied separately
  (`docs_with_zero_text`) and never score a flattering 0.0.
- **pdfboss currently lacks predefined-CMap support**, so Japanese documents
  from the 90ms-RKSJ era — the J-STAGE 2004-2006 slice the CJK fetch script
  targets — are expected to score poorly on the text metrics. This bench
  exists to measure that gap and to catch the improvement when CMap support
  lands.
- All metrics are quality, not timing, so they are load-insensitive and the
  results are published as measured. The JSON records counts and rates only,
  never file names; per-file character and U+FFFD counts print to stdout.

## Corpora fetch scripts

`corpora/fetch_cjk.sh`, `corpora/fetch_rtl.sh` and `corpora/fetch_academic.sh`
build the diversity corpora. Each writes into a user-supplied directory
**outside the repository**, sleeps between requests, sends a descriptive
User-Agent, and sniffs the `%PDF-` magic on every download so an HTML error
page or interstitial is discarded instead of polluting the open rate.

Licensing is tiered and embedded as comments in each script:

- **Redistribution-clean** (CC BY 4.0): Japanese government (soumu) white
  papers; Hindawi Foundation Arabic books via Wikimedia Commons.
- **Fetch-at-benchmark-time only** (never redistribute, never commit):
  J-STAGE articles (per-journal licensing), arXiv PDFs (arXiv-only
  distribution; link back to arXiv), UN Official Document System PDFs
  ("All rights reserved") — the UN source is opt-in via `UN_ODS=1`, rate
  limited to 1 request/second and capped at 20 documents.

The J-STAGE slice deliberately targets 2004-2006: that era of Japanese
typesetting used predefined CMaps (90ms-RKSJ-H/V) with non-embedded fonts,
which is exactly the hard CJK case. The fetch script pins one verified
RKSJ anchor article and fills the rest from the J-STAGE search API; in the
measured slice 6 of 15 files carry RKSJ CMaps.

Measured on the fetched slices (17 CJK, 10 RTL/Arabic, 15 academic files;
see `results-diversity.json`): every engine opens, extracts and renders
non-blank on 100% of all three corpora, and the gap shows exactly where the
proxy was designed to find it — pdfboss's mean per-doc U+FFFD rate is 35.9%
on the CJK slice (9 of 17 docs affected, worst doc 96.1%) versus 0 for the
other engines, 0 on the Arabic books, and 0.29% on the academic slice
(9 of 15 docs, worst 1.24%). The CJK numbers are the baseline the
predefined-CMap feature work will be measured against.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
pip install pdfboss-fonts               # substitute faces for bench_render.py's full tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py        /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py  /path/to/scan.pdf --pages 100 --repeat 3

# diversity corpora live OUTSIDE the repo; see the licensing tiers above
bash benchmarks/corpora/fetch_cjk.sh      ~/corpora/cjk 15
bash benchmarks/corpora/fetch_rtl.sh      ~/corpora/rtl 10
bash benchmarks/corpora/fetch_academic.sh ~/corpora/arxiv 2412 15
python benchmarks/bench_diversity.py ~/corpora/cjk ~/corpora/rtl ~/corpora/arxiv
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`;
`bench_diversity.py` writes `results-diversity.json`.
All corpora stay outside the repository and are never committed —
`bench_scans.py` records the document's page count and geometry, never its
name, and `bench_diversity.py` records counts and rates only.
