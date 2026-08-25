# Benchmarks

Five scripts, because opening, rendering, scanned PDFs, malformed PDFs and
memory are different workloads.

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

## Libraries

| Library | Open | Text | Render | Scan | Robustness | Memory | Notes |
|---|:-:|:-:|:-:|:-:|:-:|:-:|---|
| pdfboss | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | | | | | pure Python; no rasterizer |
| pdfplumber | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | text via pdfminer.six, rasterizing via pdfium |
| pypdfium2 | | | ✓ | ✓ | ✓ | ✓ | pdfium bindings; text API used by bench_memory.py |
| pdfminer.six | | ✓ | | | | | pure Python |
| pikepdf | ✓ | | | | | | qpdf bindings; no text API |

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
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf pypdfium2 pillow matplotlib
pip install pdfboss-fonts               # substitute faces for bench_render.py's full tier
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py        /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_render.py /path/to/pdfs --sample 40 --repeat 3
python benchmarks/bench_scans.py  /path/to/scan.pdf --pages 100 --repeat 3

benchmarks/fetch_stress_corpus.sh /outside/repo/stress-corpus   # ~460 MB once
python benchmarks/bench_robustness.py /outside/repo/stress-corpus --sample 2000
python benchmarks/bench_memory.py     /path/to/pdfs --sample 40 --pages 10 --scale 2.0
```

`bench.py` writes `results.json` (raw numbers) and `results.png` (the chart
shown in the top-level README); `bench_render.py` writes
`results-render.json`; `bench_scans.py` writes `results-scans.json`;
`bench_robustness.py` writes `results-robustness.json`; `bench_memory.py`
writes `results-memory.json`. The real-world corpora are local and not
committed — those results record page counts, sizes and directory basenames,
never file names. The stress corpus is public (OSS-Fuzz) and its results name
it, but it stays outside the repo too.
