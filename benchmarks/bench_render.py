#!/usr/bin/env python3
"""Benchmark pdfboss against other Python PDF libraries on page rendering.

Timing a renderer is only fair when every side paints the same picture, and
pdfboss does not paint everything yet (see the README's Limitations). So this
script certifies each file before the stopwatch starts:

- pdfboss renders every page through ``render_reporting`` at the ``full``
  fonts tier — the tier that substitutes non-embedded simple fonts, which is
  what the other engines do by default. A file where any page reports dropped
  or approximated content (an unpainted shading, a masked image, an
  annotation appearance, a glyph a loaded font lacks) is excluded, and the
  exclusions are printed and counted. Nothing is dropped silently.
- Content a *failed or refused* font would have painted is configured
  behavior and not reported, so a second gate catches it: every library
  renders the first page, and a file where the libraries' ink coverage
  disagrees is excluded too. A blank page renders instantly and means
  nothing.

What survives is the corpus subset every engine rasterizes completely, and
only that subset is timed: every page to PNG bytes, so PNG encoding is on
every side of the comparison.

Usage:
    python benchmarks/bench_render.py /path/to/pdfs [--sample N] [--repeat K]
                                      [--scale S] [--fonts TIER]
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
# every page. `fonts` is pdfboss's glyph-painting tier; the other libraries
# have no such knob and ignore it.


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

# A file is excluded when any library's first-page ink coverage lands outside
# [median / INK_BAND, median * INK_BAND] of the libraries' median — wide
# enough for anti-aliasing and resampling differences, narrow enough to catch
# a page that painted only part of its content. Deviations inside
# INK_SLACK percentage points are ignored outright so near-blank pages do not
# trip the ratio on noise.
INK_BAND = 2.0
INK_SLACK = 0.15


def sample_files(corpus, n):
    if n <= 0:
        raise SystemExit("--sample must be a positive number of files")
    files = sorted(glob.glob(os.path.join(corpus, "*.pdf")))
    if not files:
        raise SystemExit(f"no PDFs found in {corpus}")
    if n >= len(files):
        return files
    # Evenly spaced across the sorted corpus for a representative spread.
    step = len(files) / n
    return [files[int(i * step)] for i in range(n)]


def certify(path, scale, fonts):
    """The file's renderable page indices, or the reason it cannot be timed.

    Returns ``(indices, None)`` when pdfboss rasterized every page with an
    empty report, and ``(None, reason)`` otherwise. The reason keeps the
    report's own wording, so the exclusion tally names what pdfboss cannot
    paint rather than hiding it.
    """
    import pdfboss

    try:
        doc = pdfboss.Document(path)
    except Exception as exc:  # noqa: BLE001 - the message is the result
        return None, f"unreadable: {type(exc).__name__}: {exc}"
    indices = []
    for i in range(doc.page_count):
        try:
            page = doc[i]
        except Exception:
            # A damaged file can declare more pages than it holds; the pages
            # that do exist were all checked, so certify those.
            break
        try:
            warnings = page.render_reporting(scale=scale, fonts=fonts)[1]
        except Exception as exc:  # noqa: BLE001 - the message is the result
            return None, f"render failed: {type(exc).__name__}: {exc}"
        if warnings:
            return None, warnings[0]
        indices.append(i)
    if not indices:
        return None, "no pages"
    return indices, None


def ink(png):
    """Percentage of dark pixels in a rendered page, or None without PIL."""
    try:
        import io

        from PIL import Image
    except ImportError:
        return None
    gray = Image.open(io.BytesIO(png)).convert("L").tobytes()
    return 100.0 * sum(1 for v in gray if v < 128) / len(gray)


def ink_agreement(path, scale, fonts):
    """Whether every library's first-page render holds the same ink, roughly.

    Returns ``None`` on agreement, else a one-line reason naming the odd one
    out. A library that raises here is left for the timing pass to exclude
    via the common-file intersection; this gate only compares the renders
    that exist.
    """
    coverage = {}
    for name, fn in LIBS.items():
        try:
            pages = fn(path, [0], scale, fonts)
        except Exception:
            continue
        pct = ink(pages[0])
        if pct is not None:
            coverage[name] = pct
    if len(coverage) < 2:
        return None
    values = sorted(coverage.values())
    median = values[len(values) // 2]
    for name, pct in coverage.items():
        if abs(pct - median) <= INK_SLACK:
            continue
        if median > 0 and median / INK_BAND <= pct <= median * INK_BAND:
            continue
        return (
            f"renders disagree: {name} ink {pct:.2f}% vs median {median:.2f}%"
        )
    return None


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


def run(corpus, sample_n, repeat, scale, fonts):
    files = sample_files(corpus, sample_n)

    # Certification: only files every engine rasterizes completely are timed.
    certified = {}
    excluded = {}
    for f in files:
        indices, reason = certify(f, scale, fonts)
        if reason is None:
            reason = ink_agreement(f, scale, fonts)
        if reason is not None:
            excluded[f] = reason
            continue
        certified[f] = indices
    print(
        f"[certify] {len(certified)} of {len(files)} sampled files rasterize"
        f" completely at fonts={fonts}; {len(excluded)} excluded:"
    )
    for f, reason in excluded.items():
        print(f"    {os.path.basename(f):20} {reason}")
    if not certified:
        raise SystemExit("no file passed certification; nothing to time")

    # Warm the OS file cache and every import before the timed passes.
    for fn in LIBS.values():
        for f, indices in certified.items():
            try:
                fn(f, indices, scale, fonts)
            except Exception:
                pass

    # Time each file, then keep only files EVERY library handled, so the
    # aggregate compares the same workload.
    timings = {name: {} for name in LIBS}
    for f, indices in certified.items():
        for name, fn in LIBS.items():
            t = time_one(fn, f, indices, scale, fonts, repeat)
            if t is not None:
                timings[name][f] = t
    common = set(certified)
    for name in LIBS:
        common &= set(timings[name])

    libraries = {}
    for name in LIBS:
        total = sum(timings[name][f] for f in common)
        pages = sum(len(certified[f]) for f in common)
        libraries[name] = {"time": total, "pages": pages, "ok": len(common)}

    # Tally exclusions by their leading words only ("2 shadings skipped:
    # ..." -> "shading skipped", depluralized so counts merge) and record no
    # file names: the corpus is not public.
    reasons = {}
    for reason in excluded.values():
        key = reason.split(":")[0].lstrip("0123456789 ")
        if key.endswith("s skipped"):
            key = key.removesuffix("s skipped") + " skipped"
        reasons[key] = reasons.get(key, 0) + 1
    results = {
        "corpus": os.path.basename(corpus.rstrip("/")),
        "files_sampled": len(files),
        "files_certified": len(certified),
        "files_compared": len(common),
        "excluded": reasons,
        "scale": scale,
        "fonts": fonts,
        "repeat": repeat,
        "libraries": libraries,
    }
    print(f"[render] {len(common)} files at scale {scale}")
    for name, r in sorted(libraries.items(), key=lambda kv: kv[1]["time"] or 1e9):
        pps = r["pages"] / r["time"] if r["time"] else 0
        print(f"    {name:14} {r['time']:8.3f}s   {pps:9.1f} pages/s")
    return results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus", help="directory of .pdf files")
    ap.add_argument("--sample", type=int, default=40, help="files to sample")
    ap.add_argument("--repeat", type=int, default=3, help="best-of-N per file")
    ap.add_argument("--scale", type=float, default=1.0, help="render scale factor")
    ap.add_argument(
        "--fonts",
        default="full",
        choices=("embedded-only", "all-embedded", "full"),
        help="pdfboss glyph-painting tier (full = substitute like the others)",
    )
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(args.corpus, args.sample, args.repeat, args.scale, args.fonts)
    out = os.path.join(here, "results-render.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
