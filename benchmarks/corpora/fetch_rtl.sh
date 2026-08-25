#!/usr/bin/env bash
# Fetch an RTL/Arabic benchmark corpus for benchmarks/bench_diversity.py.
#
# Sources and licensing:
#   1. Hindawi Foundation Arabic books hosted on Wikimedia Commons — born
#      digital, professionally typeset, CC BY 4.0 per the Commons file pages.
#      Redistribution-clean (94 files total on Commons).
#   2. (opt-in, UN_ODS=1) UN Official Document System, Arabic General
#      Assembly resolutions, with the same symbols available in English
#      (l=E) as parallel ground truth. FETCH-ONLY: un.org copyright says
#      "All rights reserved" — never redistribute or commit these files.
#      Rate-limited to 1 request/second, capped at 20 documents.
#
# Usage: fetch_rtl.sh OUTDIR [HINDAWI_COUNT]
# OUTDIR must live OUTSIDE the repository; corpus files are never committed.
set -euo pipefail

outdir="${1:?usage: fetch_rtl.sh OUTDIR [HINDAWI_COUNT]}"
hindawi_count="${2:-10}"
ua="pdfboss-bench/0.1 (+https://github.com/4thel00z/pdfboss)"
mkdir -p "$outdir"

keep_if_pdf() {
  head -c 1024 "$1" | grep -aq '%PDF-' && return 0
  echo "    not a PDF, discarded: $(basename "$1")"
  rm -f "$1"
  return 1
}

echo "[hindawi] up to $hindawi_count CC BY 4.0 Arabic books via Commons API"
urls=$(curl -s -A "$ua" \
  "https://commons.wikimedia.org/w/api.php?action=query&list=search&srsearch=%D9%87%D9%86%D8%AF%D8%A7%D9%88%D9%8A%20filetype:pdf&srnamespace=6&srlimit=100&format=json" \
  | python3 -c "
import json, sys, urllib.parse
hits = json.load(sys.stdin)['query']['search']
for r in hits:
    print('https://commons.wikimedia.org/wiki/Special:FilePath/' + urllib.parse.quote(r['title'][5:]))
")
n=0
i=0
for u in $urls; do
  [ "$n" -ge "$hindawi_count" ] && break
  i=$((i + 1))
  name="hindawi_$(printf '%03d' "$i").pdf"
  [ -f "$outdir/$name" ] && { n=$((n + 1)); continue; }
  curl -sL -A "$ua" "$u" -o "$outdir/$name"
  sleep 2
  keep_if_pdf "$outdir/$name" || continue
  n=$((n + 1))
done
echo "[hindawi] $n files"

if [ "${UN_ODS:-0}" = "1" ]; then
  echo "[un-ods] Arabic resolutions, capped at 20, 1 req/s (fetch-only license)"
  n=0
  for sess in 76 77; do
    for res in $(seq 1 10); do
      [ "$n" -ge 20 ] && break 2
      name="un_A_RES_${sess}_${res}_ar.pdf"
      [ -f "$outdir/$name" ] && continue
      curl -sL -A "$ua" \
        "https://documents.un.org/api/symbol/access?s=A/RES/$sess/$res&l=A&t=pdf" \
        -o "$outdir/$name"
      sleep 1
      keep_if_pdf "$outdir/$name" || continue
      n=$((n + 1))
    done
  done
  echo "[un-ods] $n files"
fi

echo "[done] $(ls "$outdir"/*.pdf 2>/dev/null | wc -l | tr -d ' ') PDFs in $outdir"
