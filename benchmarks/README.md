# Benchmarks

Seven scripts, because opening, rendering, scanned PDFs, rendering
*quality*, parallel throughput, malformed PDFs and memory are different
workloads — plus two
extraction-quality suites (`olmocr/`, `parsebench/`), because speed means
nothing if the output is wrong.

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

`bench_robustness.py` turns the filtering around: instead of keeping only the
files every engine handles, it feeds them fuzzer-minimized malformed PDFs and
measures survival — page count, clean exception, crash, or hang. Every other
benchmark here calls the engines in-process, where a segfault in a C engine
kills the whole run; this one and `bench_memory.py` share a subprocess
harness (`isolation.py`) that runs each measurement in a fresh interpreter
precisely so a crash is a data point instead of a disaster.

`bench_memory.py` measures peak RSS per engine on the same harness — one
fresh process per (engine, workload), each reporting its own high-water mark,
because a peak is meaningless once four engines have allocated in the same
address space.

`bench_parallel.py` measures what the sequential scripts deliberately do not:
they time per-page loops, which understates every engine that can use more
than one core. pdfboss's `Document.render_pages` and `Document.extract_text`
fan pages out across the machine's cores in one call, and the competing
engines can thread too — so each engine is timed twice on the same workload,
sequential baseline and its best available parallel route, and the speedup is
reported per engine.

## Libraries

| Library | Open | Text | Render | Scan | Fidelity | Parallel | Robustness | Memory | Notes |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | | | | | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | ✓ | ref | ✓ | ✓ | ✓ | pdfium bindings; text API used by bench_memory.py |
| pdfminer.six | | ✓ | | | | | | | pure Python |
| pikepdf | ✓ | | | | | | | | qpdf bindings; no text API |

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


## Method — robustness

- The corpus is malformed by construction: the OSS-Fuzz **public corpora** for
  `mupdf_pdf_fuzzer` and `poppler_pdf_fuzzer` (~230 MB, tens of thousands of
  hash-named seeds each), downloaded by `fetch_stress_corpus.sh` into a
  directory **outside the repo** — fuzzer-minimized inputs are fine to fetch
  and use locally, and are never committed or redistributed from here.
- Provenance, for fairness: the seeds were minimized against two specific C
  engines (MuPDF and Poppler), and one tested library binds one of them. That
  does not materially bias the comparison — a malformed file is malformed for
  every parser, and the metric is process survival, not output fidelity — but
  it is the corpus's origin and belongs in the open.
- **Isolation** — every (file, engine) pair runs in a fresh interpreter (the
  script re-runs itself in worker mode via `isolation.py`), because two of the
  engines are in-process C libraries whose segfaults cannot be caught. Each
  worker runs two stages, **parse** (open + page count) then **render** (first
  page to pixels), printing a flushed stage marker before each so a dead
  process is attributed to the stage that was running.
- **Classification** — per stage: `ok`, `error` (a clean Python exception),
  `crash` (signal, nonzero exit, or exit without a result — which is what a
  swallowed `SystemExit` looks like from outside), `timeout` (wall-clock
  `--timeout`, default 20 s, then SIGKILL). The headline is the **survival
  rate**: the share of files an engine processed with no crash and no timeout.
- A deterministic, evenly-spaced sample of the sorted corpus (`--sample`,
  default 2000). Counts and rates are load-insensitive except right at the
  timeout threshold, and 20 s is orders of magnitude above a typical parse of
  these mostly tiny seeds.
- Read the survival rate next to the per-stage `ok` counts: an engine that
  cleanly refuses most malformed files exercises far less of its own code than
  one that parses and renders them, so a high survival rate on few accepted
  files is a weaker statement than the same rate on many.

## Method — memory

- Peak RSS per engine, one **fresh subprocess per (engine, workload)** on the
  same `isolation.py` harness, so no engine's allocations sit inside another's
  peak. The child measures itself with
  `resource.getrusage(RUSAGE_SELF).ru_maxrss` just before exiting — macOS
  reports that in **bytes**, Linux in **kilobytes**, and the worker normalizes
  to bytes so results compare across platforms.
- Three workloads: **import** (import the engine and stop — the floor under
  the other two numbers, since every peak includes the interpreter and the
  engine's own libraries), **render** (the corpus's largest file, `--pages`
  evenly spaced pages to PNG bytes at `--scale`, default 10 pages at 2.0), and
  **text** (every page of the `--sample` evenly spaced corpus files, default
  40, accumulating string lengths only so no side carries a giant joined
  string).
- pdfboss renders at `fonts=full`, so its substitute-face loading — work the
  other engines also do, with their own faces — is inside its measured
  footprint.
- Peak RSS is mostly load-insensitive, but the published numbers were measured
  on a shared machine under concurrent load; co-resident processes can shift
  them a few percent through allocator and page-cache pressure.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow numpy matplotlib
pip install pdfboss-fonts               # substitute faces for the full fonts tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py          /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py   /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py    /path/to/scan.pdf --pages 100 --repeat 3
python benchmarks/bench_fidelity.py /path/to/pdfs --sample 40
python benchmarks/bench_parallel.py scan /path/to/scan.pdf --pages 60 --repeat 3
python benchmarks/bench_parallel.py text /path/to/pdfs --sample 40 --repeat 3

benchmarks/fetch_stress_corpus.sh /outside/repo/stress-corpus   # ~460 MB once
python benchmarks/bench_robustness.py /outside/repo/stress-corpus --sample 2000
python benchmarks/bench_memory.py     /path/to/pdfs --sample 40 --pages 10 --scale 2.0
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`;
`bench_fidelity.py` writes `results-fidelity.json`; `bench_parallel.py`
writes `results-parallel.json` (the two workloads merge into one file, saved
incrementally so a crashed engine keeps what finished); `bench_robustness.py`
writes `results-robustness.json`; `bench_memory.py` writes
`results-memory.json`. The real-world corpora are local and not committed —
those results record corpus shape, page counts, sizes and score
distributions, never file names. The stress corpus is public (OSS-Fuzz) and
its results name it, but it stays outside the repo too.

## olmOCR-bench

[olmocr/](olmocr/) wires pdfboss into
[olmOCR-bench](https://huggingface.co/datasets/allenai/olmOCR-bench), a
public suite of 7,010 machine-checkable tests (text presence, reading order,
table structure, math rendering) over 1,403 single-page PDFs.
`olmocr/generate_candidates.py` writes the markdown candidate tree the
suite's scorer reads; results land in `results-olmocr.json`. pdfboss is a
non-OCR engine, so the honest headline is the born-digital buckets — the
scan and LaTeX-math buckets score near zero by construction. The recipe and
the full interpretation notes are in [olmocr/README.md](olmocr/README.md).

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
