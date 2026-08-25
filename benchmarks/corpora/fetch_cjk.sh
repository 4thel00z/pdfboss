#!/usr/bin/env bash
# Fetch a CJK benchmark corpus for benchmarks/bench_diversity.py.
#
# Sources and licensing (see the per-source notes below):
#   1. J-STAGE open-access articles, 2004-2006 — the predefined-CMap era of
#      Japanese typesetting (90ms-RKSJ-H/V with non-embedded MSMincho was
#      verified on a 2005 article). FETCH-ONLY: licensing varies per journal;
#      many articles are free to read but NOT CC-licensed. Never redistribute
#      these files or commit them anywhere.
#   2. Japanese government (MIC/soumu) white-paper PDFs — CC BY 4.0 via the
#      Government of Japan Standard Terms of Use 2.0, redistribution-clean.
#      These embed their fonts (Identity-UCS), so they are the stable-text
#      regression half, not the CMap-gap half.
#   3. (opt-in, UN_ODS=1) UN Official Document System, Chinese variants.
#      FETCH-ONLY: un.org copyright says "All rights reserved". Rate-limited
#      to 1 request/second, capped at 20 documents.
#
# Usage: fetch_cjk.sh OUTDIR [JSTAGE_COUNT]
# OUTDIR must live OUTSIDE the repository; corpus files are never committed.
set -euo pipefail

outdir="${1:?usage: fetch_cjk.sh OUTDIR [JSTAGE_COUNT]}"
jstage_count="${2:-15}"
ua="pdfboss-bench/0.1 (+https://github.com/4thel00z/pdfboss)"
mkdir -p "$outdir"

keep_if_pdf() {
  head -c 1024 "$1" | grep -aq '%PDF-' && return 0
  echo "    not a PDF, discarded: $(basename "$1")"
  rm -f "$1"
  return 1
}

echo "[jstage] up to $jstage_count articles, 2004-2006 (predefined-CMap era)"
# The verified anchor: 2005, 90ms-RKSJ-H/V, non-embedded MSMincho.
curl -sL -A "$ua" "https://www.jstage.jst.go.jp/article/jjsk/41/0/41_46/_pdf" \
  -o "$outdir/jstage_anchor_jjsk_41_46.pdf"
keep_if_pdf "$outdir/jstage_anchor_jjsk_41_46.pdf" || true
sleep 2

# Enumerate more of the era via the official search API (Atom XML). The
# search term (解析, "analysis") is arbitrary; vary it for a different slice.
fetched=1
for term in "%E8%A7%A3%E6%9E%90" "%E6%83%85%E5%A0%B1"; do
  [ "$fetched" -ge "$jstage_count" ] && break
  urls=$(curl -s -A "$ua" \
    "https://api.jstage.jst.go.jp/searchapi/do?service=3&article=$term&pubyearfrom=2004&pubyearto=2006&count=50" \
    | grep -oE 'https://www.jstage.jst.go.jp/article/[^"<]+/_article[^"<]*' \
    | sed 's#/_article.*#/_pdf#' | sort -u)
  for u in $urls; do
    [ "$fetched" -ge "$jstage_count" ] && break
    name="jstage_$(echo "$u" | md5 2>/dev/null || echo "$u" | md5sum | cut -d' ' -f1).pdf"
    [ -f "$outdir/$name" ] && continue
    curl -sL -A "$ua" "$u" -o "$outdir/$name"
    sleep 2
    keep_if_pdf "$outdir/$name" || continue
    fetched=$((fetched + 1))
  done
done
echo "[jstage] $fetched files"

echo "[soumu] CC BY 4.0 white-paper PDFs (embedded fonts; regression half)"
for u in \
  "https://www.soumu.go.jp/main_content/001019264.pdf" \
  "https://www.soumu.go.jp/johotsusintokei/whitepaper/ja/r07/pdf/r07riyou.pdf"; do
  name="soumu_$(basename "$u")"
  [ -f "$outdir/$name" ] && continue
  curl -sL -A "$ua" "$u" -o "$outdir/$name"
  sleep 2
  keep_if_pdf "$outdir/$name" || true
done

if [ "${UN_ODS:-0}" = "1" ]; then
  echo "[un-ods] Chinese variants, capped at 20, 1 req/s (fetch-only license)"
  n=0
  for sess in 76 77; do
    for res in $(seq 1 10); do
      [ "$n" -ge 20 ] && break 2
      name="un_A_RES_${sess}_${res}_zh.pdf"
      [ -f "$outdir/$name" ] && continue
      curl -sL -A "$ua" \
        "https://documents.un.org/api/symbol/access?s=A/RES/$sess/$res&l=C&t=pdf" \
        -o "$outdir/$name"
      sleep 1
      keep_if_pdf "$outdir/$name" || continue
      n=$((n + 1))
    done
  done
  echo "[un-ods] $n files"
fi

echo "[done] $(ls "$outdir"/*.pdf 2>/dev/null | wc -l | tr -d ' ') PDFs in $outdir"
