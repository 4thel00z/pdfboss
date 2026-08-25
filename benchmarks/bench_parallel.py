#!/usr/bin/env python3
"""Benchmark parallel throughput against other Python PDF libraries.

The other benchmarks time strictly sequential per-page loops, which understates
every engine that can use more than one core. pdfboss's `Document.render_pages`
and `Document.extract_text` fan the pages out across the machine's cores in one
call, and the competing engines can thread too — so this script times each
engine twice on the same workload: a sequential per-page baseline and the
engine's best available parallel route, and reports the speedup honestly.

Two workloads:

- **scan** — render evenly spaced pages of one scanned document to PNG bytes,
  the `bench_scans.py` workload where every engine rasterizes the same bilevel
  picture and no glyph-painting gate is needed.
- **text** — extract the text of every page of a corpus sample, the `bench.py`
  workload.

Parallel routes, per engine:

- pdfboss: one call — `render_pages` / `extract_text`. Internally parallel
  (one worker per core, per-worker document forks); it has no thread knob, so
  `--threads` does not apply to it.
- PyMuPDF: `ThreadPoolExecutor` over per-page calls with one document handle
  per worker thread (its objects are not thread-safe across a shared handle).
- pypdfium2, pdfplumber rendering: pdfium's contract requires callers to
  serialize every pdfium call across threads, and it means it — threaded
  per-thread-document rendering intermittently corrupts pdfium's document
  loader for the rest of the process, and pdfplumber's `to_image` (a fresh
  pdfium document open/close inside every call) crashes the process outright
  (both reproduced here). So the harness serializes all pdfium calls under
  one lock and threads only the PNG encoding; the resulting near-1x IS the
  pdfium threading story. Parallel pdfium in Python means processes, a
  different workload shape than this in-process comparison.
- pdfplumber text extraction threads safely but is pure-Python and GIL-bound
  — that is its parallel story there. pypdfium2's text goes through its
  textpage API here (unused in `bench.py`).

Usage:
    python benchmarks/bench_parallel.py scan /path/to/scan.pdf
                                        [--pages N] [--repeat K] [--scale S]
                                        [--threads T]
    python benchmarks/bench_parallel.py text /path/to/pdfs
                                        [--sample N] [--repeat K] [--threads T]
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import os
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from typing import Any, Callable

PDFIUM_LOCK = threading.Lock()
FONTS = "all-embedded"


# --- render adapters ----------------------------------------------------------
#
# Each returns the PNG bytes of `indices` of `path` at `scale`, so no engine
# can skip the work and PNG encoding is on every side. The sequential route is
# a per-page loop over one document; the parallel route is the engine's best.


def pdfboss_render_seq(path: str, indices: list[int], scale: float) -> list[bytes]:
    import pdfboss

    doc = pdfboss.Document(path)
    return [doc[i].render(scale=scale, fonts=FONTS) for i in indices]


def pdfboss_render_par(
    path: str, indices: list[int], scale: float, threads: int
) -> list[bytes]:
    import pdfboss

    return pdfboss.Document(path).render_pages(
        pages=indices, scale=scale, fonts=FONTS
    )


def pymupdf_render_seq(path: str, indices: list[int], scale: float) -> list[bytes]:
    import fitz

    doc = fitz.open(path)
    try:
        matrix = fitz.Matrix(scale, scale)
        return [doc[i].get_pixmap(matrix=matrix).tobytes("png") for i in indices]
    finally:
        doc.close()


def pymupdf_render_par(
    path: str, indices: list[int], scale: float, threads: int
) -> list[bytes]:
    import fitz

    local = threading.local()
    docs: list[Any] = []
    docs_lock = threading.Lock()
    matrix = fitz.Matrix(scale, scale)

    def one(i: int) -> bytes:
        if not hasattr(local, "doc"):
            local.doc = fitz.open(path)
            with docs_lock:
                docs.append(local.doc)
        return local.doc[i].get_pixmap(matrix=matrix).tobytes("png")

    try:
        with ThreadPoolExecutor(max_workers=threads) as ex:
            return list(ex.map(one, indices))
    finally:
        for doc in docs:
            doc.close()


def pypdfium2_png(page: Any) -> bytes:
    buf = io.BytesIO()
    page.to_pil().save(buf, format="PNG")
    return buf.getvalue()


def pypdfium2_render_seq(path: str, indices: list[int], scale: float) -> list[bytes]:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        return [pypdfium2_png(doc[i].render(scale=scale)) for i in indices]
    finally:
        doc.close()


def pypdfium2_render_par(
    path: str, indices: list[int], scale: float, threads: int
) -> list[bytes]:
    import pypdfium2

    with PDFIUM_LOCK:
        doc = pypdfium2.PdfDocument(path)

    def one(i: int) -> bytes:
        with PDFIUM_LOCK:
            pil = doc[i].render(scale=scale).to_pil()
        buf = io.BytesIO()
        pil.save(buf, format="PNG")
        return buf.getvalue()

    try:
        with ThreadPoolExecutor(max_workers=threads) as ex:
            return list(ex.map(one, indices))
    finally:
        with PDFIUM_LOCK:
            doc.close()


def pdfplumber_png(pdf: Any, i: int, resolution: float) -> bytes:
    page = pdf.pages[i]
    buf = io.BytesIO()
    page.to_image(resolution=resolution).original.save(buf, format="PNG")
    page.close()
    return buf.getvalue()


def pdfplumber_render_seq(path: str, indices: list[int], scale: float) -> list[bytes]:
    import pdfplumber

    resolution = 72.0 * scale
    with pdfplumber.open(path) as pdf:
        return [pdfplumber_png(pdf, i, resolution) for i in indices]


def pdfplumber_render_par(
    path: str, indices: list[int], scale: float, threads: int
) -> list[bytes]:
    import pdfplumber

    resolution = 72.0 * scale
    lock = threading.Lock()
    with pdfplumber.open(path) as pdf:

        def one(i: int) -> bytes:
            with lock:
                return pdfplumber_png(pdf, i, resolution)

        with ThreadPoolExecutor(max_workers=threads) as ex:
            return list(ex.map(one, indices))


# --- text adapters --------------------------------------------------------
#
# Each returns the concatenated text of every page of `path`, so an engine
# that extracts nothing is visible in the recorded character counts.


def pdfboss_text_seq(path: str) -> str:
    import pdfboss

    doc = pdfboss.Document(path)
    return "\f".join(doc[i].extract_text() for i in range(doc.page_count))


def pdfboss_text_par(path: str, threads: int) -> str:
    import pdfboss

    return pdfboss.Document(path).extract_text()


def pymupdf_text_seq(path: str) -> str:
    import fitz

    doc = fitz.open(path)
    try:
        return "".join(page.get_text() for page in doc)
    finally:
        doc.close()


def pymupdf_text_par(path: str, threads: int) -> str:
    import fitz

    local = threading.local()
    docs: list[Any] = []
    docs_lock = threading.Lock()
    count = pymupdf_page_count(path)

    def one(i: int) -> str:
        if not hasattr(local, "doc"):
            local.doc = fitz.open(path)
            with docs_lock:
                docs.append(local.doc)
        return local.doc[i].get_text()

    try:
        with ThreadPoolExecutor(max_workers=threads) as ex:
            return "".join(ex.map(one, range(count)))
    finally:
        for doc in docs:
            doc.close()


def pymupdf_page_count(path: str) -> int:
    import fitz

    doc = fitz.open(path)
    try:
        return doc.page_count
    finally:
        doc.close()


def pypdfium2_text_seq(path: str) -> str:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        return "".join(doc[i].get_textpage().get_text_bounded() for i in range(len(doc)))
    finally:
        doc.close()


def pypdfium2_text_par(path: str, threads: int) -> str:
    import pypdfium2

    with PDFIUM_LOCK:
        doc = pypdfium2.PdfDocument(path)
        count = len(doc)

    def one(i: int) -> str:
        with PDFIUM_LOCK:
            return doc[i].get_textpage().get_text_bounded()

    try:
        with ThreadPoolExecutor(max_workers=threads) as ex:
            return "".join(ex.map(one, range(count)))
    finally:
        with PDFIUM_LOCK:
            doc.close()


def pdfplumber_text_seq(path: str) -> str:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        return "".join((page.extract_text() or "") for page in pdf.pages)


def pdfplumber_text_par(path: str, threads: int) -> str:
    import pdfplumber

    local = threading.local()
    docs: list[Any] = []
    docs_lock = threading.Lock()
    with pdfplumber.open(path) as pdf:
        count = len(pdf.pages)

    def one(i: int) -> str:
        if not hasattr(local, "doc"):
            local.doc = pdfplumber.open(path)
            with docs_lock:
                docs.append(local.doc)
        return local.doc.pages[i].extract_text() or ""

    try:
        with ThreadPoolExecutor(max_workers=threads) as ex:
            return "".join(ex.map(one, range(count)))
    finally:
        for doc in docs:
            doc.close()


# Engine display name -> routes and the one-line parallel policy that goes in
# the results. Order controls report order.
RENDER_ENGINES: dict[str, dict[str, Any]] = {
    "pdfboss": {
        "seq": pdfboss_render_seq,
        "par": pdfboss_render_par,
        "policy": "one call (render_pages), internally parallel across cores",
    },
    "PyMuPDF": {
        "seq": pymupdf_render_seq,
        "par": pymupdf_render_par,
        "policy": "thread pool, one document per worker thread",
    },
    "pypdfium2": {
        "seq": pypdfium2_render_seq,
        "par": pypdfium2_render_par,
        "policy": "pdfium calls serialized (upstream contract); PNG encoding threads",
    },
    "pdfplumber": {
        "seq": pdfplumber_render_seq,
        "par": pdfplumber_render_par,
        "policy": "serialized by the harness — concurrent rasterization crashes",
    },
}

TEXT_ENGINES: dict[str, dict[str, Any]] = {
    "pdfboss": {
        "seq": pdfboss_text_seq,
        "par": pdfboss_text_par,
        "policy": "one call (extract_text), internally parallel across cores",
    },
    "PyMuPDF": {
        "seq": pymupdf_text_seq,
        "par": pymupdf_text_par,
        "policy": "thread pool, one document per worker thread",
    },
    "pypdfium2": {
        "seq": pypdfium2_text_seq,
        "par": pypdfium2_text_par,
        "policy": "pdfium calls serialized (upstream contract) — near-1x expected",
    },
    "pdfplumber": {
        "seq": pdfplumber_text_seq,
        "par": pdfplumber_text_par,
        "policy": "thread pool, one document per worker; pure Python, GIL-bound",
    },
}


def page_indices(path: str, limit: int) -> tuple[list[int], int]:
    """Evenly spaced page indices across the document, at most `limit` of them."""
    import pdfboss

    count = pdfboss.Document(path).page_count
    if count == 0:
        raise SystemExit(f"{path} has no pages")
    if limit <= 0 or limit >= count:
        return list(range(count)), count
    step = count / limit
    return [int(i * step) for i in range(limit)], count


def ink(png: bytes) -> float | None:
    """Percentage of dark pixels in a rendered page, or None without PIL.

    An engine that cannot decode the scan's bilevel codec often returns a
    blank page instead of raising, which would time beautifully and mean
    nothing. Comparing ink across engines catches that.
    """
    try:
        from PIL import Image
    except ImportError:
        return None
    gray = Image.open(io.BytesIO(png)).convert("L").tobytes()
    return 100.0 * sum(1 for v in gray if v < 128) / len(gray)


def best_of(call: Callable[[], object], repeat: int) -> tuple[float | None, str | None]:
    """Best-of-`repeat` wall time, or the reason the call raised."""
    best: float | None = None
    for attempt in range(repeat):
        start = time.perf_counter()
        try:
            call()
        except Exception as exc:  # noqa: BLE001 - the message is the result
            return None, f"{type(exc).__name__}: {exc}".strip()
        elapsed = time.perf_counter() - start
        if best is None or elapsed < best:
            best = elapsed
    return best, None


def save(section: str, payload: dict[str, Any], out: str) -> None:
    """Merge `payload` under `section` into the results file and write it.

    Written after every unit of work, so an engine crash mid-run keeps what
    already finished.
    """
    results: dict[str, Any] = {}
    if os.path.exists(out):
        with open(out) as f:
            results = json.load(f)
    results[section] = payload
    with open(out, "w") as f:
        json.dump(results, f, indent=2)


def run_scan(
    path: str, limit: int, repeat: int, scale: float, threads: int, out: str
) -> None:
    indices, total_pages = page_indices(path, limit)

    supported: dict[str, dict[str, Any]] = {}
    refused: dict[str, str] = {}
    coverage: dict[str, float | None] = {}
    for name, spec in RENDER_ENGINES.items():
        try:
            pages = spec["seq"](path, indices[:1], scale)
        except ImportError:
            refused[name] = "not installed"
        except Exception as exc:  # noqa: BLE001 - the message is the result
            refused[name] = f"{type(exc).__name__}: {exc}".strip()
        else:
            supported[name] = spec
            coverage[name] = ink(pages[0])
    for name, why in refused.items():
        print(f"    {name:14} cannot render this file — {why}")
    if not supported:
        raise SystemExit("no engine rendered the file")
    print(f"[ink] page {indices[0] + 1}, dark pixels — the renders must agree")
    for name, pct in coverage.items():
        print(f"    {name:14} {'n/a' if pct is None else f'{pct:8.2f}%'}")

    # Record the file's shape, never its name: the corpus is not public.
    engines: dict[str, dict[str, Any]] = {}
    payload: dict[str, Any] = {
        "document_pages": total_pages,
        "pages_rendered": len(indices),
        "scale": scale,
        "repeat": repeat,
        "threads": threads,
        "cpu_count": os.cpu_count(),
        "ink_percent": coverage,
        "refused": refused,
        "engines": engines,
    }
    print(f"[scan] {len(indices)} of {total_pages} pages at scale {scale}, {threads} threads")
    for name, spec in supported.items():
        seq_call = lambda s=spec: s["seq"](path, indices, scale)
        par_call = lambda s=spec: s["par"](path, indices, scale, threads)
        for call in (seq_call, par_call):
            try:
                call()
            except Exception:
                pass
        seq, seq_err = best_of(seq_call, repeat)
        par, par_err = best_of(par_call, repeat)
        if seq is None and par is None:
            refused[name] = f"failed under timing — seq: {seq_err}; par: {par_err}"
            print(f"    {name:14} {refused[name]}")
            save("scan", payload, out)
            continue
        engines[name] = {
            "sequential_s": seq,
            "parallel_s": par,
            "speedup": seq / par if seq and par else None,
            "pages": len(indices),
            "policy": spec["policy"],
        }
        if seq_err or par_err:
            engines[name]["error"] = seq_err or par_err
        save("scan", payload, out)
        seq_pps = f"{len(indices) / seq:9.1f}" if seq else "      n/a"
        par_pps = f"{len(indices) / par:9.1f}" if par else "      n/a"
        gain = f"{seq / par:5.2f}x" if seq and par else "   n/a"
        print(f"    {name:14} seq {seq_pps} pages/s   par {par_pps} pages/s   {gain}")


def sample_files(corpus: str, n: int) -> list[str]:
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


def run_text(corpus: str, sample_n: int, repeat: int, threads: int, out: str) -> None:
    import pdfboss

    files = sample_files(corpus, sample_n)
    pages = {}
    for f in files:
        try:
            pages[f] = pdfboss.Document(f).page_count
        except Exception:
            pages[f] = 0

    # Time each file in both modes, dumping progress after every file, then
    # aggregate only over files EVERY engine handled in BOTH modes so the
    # totals compare the exact same workload.
    timings: dict[str, dict[str, dict[str, float]]] = {
        name: {"seq": {}, "par": {}} for name in TEXT_ENGINES
    }
    chars: dict[str, dict[str, int]] = {name: {} for name in TEXT_ENGINES}
    payload: dict[str, Any] = {
        "corpus": os.path.basename(corpus.rstrip("/")),
        "files_sampled": len(files),
        "repeat": repeat,
        "threads": threads,
        "cpu_count": os.cpu_count(),
        "in_progress": True,
    }
    for pos, f in enumerate(files):
        for name, spec in TEXT_ENGINES.items():
            seq_call = lambda s=spec, p=f: s["seq"](p)
            par_call = lambda s=spec, p=f: s["par"](p, threads)
            try:
                chars[name][f] = len(seq_call())
            except Exception:
                continue
            try:
                par_call()
            except Exception:
                continue
            seq, seq_err = best_of(seq_call, repeat)
            par, par_err = best_of(par_call, repeat)
            if seq is None or par is None:
                print(f"    {name:14} skipped a file — {seq_err or par_err}")
                continue
            timings[name]["seq"][f] = seq
            timings[name]["par"][f] = par
        payload["files_timed"] = pos + 1
        save("text", payload, out)
        print(f"[text] {pos + 1}/{len(files)} files timed", flush=True)

    common = set(files)
    for name in TEXT_ENGINES:
        common &= set(timings[name]["seq"])
    engines: dict[str, dict[str, Any]] = {}
    for name, spec in TEXT_ENGINES.items():
        seq = sum(timings[name]["seq"][f] for f in common)
        par = sum(timings[name]["par"][f] for f in common)
        engines[name] = {
            "sequential_s": seq,
            "parallel_s": par,
            "speedup": seq / par if seq and par else None,
            "pages": sum(pages[f] for f in common),
            "chars": sum(chars[name][f] for f in common),
            "policy": spec["policy"],
        }
    payload.pop("in_progress")
    payload["files_compared"] = len(common)
    payload["engines"] = engines
    save("text", payload, out)
    print(f"[text] compared {len(common)} files across {len(engines)} engines, {threads} threads")
    for name, r in sorted(engines.items(), key=lambda kv: kv[1]["parallel_s"] or 1e9):
        seq_pps = f"{r['pages'] / r['sequential_s']:9.1f}" if r["sequential_s"] else "      n/a"
        par_pps = f"{r['pages'] / r['parallel_s']:9.1f}" if r["parallel_s"] else "      n/a"
        gain = f"{r['speedup']:5.2f}x" if r["speedup"] else "   n/a"
        print(f"    {name:14} seq {seq_pps} pages/s   par {par_pps} pages/s   {gain}")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="workload", required=True)
    scan = sub.add_parser("scan", help="render a scanned document's pages")
    scan.add_argument("scan_pdf", help="a scanned .pdf — one bilevel image per page")
    scan.add_argument("--pages", type=int, default=60, help="pages to render (0 = all)")
    scan.add_argument("--repeat", type=int, default=3, help="best-of-N")
    scan.add_argument("--scale", type=float, default=1.0, help="render scale factor")
    scan.add_argument("--threads", type=int, default=0, help="worker threads (0 = cores)")
    text = sub.add_parser("text", help="extract every page's text from a corpus sample")
    text.add_argument("corpus", help="directory of .pdf files")
    text.add_argument("--sample", type=int, default=40, help="files to sample")
    text.add_argument("--repeat", type=int, default=3, help="best-of-N per file")
    text.add_argument("--threads", type=int, default=0, help="worker threads (0 = cores)")
    args = ap.parse_args()

    threads = args.threads or os.cpu_count() or 1
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.join(here, "results-parallel.json")
    if args.workload == "scan":
        run_scan(args.scan_pdf, args.pages, args.repeat, args.scale, threads, out)
    if args.workload == "text":
        run_text(args.corpus, args.sample, args.repeat, threads, out)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
