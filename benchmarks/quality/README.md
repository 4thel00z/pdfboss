# Extraction quality

The top-level README's quality table comes from
[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench),
a 200-document corpus with ground-truth Markdown and an evaluator that scores
reading order (NID), heading structure (MHS) and table structure (TEDS).
The corpus and evaluator live in that repository; this directory holds the
two pdfboss adapters and the recipe that reproduces the table's pdfboss rows.

## Setup

Clone the benchmark's results branch sparsely — the corpus, ground truth and
evaluator, without the accumulated prediction history:

```bash
git clone --depth 1 --branch abi/pdf-parser-benchmark-results --filter=blob:none \
    --sparse https://github.com/firecrawl/opendataloader-bench.git
cd opendataloader-bench
git sparse-checkout set src pdfs ground-truth
```

Create a venv with the evaluator's needs and a **release** pdfboss wheel.
Skip the repo's own `uv sync` — it pulls the OCR engines' full ML stack:

```bash
python -m venv .venv && source .venv/bin/activate
pip install rapidfuzz lxml apted beautifulsoup4 py-cpuinfo pdfboss
```

## Register the engines

Copy the two adapters next to the runner:

```bash
cp /path/to/pdfboss/benchmarks/quality/pdf_parser_pdfboss*.py src/
```

Then register them in `src/engine_registry.py`: add both engines to
`ENGINES` (the value is the version, `python -c "import pdfboss;
print(pdfboss.__version__)"`), and their modules to `_ENGINE_MODULES`:

```python
ENGINES: Dict[str, str] = {
    ...
    "pdfboss": "0.15.0",
    "pdfboss-text": "0.15.0",
}

_ENGINE_MODULES: Dict[str, str] = {
    ...
    "pdfboss": "pdf_parser_pdfboss",
    "pdfboss-text": "pdf_parser_pdfboss_text",
}
```

## Run

```bash
cd src
python pdf_parser.py --engine pdfboss        # writes prediction/pdfboss/markdown/
python pdf_parser.py --engine pdfboss-text
python evaluator.py  --engine pdfboss        # writes prediction/pdfboss/evaluation.json
python evaluator.py  --engine pdfboss-text
```

`evaluation.json` carries the per-document and aggregate scores; the README
table's NID column is its reading-order aggregate. `pdf_parser.py` also
writes a `summary.json` with the wall-clock time per engine — the README's
timing protocol is the median of five such runs after a warm-up, single
process, on an otherwise idle machine.

The plain-text engine is scored as if its output were Markdown, so it earns
nothing on headings or tables; NID is the one number that means something
for it, and that is how the README table presents it.
