#!/usr/bin/env python3
"""Benchmark peak memory of pdfboss against other Python PDF libraries.

Peak RSS only means something when one engine owns the address space, so
every (engine, workload) pair runs in a fresh interpreter (this same script
in worker mode, via isolation.py) and the child measures itself with
``resource.getrusage(RUSAGE_SELF).ru_maxrss`` just before exiting. macOS
reports ru_maxrss in bytes and Linux in kilobytes; the worker normalizes to
bytes, so the numbers compare across platforms.

Three workloads, coarse to fine:

- import — import the engine and stop; the floor under the other numbers,
  since every peak includes the interpreter and the engine's own libraries.
- render — the largest file of the corpus: --pages evenly spaced pages to
  PNG bytes at --scale.
- text   — extract the text of every page of --sample evenly spaced corpus
  files, accumulating lengths only so no side carries a giant joined string.

Usage:
    python benchmarks/bench_memory.py /path/to/pdfs [--sample N] [--pages P]
                                      [--scale S] [--fonts TIER] [--timeout S]
"""

from __future__ import annotations

import argparse
import glob
import importlib
import json
import os
import resource
import sys

import isolation


def rss_bytes() -> int:
    """This process's peak RSS in bytes, whatever the platform reports in."""
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return peak
    return peak * 1024


def spread(count: int, limit: int) -> list[int]:
    """Evenly spaced indices across `count`, at most `limit` of them.

    If *limit* is <= 0 or >= *count* all indices are returned (no limit).
    """
    if limit <= 0 or limit >= count:
        return list(range(count))
    step = count / limit
    return [int(i * step) for i in range(limit)]


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


# --- render adapters ----------------------------------------------------------
#
# Each renders `pages` evenly spaced pages of `path` to PNG bytes at `scale`,
# the same materialization bench_render.py times, and returns how many pages
# failed — a quirky page should dent the count, not kill the measurement.


def pdfboss_render(path: str, pages: int, scale: float, fonts: str) -> int:
    import pdfboss

    doc = pdfboss.Document(path)
    failed = 0
    for i in spread(doc.page_count, pages):
        try:
            doc[i].render(scale=scale, fonts=fonts)
        except Exception:  # noqa: BLE001 - counted, not classified
            failed += 1
    return failed


def pymupdf_render(path: str, pages: int, scale: float, fonts: str) -> int:
    import fitz

    doc = fitz.open(path)
    try:
        matrix = fitz.Matrix(scale, scale)
        failed = 0
        for i in spread(doc.page_count, pages):
            try:
                doc[i].get_pixmap(matrix=matrix).tobytes("png")
            except Exception:  # noqa: BLE001 - counted, not classified
                failed += 1
        return failed
    finally:
        doc.close()


def pypdfium2_render(path: str, pages: int, scale: float, fonts: str) -> int:
    import io

    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        failed = 0
        for i in spread(len(doc), pages):
            try:
                buf = io.BytesIO()
                doc[i].render(scale=scale).to_pil().save(buf, format="PNG")
            except Exception:  # noqa: BLE001 - counted, not classified
                failed += 1
        return failed
    finally:
        doc.close()


def pdfplumber_render(path: str, pages: int, scale: float, fonts: str) -> int:
    import io

    import pdfplumber

    # pdfplumber measures rasterization in DPI, not as a scale factor.
    resolution = 72.0 * scale
    with pdfplumber.open(path) as pdf:
        failed = 0
        for i in spread(len(pdf.pages), pages):
            try:
                buf = io.BytesIO()
                pdf.pages[i].to_image(resolution=resolution).original.save(
                    buf, format="PNG"
                )
            except Exception:  # noqa: BLE001 - counted, not classified
                failed += 1
        return failed


# --- text adapters ------------------------------------------------------------
#
# Each extracts the text of every page of every file, page by page, and
# returns (total characters, files that raised).


