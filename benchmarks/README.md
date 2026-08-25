# Benchmarks

Four scripts, because opening, rendering, scanned PDFs and parallel throughput
are different workloads.

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

`bench_parallel.py` measures what the other three deliberately do not: the
sequential scripts time per-page loops, which understates every engine that
can use more than one core. pdfboss's `Document.render_pages` and
`Document.extract_text` fan pages out across the machine's cores in one call,
and the competing engines can thread too — so each engine is timed twice on
the same workload, sequential baseline and its best available parallel route,
and the speedup is reported per engine.

## Libraries

| Library | Open | Text | Render | Scan | Parallel | Notes |
|---|:-:|:-:|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | ✓ | ✓ | pdfium bindings; no text API used in `bench.py` |
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

## Method — parallel

Two workloads, one results file (`results-parallel.json`, sections merge):

- **scan** — render `--pages` evenly spaced pages (default 60) of one scanned
  document to PNG bytes at `--scale`, the `bench_scans.py` workload where
  every engine rasterizes the same bilevel picture and no glyph-painting gate
  is needed. The `bench_scans.py` ink check runs first: each engine's render
  of the first sampled page is measured for dark-pixel coverage, printed and
  recorded — the renders must agree.
- **text** — extract the text of every page of a corpus sample (`--sample`,
  default 40), the `bench.py` workload. Extracted character counts are
  recorded per engine, so an engine extracting nothing is visible.

Each engine is timed in both modes, best-of-`--repeat` after one warm-up pass
per mode; the text aggregate keeps only files every engine handled in both
modes, so the totals compare the exact same workload. `--threads` (default:
the machine's cores) sizes the competitors' pools.

Per-engine parallel routes, documented because they are the comparison:

- **pdfboss** — one call: `render_pages` / `extract_text`, with the same
  explicit `scale`/`fonts` arguments as its sequential per-page loop. The
  call is internally parallel (one worker per core, each holding its own fork
  of the document — shared bytes and cross-reference table, private caches);
  it has no thread knob, so `--threads` does not apply to it.
- **PyMuPDF** — `ThreadPoolExecutor` over per-page calls, one document handle
  per worker thread (its objects are not thread-safe across a shared handle).
  Whatever its internal serialization then allows is its parallel story.
- **pypdfium2, pdfplumber rendering** — pdfium's contract requires callers to
  serialize every pdfium call across threads, and it means it: threaded
  rendering with one document per worker intermittently corrupts pdfium's
  document loader for the rest of the process, and pdfplumber's `to_image`
  (a fresh pdfium document open/close inside every call) crashes the process
  outright — both reproduced while building this bench. The harness therefore
  serializes all pdfium calls under one lock and threads only the PNG
  encoding; the resulting near-1x IS the pdfium threading story. Parallel
  pdfium in Python means one process per worker, a different workload shape
  than this in-process comparison.
- **pdfplumber text** — threads with one document per worker; extraction is
  pure Python, so it stays GIL-bound. pypdfium2's text goes through its
  textpage API here (the Libraries table's "no text API used here" describes
  `bench.py`).

Results table: **pending a quiet-machine pass.** The script is smoke-tested
(both workloads, every engine timed in both modes), but the machine that
built it was under heavy parallel load, so no wall-clock numbers are
published yet.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
pip install pdfboss-fonts               # substitute faces for bench_render.py's full tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py        /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py  /path/to/scan.pdf --pages 100 --repeat 3
python benchmarks/bench_parallel.py scan /path/to/scan.pdf --pages 60 --repeat 3
python benchmarks/bench_parallel.py text /path/to/pdfs --sample 40 --repeat 3
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`;
`bench_parallel.py` writes `results-parallel.json` (the two workloads merge
into one file, saved incrementally so a crashed engine keeps what finished).
Both datasets are local corpora of real-world PDFs and are not committed —
the results record page counts, shapes and corpus directory basenames, never
file names.
