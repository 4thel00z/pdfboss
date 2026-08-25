# olmOCR-bench

[olmOCR-bench](https://huggingface.co/datasets/allenai/olmOCR-bench) scores
PDF-to-markdown conversion with 7,010 machine-checkable unit tests over 1,403
single-page PDFs: is this text present (or absent), does this text come
before that text, does this table cell carry that heading, does this equation
render to the same symbols. The scorer is the `olmocr` package's bench
module; this directory holds the candidate generator and the recipe that
produced pdfboss's scores in `../results-olmocr.json`.

## Reading the numbers honestly

pdfboss extracts the text a PDF carries; it does not OCR pixels. The
benchmark mixes both worlds, and the per-bucket table only means something
with the text-layer census next to it (the generator prints it — `empty` is
the count of PDFs whose extraction produced zero characters):

- **Born-digital buckets** — `table_tests`, `multi_column`,
  `headers_footers`, and the auto-added per-PDF `baseline` — are the honest
  headline for a non-OCR engine. Nearly every PDF there has a text layer.
- **Scan buckets** — `old_scans` is 98/98 image-only, `old_scans_math` 21/36,
  `long_tiny_text` 23/62. Scores there measure the OCR pdfboss does not do
  (plus, for the two partial buckets, whatever embedded text the scans carry).
- **`arxiv_math` is ~0 by construction**: its 2,927 tests require LaTeX
  inside `$…$`-family delimiters and are checked by rendering both sides with
  KaTeX. The PDFs have text layers, but a rendered PDF's text layer holds
  glyphs, not LaTeX source — no text extractor scores here.
- **`headers_footers` is all absence tests** — the engine is rewarded for
  *stripping* page headers, footers and page numbers. An empty output would
  pass them all, so the number only means something next to `baseline`
  (which requires real content per PDF). pdfboss keeps page headers and
  footers in its markdown today, and the low score reflects that choice.
- **Markdown tables cap the table score**: tests that hinge on rowspan or
  colspan structure only pass with HTML `<table>` output (stated in the
  benchmark's README). pdfboss emits pipe tables.
- The overall score is a macro-average of the 8 buckets, each weighted
  equally — the scan and math buckets pull a non-OCR engine's overall down
  by construction. The leaderboard's engines OCR a rasterized page, which is
  a different (strictly harder) task on the scan buckets.

## Setup

```bash
python3.12 -m venv olmbench && source olmbench/bin/activate
git clone --depth 1 https://github.com/allenai/olmocr.git
pip install -e './olmocr[bench]' numpy pdfboss
playwright install chromium   # math references render in Chromium at jsonl load time

huggingface-cli download --repo-type dataset allenai/olmOCR-bench --local-dir ./olmOCR-bench
```

## Generate the candidate

The scorer treats every subdirectory of `bench_data/` except `pdfs/` as a
candidate and hard-fails one that is missing the markdown for *any* PDF, so
the generator writes a file per PDF unconditionally — empty on failure, which
merely fails that PDF's own tests:

```bash
python benchmarks/olmocr/generate_candidates.py ./olmOCR-bench/bench_data
```

It prints the per-category text-layer census and refuses to finish with
fewer outputs than PDFs.

## Score

```bash
# All 8 buckets (the numbers in ../results-olmocr.json):
python -m olmocr.bench.benchmark --dir ./olmOCR-bench/bench_data --candidate pdfboss

# A single bucket — pass its jsonl as --dir; baseline is then restricted to
# that bucket's PDFs, so single-bucket numbers do not mix into the full table:
python -m olmocr.bench.benchmark --dir ./olmOCR-bench/bench_data/table_tests.jsonl --candidate pdfboss
```

The first full run renders every reference equation in headless Chromium
(cached in sqlite under `~/.cache/olmocr/bench/equations/`), so it is slow
once and fast after.

## Results

pdfboss 0.17.1, one repeat (extraction is deterministic), full 8-bucket run.
Pass rates are load-insensitive and reported as measured; raw numbers in
[`../results-olmocr.json`](../results-olmocr.json).

| Bucket | Pass rate | Tests |
|---|--:|--:|
| baseline | 87.3% | 1394 |
| headers_footers | 35.0% | 760 |
| table_tests | 30.0% | 1022 |
| long_tiny_text | 22.2% | 442 |
| multi_column | 18.9% | 884 |
| old_scans | 13.3% | 526 |
| old_scans_math | 0.0% | 458 |
| arxiv_math | 0.0% | 2927 |
| **overall (macro over 8 buckets)** | **25.8% ± 0.9%** | 8413 |
| **born-digital subset (baseline, tables, multi_column, headers_footers)** | **42.8%** | 4060 |

(A few jsonls carry baseline-type tests of their own, which is why the test
counts differ slightly from the bucket sizes; the born-digital subset number
is the macro over those four rows of this same run, not a separate run.
old_scans' 13.3% is exactly its 70 absence tests, passing trivially on
empty output — the fully image-only bucket contributes nothing real.)