def pdfboss_text(files: list[str]) -> tuple[int, int]:
    import pdfboss

    chars, failed = 0, 0
    for path in files:
        try:
            doc = pdfboss.Document(path)
            chars += sum(len(doc[i].extract_text()) for i in range(doc.page_count))
        except Exception:  # noqa: BLE001 - counted, not classified
            failed += 1
    return chars, failed


def pymupdf_text(files: list[str]) -> tuple[int, int]:
    import fitz

    chars, failed = 0, 0
    for path in files:
        try:
            doc = fitz.open(path)
            try:
                chars += sum(len(page.get_text()) for page in doc)
            finally:
                doc.close()
        except Exception:  # noqa: BLE001 - counted, not classified
            failed += 1
    return chars, failed


def pypdfium2_text(files: list[str]) -> tuple[int, int]:
    import pypdfium2

    chars, failed = 0, 0
    for path in files:
        try:
            doc = pypdfium2.PdfDocument(path)
            try:
                chars += sum(
                    len(doc[i].get_textpage().get_text_range())
                    for i in range(len(doc))
                )
            finally:
                doc.close()
        except Exception:  # noqa: BLE001 - counted, not classified
            failed += 1
    return chars, failed


def pdfplumber_text(files: list[str]) -> tuple[int, int]:
    import pdfplumber

    chars, failed = 0, 0
    for path in files:
        try:
            with pdfplumber.open(path) as pdf:
                chars += sum(len(page.extract_text() or "") for page in pdf.pages)
        except Exception:  # noqa: BLE001 - counted, not classified
            failed += 1
    return chars, failed


# Engine display name -> (module to probe, render, text). Order controls
# report order.
ENGINES = {
    "pdfboss": ("pdfboss", pdfboss_render, pdfboss_text),
    "PyMuPDF": ("fitz", pymupdf_render, pymupdf_text),
    "pypdfium2": ("pypdfium2", pypdfium2_render, pypdfium2_text),
    "pdfplumber": ("pdfplumber", pdfplumber_render, pdfplumber_text),
}

WORKLOADS = ("import", "render", "text")


def worker(engine: str, workload: str, spec: dict[str, object]) -> None:
    module, render, text = ENGINES[engine]
    isolation.stage(workload)
    payload: dict[str, object] = {}
    if workload == "import":
        importlib.import_module(module)
    if workload == "render":
        payload["pages_failed"] = render(
            str(spec["path"]),
            int(spec["pages"]),
            float(spec["scale"]),
            str(spec["fonts"]),
        )
    if workload == "text":
        files = sample_files(str(spec["corpus"]), int(spec["sample"]))
        payload["text_chars"], payload["files_failed"] = text(files)
    payload["ru_maxrss_bytes"] = rss_bytes()
    isolation.finish(payload)


def probe() -> list[str]:
    """The importable engines; the rest are reported and skipped."""
    available = []
    for name, spec in ENGINES.items():
        try:
            importlib.import_module(spec[0])
        except ImportError as exc:
            print(f"    {name:14} skipped — {exc}")
            continue
        available.append(name)
    return available


def largest_file(corpus: str) -> str:
    files = glob.glob(os.path.join(corpus, "*.pdf"))
    if not files:
        raise SystemExit(f"no PDFs found in {corpus}")
    return max(files, key=os.path.getsize)


def _page_count(path: str, engine: str) -> int:
    """Return the page count of *path* using *engine* (must be importable)."""
    module, _, _ = ENGINES[engine]
    lib = importlib.import_module(module)
    if engine == "pdfboss":
        return lib.Document(path).page_count
    if engine == "PyMuPDF":
        doc = lib.open(path)
        try:
            return doc.page_count
        finally:
            doc.close()
    if engine == "pypdfium2":
        doc = lib.PdfDocument(path)
        try:
            return len(doc)
        finally:
            doc.close()
    if engine == "pdfplumber":
        with lib.open(path) as doc:
            return len(doc.pages)
    raise ValueError(f"unknown engine: {engine}")


