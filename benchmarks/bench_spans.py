#!/usr/bin/env python3
"""Sweep styled-span extraction over a PDF corpus and hold it to its contract.

The question is not speed but the leniency promise: iterating a loadable
document's spans must never raise — unreadable content yields no spans, not
an exception. The sweep walks every PDF under the corpus directory, counts
what the style channels report, and fails loudly on any document whose span
iteration raises.

Files that fail at the Document constructor (encrypted files, intentionally
broken fixtures) are counted per suite and reported, never treated as span
failures — that path is load, not extraction.

Per top-level suite directory the report carries: documents swept, load
failures, spans seen, and the rates of underline, strikethrough, invisible
(render modes 3/7) and pattern-colored (color is None) spans. Rates that
jump between runs are the regression signal for the decoration heuristics.

Usage:
    python benchmarks/bench_spans.py /path/to/corpus [--sample N]

The corpus comes from benchmarks/corpora/fetch_pdf_oxide.sh (the veraPDF +
pdf.js + safedocs suites pdf_oxide benchmarks against), downloaded OUTSIDE
the repo and never committed. Any directory of PDFs works; suites are its
first-level subdirectories, with loose files reported under ".".
"""

from __future__ import annotations

import argparse
import glob
import os
import sys
import time
import traceback

import pdfboss


def sample_files(files: list[str], n: int) -> list[str]:
    if n >= len(files):
        return files
    # Evenly spaced across the sorted corpus for a representative spread.
    step = len(files) / n
    return [files[int(i * step)] for i in range(n)]


def suite_of(corpus: str, path: str) -> str:
    rel = os.path.relpath(path, corpus)
    head, _, tail = rel.partition(os.sep)
    return head if tail else "."


def run(corpus: str, sample_n: int | None) -> int:
    files = sorted(glob.glob(os.path.join(corpus, "**", "*.pdf"), recursive=True))
    if not files:
        raise SystemExit(f"no PDFs found in {corpus}")
    if sample_n:
        files = sample_files(files, sample_n)

    suites: dict[str, dict[str, int]] = {}
    span_failures: list[str] = []
    started = time.time()
    for path in files:
        s = suites.setdefault(
            suite_of(corpus, path),
            dict(docs=0, loadfail=0, spans=0, under=0, strike=0, invis=0, nocolor=0),
        )
        try:
            doc = pdfboss.Document(path)
        except Exception:
            s["loadfail"] += 1
            continue
        try:
            for span in doc.spans():
                s["spans"] += 1
                s["under"] += span.underline
                s["strike"] += span.strikethrough
                s["invis"] += span.invisible
                s["nocolor"] += span.color is None
            s["docs"] += 1
        except Exception:
            span_failures.append(path)
            traceback.print_exc()
    elapsed = time.time() - started

    print(f"{len(files)} files in {elapsed:.1f}s")
    for name, s in sorted(suites.items()):
        spans = s["spans"] or 1
        print(
            f"  {name:12} docs={s['docs']:5} loadfail={s['loadfail']:3} "
            f"spans={s['spans']:8} underline={s['under'] / spans:6.2%} "
            f"strike={s['strike'] / spans:6.2%} invisible={s['invis'] / spans:6.2%} "
            f"pattern-color={s['nocolor'] / spans:6.2%}"
        )
    if span_failures:
        print(f"FAIL: span iteration raised on {len(span_failures)} documents:")
        for path in span_failures:
            print(f"  {path}")
        return 1
    print("span iteration raised on 0 documents")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("corpus", help="directory of PDFs (suites as subdirectories)")
    parser.add_argument("--sample", type=int, default=None, help="sweep only N files")
    args = parser.parse_args()
    sys.exit(run(args.corpus, args.sample))


if __name__ == "__main__":
    main()
