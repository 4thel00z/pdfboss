#!/usr/bin/env python3
"""Benchmark pdfboss against other Python PDF libraries on scanned documents.

A scanned page is a single full-page bilevel image — JBIG2 or CCITT G3/G4
coded — with no text operators at all. That makes rendering comparable here in
a way it is not for the mixed corpus in ``bench.py``: there are no glyphs to
paint, so every library that rasterizes the page produces the same picture and
the time is dominated by the bilevel decoder.

Each library renders the same pages of the same file to PNG bytes, so the
comparison includes PNG encoding on every side.

`--suite` widens the comparison from one document to a set of them and from
one scale to a sweep: every (file, scale) cell is timed separately, each with
its own ink gate. Scale is an axis worth sweeping since 0.17.1: rendering a
high-resolution bilevel scan at scale 1.0 minifies it several-fold (averaging
the source footprint per device pixel), while scale 2.0 barely minifies at
all — different code paths, honest at both.

Usage:
    python benchmarks/bench_scans.py scan.pdf [--pages N] [--repeat K] [--scale S]
    python benchmarks/bench_scans.py --suite DIR_OR_LIST [--scales 1.0,1.5,2.0]
                                     [--pages N] [--repeat K]
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import time
import traceback


# --- library adapters -------------------------------------------------------
#
# Each renders `indices` of `path` at `scale` and returns the PNG bytes of
# every page, which keeps a lazy library from skipping the work and lets the
# caller check that what came back is a picture of the scan. `fonts` is
# pdfboss's glyph-painting tier — irrelevant on a pure scan (no text
# operators), needed when a suite file carries a text cover page; the other
# libraries have no such knob and ignore it.


def pdfboss_render(path, indices, scale, fonts):
    import pdfboss

    doc = pdfboss.Document(path)
    return [doc[i].render(scale=scale, fonts=fonts) for i in indices]


def pymupdf_render(path, indices, scale, fonts):
    import fitz

    doc = fitz.open(path)
    try:
        matrix = fitz.Matrix(scale, scale)
        return [doc[i].get_pixmap(matrix=matrix).tobytes("png") for i in indices]
    finally:
        doc.close()


def pypdfium2_render(path, indices, scale, fonts):
    import io

    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        out = []
        for i in indices:
            buf = io.BytesIO()
            doc[i].render(scale=scale).to_pil().save(buf, format="PNG")
            out.append(buf.getvalue())
        return out
    finally:
        doc.close()


def pdfplumber_render(path, indices, scale, fonts):
    import io

    import pdfplumber

    # pdfplumber measures rasterization in DPI, not as a scale factor.
    resolution = 72.0 * scale
    with pdfplumber.open(path) as pdf:
        out = []
        for i in indices:
            page = pdf.pages[i]
            buf = io.BytesIO()
            page.to_image(resolution=resolution).original.save(buf, format="PNG")
            out.append(buf.getvalue())
            page.close()
        return out


# Library display name -> renderer. Order controls report order.
LIBS = {
    "pdfboss": pdfboss_render,
    "PyMuPDF": pymupdf_render,
    "pypdfium2": pypdfium2_render,
    "pdfplumber": pdfplumber_render,
}


def page_indices(path, limit):
    """Evenly spaced page indices across the document, at most `limit` of them."""
    import pdfboss

    count = pdfboss.Document(path).page_count
    if count == 0:
        raise SystemExit(f"{path} has no pages")
    if limit <= 0 or limit >= count:
        return list(range(count)), count
    step = count / limit
    return [int(i * step) for i in range(limit)], count


def ink(png):
    """Percentage of dark pixels in a rendered page, or None without PIL.

    A library that cannot decode the scan's bilevel codec often returns a
    blank page instead of raising, which would time beautifully and mean
    nothing. Comparing ink across libraries catches that.
    """
    try:
        import io

        from PIL import Image
    except ImportError:
        return None
    gray = Image.open(io.BytesIO(png)).convert("L").tobytes()
    return 100.0 * sum(1 for v in gray if v < 128) / len(gray)


def probe(path, indices, scale, fonts):
    """Which libraries render this file at all, and why the others do not."""
    supported, refused, coverage = {}, {}, {}
    for name, fn in LIBS.items():
        try:
            pages = fn(path, indices[:1], scale, fonts)
        except ImportError:
            refused[name] = "not installed"
        except Exception as exc:  # noqa: BLE001 - the message is the result
            refused[name] = f"{type(exc).__name__}: {exc}".strip()
        else:
            supported[name] = fn
            coverage[name] = ink(pages[0])
    return supported, refused, coverage


def time_one(fn, path, indices, scale, fonts, repeat):
    """Best-of-`repeat` wall time, or None if the library raised."""
    best = None
    for _ in range(repeat):
        start = time.perf_counter()
        try:
            fn(path, indices, scale, fonts)
        except Exception:
            return None
        elapsed = time.perf_counter() - start
        if best is None or elapsed < best:
            best = elapsed
    return best


def run(path, limit, repeat, scale, fonts):
    indices, total_pages = page_indices(path, limit)
    supported, refused, coverage = probe(path, indices, scale, fonts)
    for name, why in refused.items():
        print(f"    {name:14} cannot render this file — {why}")
    if not supported:
        raise SystemExit("no library rendered the file")
    print(f"[ink] page {indices[0] + 1}, dark pixels — the renders must agree")
    for name, pct in coverage.items():
        print(f"    {name:14} {'n/a' if pct is None else f'{pct:8.2f}%'}")

    # Warm the OS file cache and every import before the timed passes.
    for fn in supported.values():
        try:
            fn(path, indices, scale, fonts)
        except Exception:
            pass

    libraries = {}
    for name, fn in supported.items():
        elapsed = time_one(fn, path, indices, scale, fonts, repeat)
        if elapsed is None:
            refused[name] = "failed under timing"
            continue
        libraries[name] = {
            "time": elapsed,
            "pages": len(indices),
            "ink_percent": coverage.get(name),
        }

    # Record the file's shape, never its name: the corpus is not public.
    results = {
        "document_pages": total_pages,
        "pages_rendered": len(indices),
        "scale": scale,
        "fonts": fonts,
        "repeat": repeat,
        "libraries": libraries,
        "refused": refused,
    }
    print(f"[render] {len(indices)} of {total_pages} pages at scale {scale}")
    for name, r in sorted(libraries.items(), key=lambda kv: kv[1]["time"]):
        pps = r["pages"] / r["time"]
        print(f"    {name:14} {r['time']:8.3f}s   {pps:9.1f} pages/s")
    return results


# A cell excludes a library whose first-page ink coverage lands outside
# [median / INK_BAND, median * INK_BAND] of the libraries' median — wide
# enough for resampling differences, narrow enough to catch a page that
# painted only part of its content. Deviations inside INK_SLACK percentage
# points are ignored outright so near-blank pages do not trip the ratio on
# noise. Same constants as bench_render.py's file-level gate.
INK_BAND = 2.0
INK_SLACK = 0.15


def ink_gate(coverage: dict[str, float | None]) -> dict[str, str]:
    """Libraries whose ink coverage disagrees with the cross-library median."""
    values = sorted(pct for pct in coverage.values() if pct is not None)
    if len(values) < 2:
        return {}
    median = values[len(values) // 2]
    gated = {}
    for name, pct in coverage.items():
        if pct is None:
            continue
        if abs(pct - median) <= INK_SLACK:
            continue
        if median > 0 and median / INK_BAND <= pct <= median * INK_BAND:
            continue
        gated[name] = f"ink gate: {pct:.2f}% vs median {median:.2f}%"
    return gated


def suite_files(source: str) -> list[str]:
    """The suite's PDFs: every *.pdf in a directory, or the lines of a list file."""
    if os.path.isdir(source):
        files = sorted(glob.glob(os.path.join(source, "*.pdf")))
        if not files:
            raise SystemExit(f"no PDFs found in {source}")
        return files
    if not os.path.isfile(source):
        raise SystemExit(f"{source} is neither a directory nor a file list")
    with open(source) as f:
        lines = [line.strip() for line in f]
    files = [line for line in lines if line and not line.startswith("#")]
    if not files:
        raise SystemExit(f"{source} lists no files")
    return files


