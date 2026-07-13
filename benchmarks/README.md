# Benchmarks

`bench.py` compares pdfboss against other Python PDF libraries on the two
operations they all produce comparable output for:

- **Open + parse** — open the file and read its page count.
- **Text extraction** — extract the text of every page.

Rendering is **not** benchmarked: pdfboss's rasterizer does not yet paint every
glyph, so timing its incomplete output against full renderers would be
misleading.

## Libraries

| Library | Open | Text | Notes |
|---|:-:|:-:|---|
| pdfboss | ✓ | ✓ | this project (Rust) |
| PyMuPDF | ✓ | ✓ | C-backed |
| pypdf | ✓ | ✓ | pure Python |
| pdfplumber | ✓ | ✓ | pure Python (on pdfminer.six) |
| pdfminer.six | | ✓ | pure Python |
| pikepdf | ✓ | | qpdf bindings; no text API |

## Method

- A deterministic, evenly-spaced sample of the corpus (`--sample`, default 40).
- Each file is processed **best-of-N** (`--repeat`, default 3) after one warm-up
  pass, so OS file cache and imports are hot and the minimum time is kept.
- Each operation is aggregated **only over files every library handled**, so the
  reported totals compare the exact same workload.
- The headline metric is **pages per second** = (pages in the compared files) /
  (total time), which is independent of sample size.

## Running

```bash
pip install pypdf pdfminer.six pdfplumber pikepdf pymupdf matplotlib
maturin develop --release           # build pdfboss into the venv
python benchmarks/bench.py /path/to/pdfs --sample 40 --repeat 3
```

This writes `results.json` (raw numbers) and `results.png` (the chart shown in
the top-level README). The dataset is a local corpus of real-world PDFs and is
not committed.
