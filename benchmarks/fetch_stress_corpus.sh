#!/usr/bin/env bash
# Download the OSS-Fuzz public PDF fuzzer corpora for bench_robustness.py.
#
# These are fuzzer-minimized malformed inputs (~230 MB per zip, tens of
# thousands of hash-named seed files). Fetch them into a directory OUTSIDE
# the repo; they are fine to use locally and must never be committed or
# redistributed from here.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <corpus-dir (outside the repo)>" >&2
  exit 1
fi

dest=$1
mkdir -p "$dest"

fetch() {
  local name=$1 url=$2
  local zip="$dest/$name.zip"
  if [ ! -f "$zip" ]; then
    echo "downloading $name (~230 MB) ..."
    curl -SL --fail -o "$zip" "$url"
  fi
  if [ ! -d "$dest/$name" ] || [ -z "$(ls -A "$dest/$name")" ]; then
    echo "unzipping $name ..."
    mkdir -p "$dest/$name"
    unzip -q "$zip" -d "$dest/$name"
  fi
  echo "$name: $(find "$dest/$name" -type f | wc -l | tr -d ' ') files"
}

fetch mupdf   https://storage.googleapis.com/mupdf-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/mupdf_pdf_fuzzer/public.zip
fetch poppler https://storage.googleapis.com/poppler-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/poppler_pdf_fuzzer/public.zip

echo "corpus ready: python benchmarks/bench_robustness.py '$dest'"