def run(
    corpus: str,
    sample_n: int,
    pages: int,
    scale: float,
    fonts: str,
    timeout: float,
) -> dict[str, object]:
    engines = probe()
    if not engines:
        raise SystemExit("no engine is importable; nothing to measure")
    big = largest_file(corpus)
    big_pages = _page_count(big, engines[0])
    big_bytes = os.path.getsize(big)
    rendered = len(spread(big_pages, pages))
    files = sample_files(corpus, sample_n)

    specs: dict[str, dict[str, object]] = {
        "import": {},
        "render": {"path": big, "pages": pages, "scale": scale, "fonts": fonts},
        "text": {"corpus": corpus, "sample": sample_n},
    }
    script = os.path.abspath(__file__)
    measured: dict[str, dict[str, dict[str, object]]] = {w: {} for w in WORKLOADS}
    for workload in WORKLOADS:
        for name in engines:
            args = [name, workload, json.dumps(specs[workload])]
            result = isolation.run_worker(script, args, timeout)
            if result["outcome"] != "ok":
                print(f"    {name:14} {workload}: {result['outcome']} ({result['detail']})")
                measured[workload][name] = {
                    "outcome": result["outcome"],
                    "detail": result["detail"],
                }
                continue
            payload = dict(result["payload"])
            peak = int(payload.pop("ru_maxrss_bytes"))
            measured[workload][name] = {
                "peak_rss_bytes": peak,
                "peak_rss_mib": round(peak / (1024 * 1024), 1),
                **payload,
            }

    print("[memory] peak RSS per fresh process, normalized to MiB")
    print(
        f"    render: largest corpus file ({big_bytes / 1e6:.1f} MB,"
        f" {big_pages} pages), {rendered} pages at scale {scale}, fonts={fonts}"
    )
    print(f"    text:   every page of {len(files)} corpus files")
    header = "".join(f"{w:>10}" for w in WORKLOADS)
    print(f"    {'':14}{header}")
    for name in engines:
        cells = "".join(
            f"{measured[w][name].get('peak_rss_mib', '—'):>10}" for w in WORKLOADS
        )
        print(f"    {name:14}{cells}")

    # Record the corpus by directory basename and the large file by shape,
    # never by name: the corpus is not public.
    return {
        "corpus": os.path.basename(corpus.rstrip("/")),
        "ru_maxrss_normalization": (
            "getrusage(RUSAGE_SELF).ru_maxrss read by the child itself;"
            " bytes on macOS, kilobytes elsewhere, normalized to bytes"
        ),
        "caveat": (
            "measured on a shared machine under concurrent load; peak RSS is"
            " mostly load-insensitive, but co-resident processes can shift"
            " numbers a few percent through allocator and page-cache pressure"
        ),
        "workloads": {
            "import": {"libraries": measured["import"]},
            "render": {
                "file_bytes": big_bytes,
                "document_pages": big_pages,
                "pages_rendered": rendered,
                "scale": scale,
                "fonts": fonts,
                "libraries": measured["render"],
            },
            "text": {
                "files": len(files),
                "libraries": measured["text"],
            },
        },
    }


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "worker":
        worker(sys.argv[2], sys.argv[3], json.loads(sys.argv[4]))
        return
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus", help="directory of .pdf files")
    ap.add_argument("--sample", type=int, default=40, help="files for the text workload")
    ap.add_argument("--pages", type=int, default=10, help="pages for the render workload")
    ap.add_argument("--scale", type=float, default=2.0, help="render scale factor")
    ap.add_argument(
        "--fonts",
        default="full",
        choices=("embedded-only", "all-embedded", "full"),
        help="pdfboss glyph-painting tier (full = substitute like the others)",
    )
    ap.add_argument("--timeout", type=float, default=900.0, help="per-worker seconds")
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(
        args.corpus, args.sample, args.pages, args.scale, args.fonts, args.timeout
    )
    out = os.path.join(here, "results-memory.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
