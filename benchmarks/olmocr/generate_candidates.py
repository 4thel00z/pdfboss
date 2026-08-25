#!/usr/bin/env python3
"""Generate the pdfboss candidate tree for olmOCR-bench.

olmOCR-bench (HuggingFace dataset ``allenai/olmOCR-bench``) scores an engine
by the markdown it wrote for each single-page PDF under ``bench_data/pdfs/``.
The scorer treats every subdirectory of ``bench_data/`` except ``pdfs/`` as a
candidate, and hard-fails a candidate missing the file for *any* PDF — so
this script writes a file for every PDF no matter what: markdown when
extraction succeeds, an empty file when it does not (an empty file merely
fails its own tests). Markdown mode is required, not plain text: the table
tests only read pipe or HTML tables.

Filename convention (the PDF basename keeps its own suffix, so a double
``_pgN_pg1`` is correct):

    pdfs/arxiv_math/2502.15977_pg21.pdf
      -> pdfboss/arxiv_math/2502.15977_pg21_pg1_repeat1.md

One repeat is written — pdfboss is deterministic. The per-category summary
printed at the end doubles as a text-layer census: a category whose outputs
are almost all empty is image-only scans, which a non-OCR engine cannot read.

Usage:
    python benchmarks/olmocr/generate_candidates.py /path/to/bench_data
                                                    [--candidate NAME]
                                                    [--workers N]
"""

from __future__ import annotations

import argparse
import os
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path


def extract_one(pdf_path: str, pdf_root: str, out_root: str) -> tuple[str, int, str]:
    """Writes every page's markdown repeat for one PDF.

    Returns ``(category, chars_written, failure)`` where ``failure`` is empty
    when every page extracted, else the reason the output is empty. A PDF
    that cannot even open still gets an empty ``_pg1_repeat1.md`` — a missing
    file fails the whole candidate, an empty one only its own tests.
    """
    import pdfboss

    rel = Path(pdf_path).relative_to(pdf_root)
    category = rel.parts[0] if len(rel.parts) > 1 else "."
    out_dir = Path(out_root) / rel.parent
    out_dir.mkdir(parents=True, exist_ok=True)
    stem = rel.stem

    failure = ""
    page_count = 0
    try:
        doc = pdfboss.Document(pdf_path)
        page_count = doc.page_count
    except Exception as exc:  # noqa: BLE001 - the message is the result
        failure = f"{type(exc).__name__}: {exc}"
    if not page_count:
        (out_dir / f"{stem}_pg1_repeat1.md").write_text("", encoding="utf-8")
        return category, 0, failure or "no pages"

    chars = 0
    for index in range(page_count):
        try:
            markdown = doc[index].extract_markdown()
        except Exception as exc:  # noqa: BLE001 - the message is the result
            markdown = ""
            failure = f"{type(exc).__name__}: {exc}"
        out = out_dir / f"{stem}_pg{index + 1}_repeat1.md"
        out.write_text(markdown, encoding="utf-8")
        chars += len(markdown)
    return category, chars, failure


def run(bench_data: str, candidate: str, workers: int) -> None:
    pdf_root = os.path.join(bench_data, "pdfs")
    out_root = os.path.join(bench_data, candidate)
    pdfs = sorted(str(p) for p in Path(pdf_root).rglob("*.pdf"))
    if not pdfs:
        raise SystemExit(f"no PDFs found under {pdf_root}")

    stats: dict[str, dict[str, int]] = {}
    with ProcessPoolExecutor(max_workers=workers) as pool:
        results = pool.map(
            extract_one, pdfs, [pdf_root] * len(pdfs), [out_root] * len(pdfs)
        )
        for category, chars, failure in results:
            tally = stats.setdefault(
                category, {"pdfs": 0, "empty": 0, "errors": 0, "chars": 0}
            )
            tally["pdfs"] += 1
            tally["chars"] += chars
            if not chars:
                tally["empty"] += 1
            if failure:
                tally["errors"] += 1

    written = sum(1 for _ in Path(out_root).rglob("*.md"))
    print(f"[candidate] {written} markdown files for {len(pdfs)} PDFs -> {out_root}")
    print(f"{'category':16} {'pdfs':>5} {'empty':>6} {'errors':>7} {'chars/pdf':>10}")
    for category in sorted(stats):
        tally = stats[category]
        mean = tally["chars"] / tally["pdfs"]
        print(
            f"{category:16} {tally['pdfs']:5} {tally['empty']:6}"
            f" {tally['errors']:7} {mean:10.0f}"
        )
    if written < len(pdfs):
        raise SystemExit("fewer outputs than PDFs — the scorer would hard-fail")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("bench_data", help="the dataset's bench_data directory")
    ap.add_argument("--candidate", default="pdfboss", help="candidate folder name")
    ap.add_argument(
        "--workers",
        type=int,
        default=min(8, os.cpu_count() or 1),
        help="parallel extraction processes",
    )
    args = ap.parse_args()
    run(args.bench_data, args.candidate, args.workers)


if __name__ == "__main__":
    main()
