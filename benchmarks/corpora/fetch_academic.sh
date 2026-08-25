#!/usr/bin/env bash
# Fetch an academic (arXiv) benchmark corpus for benchmarks/bench_diversity.py.
#
# Source and licensing:
#   arXiv PDFs via the free GCS mirror gs://arxiv-dataset (anonymous HTTPS,
#   no billing). FETCH-ONLY: most arXiv papers grant arXiv-only distribution
#   rights, and tools built on the full text must link back to arXiv —
#   benchmark locally, never redistribute or commit the PDFs. The official
#   (requester-pays) fallback is s3://arxiv/pdf/.
#
# Usage: fetch_academic.sh OUTDIR [YYMM] [COUNT]
#   YYMM  month slice, e.g. 2412 for December 2024 (default 2412)
# OUTDIR must live OUTSIDE the repository; corpus files are never committed.
set -euo pipefail

outdir="${1:?usage: fetch_academic.sh OUTDIR [YYMM] [COUNT]}"
yymm="${2:-2412}"
count="${3:-15}"
ua="pdfboss-bench/0.1 (+https://github.com/4thel00z/pdfboss)"
mkdir -p "$outdir"

keep_if_pdf() {
  head -c 1024 "$1" | grep -aq '%PDF-' && return 0
  echo "    not a PDF, discarded: $(basename "$1")"
  rm -f "$1"
  return 1
}

echo "[arxiv] up to $count v1 papers from $yymm via the GCS mirror"
urls=$(curl -s -A "$ua" \
  "https://storage.googleapis.com/storage/v1/b/arxiv-dataset/o?prefix=arxiv/arxiv/pdf/$yymm&maxResults=200&fields=items(name)" \
  | python3 -c "
import json, sys
items = json.load(sys.stdin).get('items', [])
for i in items:
    if i['name'].endswith('v1.pdf'):
        print('https://storage.googleapis.com/arxiv-dataset/' + i['name'])
")
n=0
for u in $urls; do
  [ "$n" -ge "$count" ] && break
  name="arxiv_$(basename "$u")"
  [ -f "$outdir/$name" ] && { n=$((n + 1)); continue; }
  curl -sL -A "$ua" "$u" -o "$outdir/$name"
  sleep 1
  keep_if_pdf "$outdir/$name" || continue
  n=$((n + 1))
done
echo "[arxiv] $n files"

echo "[done] $(ls "$outdir"/*.pdf 2>/dev/null | wc -l | tr -d ' ') PDFs in $outdir"
