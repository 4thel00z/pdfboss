#!/usr/bin/env python3
"""Measure pdfboss quality on non-Latin and academic corpora.

Every published pdfboss number so far comes from Latin-script corpora, so
CJK, RTL and academic behavior is unmeasured. This script is the regression
harness for that gap: per corpus directory it scores each engine on

- **open rate** — the file opens and reports a page count;
- **text non-crash rate** — text extraction over the first ``--max-pages``
  pages raises nothing;
- **U+FFFD replacement-character rate** — replacement characters per
  extracted character, the honest proxy for encoding gaps. A doc that
  extracts *zero* characters is worse than one full of U+FFFD, so zero-text
  docs are counted separately and never score a flattering 0.0;
- **markdown non-crash rate** — pdfboss only; the other engines have no
  comparable API here;
- **render page-1 non-blank rate** — the first page rasterizes with more
  than 0.1% dark pixels.

pdfboss currently lacks predefined-CMap support, so Japanese documents of
the 90ms-RKSJ era (the J-STAGE 2004-2006 slice) are EXPECTED to score
poorly on text metrics. This bench exists to measure that gap and to catch
the improvement when CMap support lands.

Everything here is a quality metric, not a timing — results are
load-insensitive and publishable as measured. The JSON records counts and
rates only, never file names; per-file character and U+FFFD counts go to
stdout.

Usage:
    python benchmarks/bench_diversity.py DIR [DIR ...] [--max-pages N]
                                         [--scale S] [--fonts TIER]
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import os
import statistics
import traceback
from typing import Callable


# --- engine adapters ---------------------------------------------------------
#
# Each engine supplies `open` (page count), `text` (concatenated text of the
# first max_pages pages), `render` (PNG bytes of page 1) and optionally
# `markdown`. `fonts` is pdfboss's glyph-painting tier; the other engines
# have no such knob and ignore it. Imports stay inside the functions so a
# missing engine is skipped, never fatal.


def pdfboss_open(path: str) -> int:
    import pdfboss

    return pdfboss.Document(path).page_count


def pdfboss_text(path: str, max_pages: int) -> str:
    import pdfboss

    doc = pdfboss.Document(path)
    pages = min(doc.page_count, max_pages)
    return "".join(doc[i].extract_text() for i in range(pages))


def pdfboss_markdown(path: str, max_pages: int) -> str:
    import pdfboss

    doc = pdfboss.Document(path)
    pages = min(doc.page_count, max_pages)
    return "".join(doc[i].extract_markdown() for i in range(pages))


def pdfboss_render(path: str, scale: float, fonts: str) -> bytes:
    import pdfboss

    return pdfboss.Document(path)[0].render(scale=scale, fonts=fonts)


def pymupdf_open(path: str) -> int:
    import fitz

    with fitz.open(path) as doc:
        return doc.page_count


def pymupdf_text(path: str, max_pages: int) -> str:
    import fitz

    with fitz.open(path) as doc:
        pages = min(doc.page_count, max_pages)
        return "".join(doc[i].get_text() for i in range(pages))


def pymupdf_render(path: str, scale: float, fonts: str) -> bytes:
    import fitz

    with fitz.open(path) as doc:
        matrix = fitz.Matrix(scale, scale)
        return doc[0].get_pixmap(matrix=matrix).tobytes("png")


def pypdfium2_open(path: str) -> int:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        return len(doc)
    finally:
        doc.close()


def pypdfium2_text(path: str, max_pages: int) -> str:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        pages = min(len(doc), max_pages)
        return "".join(doc[i].get_textpage().get_text_range() for i in range(pages))
    finally:
        doc.close()


def pypdfium2_render(path: str, scale: float, fonts: str) -> bytes:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        buf = io.BytesIO()
        doc[0].render(scale=scale).to_pil().save(buf, format="PNG")
        return buf.getvalue()
    finally:
        doc.close()


def pdfplumber_open(path: str) -> int:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        return len(pdf.pages)


def pdfplumber_text(path: str, max_pages: int) -> str:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        pages = pdf.pages[:max_pages]
        return "".join(p.extract_text() or "" for p in pages)


def pdfplumber_render(path: str, scale: float, fonts: str) -> bytes:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        buf = io.BytesIO()
        pdf.pages[0].to_image(resolution=72.0 * scale).original.save(buf, format="PNG")
        return buf.getvalue()


Adapter = dict[str, Callable | str | None]

# Engine display name -> adapter. Order controls report order. `module` is
# what must import for the engine to participate.
ENGINES: dict[str, Adapter] = {
    "pdfboss": {
        "module": "pdfboss",
        "open": pdfboss_open,
        "text": pdfboss_text,
        "markdown": pdfboss_markdown,
        "render": pdfboss_render,
    },
    "PyMuPDF": {
        "module": "fitz",
        "open": pymupdf_open,
        "text": pymupdf_text,
        "markdown": None,
        "render": pymupdf_render,
    },
    "pypdfium2": {
        "module": "pypdfium2",
        "open": pypdfium2_open,
        "text": pypdfium2_text,
        "markdown": None,
        "render": pypdfium2_render,
    },
    "pdfplumber": {
        "module": "pdfplumber",
        "open": pdfplumber_open,
        "text": pdfplumber_text,
        "markdown": None,
        "render": pdfplumber_render,
    },
}

# A page-1 render counts as non-blank when more than this percentage of its
# pixels is dark. Matches bench_render.py's notion of ink.
NONBLANK_INK = 0.1


def importable(module: str) -> bool:
    try:
        __import__(module)
    except ImportError:
        return False
    return True


def ink(png: bytes) -> float | None:
    """Percentage of dark pixels in a rendered page, or None without PIL."""
    try:
        from PIL import Image
    except ImportError:
        return None
    gray = Image.open(io.BytesIO(png)).convert("L").tobytes()
    return 100.0 * sum(1 for v in gray if v < 128) / len(gray)


def score_engine(
    name: str,
    adapter: Adapter,
    files: list[str],
    max_pages: int,
    scale: float,
    fonts: str,
) -> dict:
    """Quality counts for one engine over one corpus directory."""
    opened = 0
    text_ok = 0
    zero_text = 0
    fffd_docs = 0
    fffd_rates: list[float] = []
    chars_per_doc: list[int] = []
    markdown_ok = 0
    render_ok = 0
    nonblank = 0
    ink_known = 0
    for path in files:
        base = os.path.basename(path)
        try:
            adapter["open"](path)
            opened += 1
        except Exception:
            pass
        try:
            text = adapter["text"](path, max_pages)
            text_ok += 1
        except Exception:
            text = None
        if text is not None:
            chars = len(text)
            chars_per_doc.append(chars)
            if not chars:
                zero_text += 1
                print(f"    [{name}] {base}: 0 chars")
            else:
                fffd = text.count("�")
                rate = fffd / chars
                fffd_rates.append(rate)
                if fffd:
                    fffd_docs += 1
                print(f"    [{name}] {base}: {chars} chars, {fffd} U+FFFD ({100 * rate:.2f}%)")
        if adapter["markdown"] is not None:
            try:
                adapter["markdown"](path, max_pages)
                markdown_ok += 1
            except Exception:
                pass
        try:
            png = adapter["render"](path, scale, fonts)
            render_ok += 1
        except Exception:
            png = None
        if png is None:
            continue
        pct = ink(png)
        if pct is None:
            continue
        ink_known += 1
        if pct > NONBLANK_INK:
            nonblank += 1
    n = len(files)
    return {
        "open_ok": opened,
        "open_rate": opened / n,
        "text_ok": text_ok,
        "text_rate": text_ok / n,
        "docs_with_zero_text": zero_text,
        "docs_with_fffd": fffd_docs,
        "mean_fffd_rate": statistics.mean(fffd_rates) if fffd_rates else None,
        "max_fffd_rate": max(fffd_rates) if fffd_rates else None,
        "median_chars": int(statistics.median(chars_per_doc)) if chars_per_doc else None,
        "markdown_ok": markdown_ok if adapter["markdown"] is not None else None,
        "markdown_rate": markdown_ok / n if adapter["markdown"] is not None else None,
        "render_ok": render_ok,
        "render_nonblank": nonblank,
        "render_nonblank_rate": nonblank / ink_known if ink_known else None,
    }


def run_corpus(corpus: str, max_pages: int, scale: float, fonts: str) -> dict:
    files = sorted(glob.glob(os.path.join(corpus, "*.pdf")))
    if not files:
        raise SystemExit(f"no PDFs found in {corpus}")
    engines = {}
    for name, adapter in ENGINES.items():
        if not importable(adapter["module"]):
            print(f"  [{name}] not installed, skipped")
            continue
        print(f"  [{name}]")
        engines[name] = score_engine(name, adapter, files, max_pages, scale, fonts)
    return {"files": len(files), "engines": engines}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("corpora", nargs="+", help="directories of .pdf files")
    ap.add_argument("--max-pages", type=int, default=20, help="pages per doc for text/markdown")
    ap.add_argument("--scale", type=float, default=1.0, help="render scale factor")
    ap.add_argument(
        "--fonts",
        default="full",
        choices=("embedded-only", "all-embedded", "full"),
        help="pdfboss glyph-painting tier (full = substitute like the others)",
    )
    args = ap.parse_args()
    if args.max_pages <= 0:
        raise SystemExit("--max-pages must be >= 1")

    results = {
        "max_pages": args.max_pages,
        "scale": args.scale,
        "fonts": args.fonts,
        "corpora": {},
    }
    for corpus in args.corpora:
        name = os.path.basename(corpus.rstrip("/"))
        print(f"[{name}]")
        results["corpora"][name] = run_corpus(corpus, args.max_pages, args.scale, args.fonts)

    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "results-diversity.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
