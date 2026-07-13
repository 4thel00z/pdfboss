#!/usr/bin/env python3
"""Benchmark pdfboss against other Python PDF libraries.

Times two operations that every library produces comparable output for —
opening/parsing a document and extracting all its text — over a deterministic
sample of a PDF corpus, then writes ``results.json`` and ``results.png``.

Rendering is deliberately excluded: pdfboss's rasterizer does not yet paint
every glyph, so timing its (incomplete) output against full renderers would be
misleading.

Usage:
    python benchmarks/bench.py /path/to/pdfs [--sample N] [--repeat K]
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import time
import traceback


# --- library adapters: each returns a page count (open) or the text (text) ---


def pdfboss_open(path):
    import pdfboss

    return pdfboss.Document(path).page_count


def pdfboss_text(path):
    import pdfboss

    return pdfboss.Document(path).extract_text()


def pypdf_open(path):
    from pypdf import PdfReader

    return len(PdfReader(path).pages)


def pypdf_text(path):
    from pypdf import PdfReader

    return "".join(p.extract_text() for p in PdfReader(path).pages)


def pdfminer_text(path):
    from pdfminer.high_level import extract_text

    return extract_text(path)


def pdfplumber_open(path):
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        return len(pdf.pages)


def pdfplumber_text(path):
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        return "".join((pg.extract_text() or "") for pg in pdf.pages)


def pikepdf_open(path):
    import pikepdf

    with pikepdf.open(path) as pdf:
        return len(pdf.pages)


def pymupdf_open(path):
    import fitz

    doc = fitz.open(path)
    try:
        return doc.page_count
    finally:
        doc.close()


def pymupdf_text(path):
    import fitz

    doc = fitz.open(path)
    try:
        return "".join(pg.get_text() for pg in doc)
    finally:
        doc.close()


# Library display name -> {operation: adapter}. Order controls plot order.
LIBS = {
    "pdfboss": {"open": pdfboss_open, "text": pdfboss_text},
    "PyMuPDF": {"open": pymupdf_open, "text": pymupdf_text},
    "pypdf": {"open": pypdf_open, "text": pypdf_text},
    "pdfplumber": {"open": pdfplumber_open, "text": pdfplumber_text},
    "pdfminer.six": {"text": pdfminer_text},
    "pikepdf": {"open": pikepdf_open},
}


def sample_files(corpus, n):
    files = sorted(glob.glob(os.path.join(corpus, "*.pdf")))
    if not files:
        raise SystemExit(f"no PDFs found in {corpus}")
    if n >= len(files):
        return files
    # Evenly spaced across the sorted corpus for a representative spread.
    step = len(files) / n
    return [files[int(i * step)] for i in range(n)]


def time_one(fn, path, repeat):
    """Best-of-`repeat` wall time for `fn(path)`, or None if it raised."""
    best = None
    for _ in range(repeat):
        t0 = time.perf_counter()
        try:
            fn(path)
        except Exception:
            return None
        dt = time.perf_counter() - t0
        if best is None or dt < best:
            best = dt
    return best


def run(corpus, sample_n, repeat):
    files = sample_files(corpus, sample_n)
    # Canonical page count per file (pdfboss), for a pages/sec metric.
    pages = {}
    for f in files:
        try:
            pages[f] = pdfboss_open(f)
        except Exception:
            pages[f] = 0

    # Record only the corpus directory name, never the full local path.
    results = {"corpus": os.path.basename(corpus.rstrip("/")), "files": len(files), "operations": {}}
    for op in ("open", "text"):
        libs = {name: spec[op] for name, spec in LIBS.items() if op in spec}
        # Warm the OS file cache and per-library import once.
        for fn in libs.values():
            for f in files:
                try:
                    fn(f)
                except Exception:
                    pass
        per_lib = {name: {"time": 0.0, "pages": 0, "ok": 0} for name in libs}
        # Time each file, then keep only files EVERY library handled, so the
        # aggregate compares the same workload.
        timings = {name: {} for name in libs}
        for f in files:
            for name, fn in libs.items():
                t = time_one(fn, f, repeat)
                if t is not None:
                    timings[name][f] = t
        common = set(files)
        for name in libs:
            common &= set(timings[name])
        for name in libs:
            for f in common:
                per_lib[name]["time"] += timings[name][f]
                per_lib[name]["pages"] += pages[f]
                per_lib[name]["ok"] += 1
        results["operations"][op] = {
            "files_compared": len(common),
            "libraries": per_lib,
        }
        print(f"[{op}] compared {len(common)} files across {len(libs)} libraries")
        for name, r in sorted(per_lib.items(), key=lambda kv: kv[1]["time"] or 1e9):
            pps = r["pages"] / r["time"] if r["time"] else 0
            print(f"    {name:14} {r['time']:8.3f}s   {pps:9.1f} pages/s")
    return results


def plot(results, out_png):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, axes = plt.subplots(1, 2, figsize=(11, 4.2))
    titles = {"open": "Open + parse", "text": "Text extraction"}
    for ax, op in zip(axes, ("open", "text")):
        data = results["operations"][op]["libraries"]
        rows = [
            (name, r["pages"] / r["time"] if r["time"] else 0.0)
            for name, r in data.items()
        ]
        rows.sort(key=lambda kv: kv[1])
        names = [n for n, _ in rows]
        vals = [v for _, v in rows]
        colors = ["#e8552d" if n == "pdfboss" else "#9aa4b2" for n in names]
        bars = ax.barh(names, vals, color=colors)
        ax.set_title(f"{titles[op]}  (pages/sec, higher is faster)", fontsize=11)
        ax.bar_label(bars, fmt="%.0f", padding=3, fontsize=9)
        ax.margins(x=0.15)
        ax.spines[["top", "right"]].set_visible(False)
        ax.tick_params(length=0)
    n = results["operations"]["text"]["files_compared"]
    fig.suptitle(
        f"pdfboss vs. Python PDF libraries — {n} real-world PDFs",
        fontsize=13,
        fontweight="bold",
    )
    fig.tight_layout(rect=(0, 0, 1, 0.95))
    fig.savefig(out_png, dpi=140)
    print(f"wrote {out_png}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus", help="directory of .pdf files")
    ap.add_argument("--sample", type=int, default=40, help="files to sample")
    ap.add_argument("--repeat", type=int, default=3, help="best-of-N per file")
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(args.corpus, args.sample, args.repeat)
    with open(os.path.join(here, "results.json"), "w") as f:
        json.dump(results, f, indent=2)
    try:
        plot(results, os.path.join(here, "results.png"))
    except Exception:
        traceback.print_exc()


if __name__ == "__main__":
    main()
