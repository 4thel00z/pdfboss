# ParseBench

[run-llama/ParseBench](https://github.com/run-llama/ParseBench) measures
document parsing quality on 2,078 human-verified enterprise pages
(insurance/finance/government) across five dimensions — Tables, Charts,
Content Faithfulness, Semantic Formatting, Visual Grounding — with ~169k
deterministic test rules and no LLM judge. It is the quality complement to
the timing benchmarks in this directory: same engines, different question.

`pdfboss_provider.py` is a drop-in provider for the ParseBench tree. It
extracts per-page markdown with `Page.extract_markdown()` and converts the
pipe tables to HTML `<table>` markup in `normalize()` — the Tables
similarity metrics (TEDS/GriTS/TableRecordMatch) consume HTML tables only,
while the rule metrics parse both forms. That conversion (markdown2 with the
`tables` extra, a core harness dependency) is the same normalization the
pdf_inspector provider applies, so the comparison stays fair. Non-PDF inputs
are rejected with their `ProviderPermanentError`, which is how the
pypdf/PyMuPDF baselines handle the 42 jpg/png Visual Grounding inputs too.

## Wiring (verified against ParseBench commit 34b7345)

```bash
git clone https://github.com/run-llama/ParseBench && cd ParseBench
uv sync --extra runners --extra fast   # fast = numba-JIT TEDS, identical scores
uv add pdfboss==0.17.1

cp /path/to/pdfboss/benchmarks/parsebench/pdfboss_provider.py \
   src/parse_bench/inference/providers/parse/pdfboss.py
```

Then two registrations:

- `src/parse_bench/inference/providers/parse/__init__.py` — add
  `"pdfboss"` to `_PROVIDER_MODULES`.
- `src/parse_bench/inference/pipelines/parse.py` — inside
  `register_parse_pipelines()`:

```python
register_fn(
    PipelineSpec(
        pipeline_name="pdfboss_markdown",
        provider_name="pdfboss",
        product_type=ProductType.PARSE,
        config={},
    )
)
```

## Running

```bash
uv run parse-bench run pdfboss_markdown --test              # smoke: 3 files/category
uv run parse-bench run pdfboss_markdown --max_concurrent 8  # full: auto-downloads ~0.6 GB from HF
uv run parse-bench run pdfboss_markdown --group table       # one dimension
uv run parse-bench serve output/pdfboss_markdown            # per-rule drill-down
```

No API keys: local engines run offline, LLM normalization is off by
default, and every metric is rule-based and deterministic — the scores are
reproducible and load-insensitive, so they are published as measured.

## Results — pdfboss 0.17.1, full run (2,078 examples)

| Dimension | Headline metric | Score |
|---|---|--:|
| Content Faithfulness | content_faithfulness | 61.50 |
| Semantic Formatting | semantic_formatting | 33.78 |
| Tables | grits_trm_composite | 28.89 |
| Visual Grounding | rule_pass_rate | 10.77 |
| Charts | chart_data_point_pass_rate | 5.04 |
| **Overall** | mean of the five | **28.00** |

Raw aggregates in `../results-parsebench.json`. Against the public
leaderboard's "Open Source - Local" bracket (numbers from the repo's
`leaderboard.csv` at the same commit, not rerun locally):

| Engine | Overall |
|---|--:|
| Warp Ingest | 40.18 |
| LiteParse (no OCR) | 32.80 |
| OpenDataLoader | 29.40 |
| **pdfboss** | **28.00** |
| pdf-inspector | 26.59 |
| MarkItDown | 18.63 |
| PyMuPDF (HTML) | 16.62 |
| PyMuPDF (Text) | 16.02 |
| pypdf | 14.87 |

## Reading the numbers

- **Charts is effectively ML-only** — the pages are chart images, and
  text-layer engines land at 0–7 across the board.
- **Visual Grounding** scores only the 64 reading-order examples; the other
  436 need a per-page layout payload (`ParseLayoutPageIR` items with
  normalized bboxes) that this provider does not emit, plus the 42 image
  inputs. The zeroing is identical for the pypdf/PyMuPDF baselines
  (verified in-harness). Follow-up: expose block geometry from pdfboss's
  layout IR through the Python bindings and emit the payload — no non-ML
  local engine on the public leaderboard does.
- **Content Faithfulness** is dragged by two by-design choices: pdfboss
  markdown drops page headers/footers (the is_header/is_footer rules score
  zero) and scan-only pages have no text layer to extract.