def run_suite(
    paths: list[str], limit: int, repeat: int, scales: list[float], fonts: str, out: str
) -> None:
    """Time every (file, scale) cell, each behind its own ink gate.

    Written to `out` after every cell, so a crash keeps what already
    finished. Cells are keyed by position in the suite, never by file name:
    the corpus is not public.
    """
    cells: dict[str, dict] = {}
    results = {
        "files": len(paths),
        "scales": scales,
        "repeat": repeat,
        "pages_limit": limit,
        "fonts": fonts,
        "cells": cells,
    }
    for pos, path in enumerate(paths):
        key = f"file_{pos + 1:02d}"
        try:
            indices, total_pages = page_indices(path, limit)
        except Exception as exc:  # noqa: BLE001 - the message is the result
            cells[key] = {"unreadable": f"{type(exc).__name__}: {exc}".strip()}
            print(f"[{key}] unreadable — {cells[key]['unreadable']}")
            continue
        entry: dict[str, dict] = {
            "document_pages": total_pages,
            "pages_rendered": len(indices),
            "scales": {},
        }
        cells[key] = entry
        for scale in scales:
            supported, refused, coverage = probe(path, indices, scale, fonts)
            for name, why in ink_gate(coverage).items():
                refused[name] = why
                supported.pop(name)
            libraries = {}
            for name, fn in supported.items():
                try:
                    fn(path, indices, scale, fonts)
                except Exception:
                    pass
                elapsed = time_one(fn, path, indices, scale, fonts, repeat)
                if elapsed is None:
                    refused[name] = "failed under timing"
                    continue
                libraries[name] = {
                    "time": elapsed,
                    "ink_percent": coverage.get(name),
                }
            entry["scales"][f"{scale:g}"] = {
                "libraries": libraries,
                "refused": refused,
            }
            with open(out, "w") as f:
                json.dump(results, f, indent=2)
            timed = ", ".join(
                f"{name} {len(indices) / r['time']:.1f} p/s"
                for name, r in sorted(libraries.items(), key=lambda kv: kv[1]["time"])
            )
            gates = f"; excluded: {', '.join(refused)}" if refused else ""
            print(f"[{key} x{scale:g}] {len(indices)} pages — {timed}{gates}")

    # Per-scale totals over the cells EVERY library passed, so the aggregate
    # compares the exact same workload.
    totals: dict[str, dict] = {}
    for scale in scales:
        scale_key = f"{scale:g}"
        common = [
            cell
            for cell in cells.values()
            if "scales" in cell
            and set(cell["scales"].get(scale_key, {}).get("libraries", {})) == set(LIBS)
        ]
        pages = sum(cell["pages_rendered"] for cell in common)
        totals[scale_key] = {
            "files_compared": len(common),
            "pages": pages,
            "libraries": {
                name: sum(
                    cell["scales"][scale_key]["libraries"][name]["time"]
                    for cell in common
                )
                for name in LIBS
            },
        }
    results["totals"] = totals
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    for scale_key, agg in totals.items():
        print(f"[total x{scale_key}] {agg['files_compared']} files, {agg['pages']} pages")
        for name, total in sorted(agg["libraries"].items(), key=lambda kv: kv[1]):
            pps = agg["pages"] / total if total else 0
            print(f"    {name:14} {total:8.3f}s   {pps:9.1f} pages/s")
    print(f"wrote {out}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("scan", nargs="?", help="a scanned .pdf — one bilevel image per page")
    ap.add_argument("--suite", help="directory or file list of scanned PDFs to sweep")
    ap.add_argument("--pages", type=int, default=50, help="pages to render (0 = all)")
    ap.add_argument("--repeat", type=int, default=3, help="best-of-N")
    ap.add_argument("--scale", type=float, default=1.0, help="render scale factor")
    ap.add_argument(
        "--scales",
        default="1.0,1.5,2.0",
        help="comma-separated scale sweep (suite mode)",
    )
    ap.add_argument(
        "--fonts",
        choices=("embedded-only", "all-embedded", "full"),
        help="pdfboss glyph-painting tier — irrelevant on a pure scan; defaults"
        " to all-embedded for one file (the historical behavior) and full for"
        " --suite, where a scan may carry a text cover page",
    )
    args = ap.parse_args()

    if bool(args.scan) == bool(args.suite):
        raise SystemExit("give either one scan.pdf or --suite, not both")
    fonts = args.fonts or ("full" if args.suite else "all-embedded")

    here = os.path.dirname(os.path.abspath(__file__))
    if args.suite:
        scales = [float(s) for s in args.scales.split(",") if s.strip()]
        if not scales:
            raise SystemExit("--scales names no scales")
        out = os.path.join(here, "results-scans-suite.json")
        run_suite(suite_files(args.suite), args.pages, args.repeat, scales, fonts, out)
        return
    results = run(args.scan, args.pages, args.repeat, args.scale, fonts)
    out = os.path.join(here, "results-scans.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
