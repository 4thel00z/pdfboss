#!/usr/bin/env python3
"""Benchmark pdfboss against other Python PDF libraries on scanned documents.

A scanned page is a single full-page bilevel image — JBIG2 or CCITT G3/G4
coded — with no text operators at all. That makes rendering comparable here in
a way it is not for the mixed corpus in ``bench.py``: there are no glyphs to
paint, so every library that rasterizes the page produces the same picture and
the time is dominated by the bilevel decoder.

Each library renders the same pages of the same file to PNG bytes, so the
comparison includes PNG encoding on every side.

Usage:
    python benchmarks/bench_scans.py scan.pdf [--pages N] [--repeat K] [--scale S]
"""

from __future__ import annotations

import argparse
import json
import os
import time
import traceback


# --- library adapters -------------------------------------------------------
#
# Each renders `indices` of `path` at `scale` and returns the PNG bytes of
# every page, which keeps a lazy library from skipping the work and lets the
# caller check that what came back is a picture of the scan.


def pdfboss_render(path, indices, scale):
    import pdfboss

    doc = pdfboss.Document(path)
    return [doc[i].render(scale=scale) for i in indices]


def pymupdf_render(path, indices, scale):
    import fitz

    doc = fitz.open(path)
    try:
        matrix = fitz.Matrix(scale, scale)
        return [doc[i].get_pixmap(matrix=matrix).tobytes("png") for i in indices]
    finally:
        doc.close()


def pypdfium2_render(path, indices, scale):
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


def pdfplumber_render(path, indices, scale):
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


def probe(path, indices, scale):
    """Which libraries render this file at all, and why the others do not."""
    supported, refused, coverage = {}, {}, {}
    for name, fn in LIBS.items():
        try:
            pages = fn(path, indices[:1], scale)
        except ImportError:
            refused[name] = "not installed"
        except Exception as exc:  # noqa: BLE001 - the message is the result
            refused[name] = f"{type(exc).__name__}: {exc}".strip()
        else:
            supported[name] = fn
            coverage[name] = ink(pages[0])
    return supported, refused, coverage


def time_one(fn, path, indices, scale, repeat):
    """Best-of-`repeat` wall time, or None if the library raised."""
    best = None
    for _ in range(repeat):
        start = time.perf_counter()
        try:
            fn(path, indices, scale)
        except Exception:
            return None
        elapsed = time.perf_counter() - start
        if best is None or elapsed < best:
            best = elapsed
    return best


def run(path, limit, repeat, scale):
    indices, total_pages = page_indices(path, limit)
    supported, refused, coverage = probe(path, indices, scale)
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
            fn(path, indices, scale)
        except Exception:
            pass

    libraries = {}
    for name, fn in supported.items():
        elapsed = time_one(fn, path, indices, scale, repeat)
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
        "repeat": repeat,
        "libraries": libraries,
        "refused": refused,
    }
    print(f"[render] {len(indices)} of {total_pages} pages at scale {scale}")
    for name, r in sorted(libraries.items(), key=lambda kv: kv[1]["time"]):
        pps = r["pages"] / r["time"]
        print(f"    {name:14} {r['time']:8.3f}s   {pps:9.1f} pages/s")
    return results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scan", help="a scanned .pdf — one bilevel image per page")
    ap.add_argument("--pages", type=int, default=50, help="pages to render (0 = all)")
    ap.add_argument("--repeat", type=int, default=3, help="best-of-N")
    ap.add_argument("--scale", type=float, default=1.0, help="render scale factor")
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(args.scan, args.pages, args.repeat, args.scale)
    out = os.path.join(here, "results-scans.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
