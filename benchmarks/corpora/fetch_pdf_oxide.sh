#!/usr/bin/env bash
# Fetch the three public suites pdf_oxide (yfedoseev/pdf_oxide) benchmarks
# against — the corpus benchmarks/bench_spans.py sweeps:
#
#   verapdf/   veraPDF-corpus (PDF/A compliance files, ~2,900 PDFs)
#   pdfjs/     Mozilla pdf.js test/pdfs (~950 real-world regression PDFs)
#   safedocs/  pdf-association/safedocs (targeted edge cases)
#
# Source and licensing:
#   veraPDF-corpus is CC BY 4.0; pdf.js and safedocs test files carry their
#   repositories' own mixed licenses. FETCH-ONLY: benchmark locally, never
#   redistribute or commit the PDFs.
#
# Usage: fetch_pdf_oxide.sh OUTDIR
# OUTDIR must live OUTSIDE the repository; corpus files are never committed.
set -euo pipefail

outdir="${1:?usage: fetch_pdf_oxide.sh OUTDIR}"
mkdir -p "$outdir"

if [ ! -d "$outdir/verapdf" ]; then
  echo "[verapdf] cloning veraPDF-corpus"
  git clone --depth 1 --quiet https://github.com/veraPDF/veraPDF-corpus.git "$outdir/verapdf"
fi
echo "[verapdf] $(find "$outdir/verapdf" -name '*.pdf' | wc -l | tr -d ' ') PDFs"

if [ ! -d "$outdir/pdfjs" ]; then
  echo "[pdfjs] sparse-cloning pdf.js test/pdfs"
  git clone --depth 1 --filter=blob:none --sparse --quiet \
    https://github.com/mozilla/pdf.js.git "$outdir/pdfjs"
  git -C "$outdir/pdfjs" sparse-checkout set test/pdfs
fi
echo "[pdfjs] $(find "$outdir/pdfjs" -name '*.pdf' | wc -l | tr -d ' ') PDFs"

if [ ! -d "$outdir/safedocs" ]; then
  echo "[safedocs] cloning pdf-association/safedocs"
  git clone --depth 1 --quiet https://github.com/pdf-association/safedocs.git "$outdir/safedocs"
fi
echo "[safedocs] $(find "$outdir/safedocs" -name '*.pdf' | wc -l | tr -d ' ') PDFs"

echo "[done] $(find "$outdir" -name '*.pdf' | wc -l | tr -d ' ') PDFs in $outdir"
