#!/usr/bin/env python3
"""Benchmark how pdfboss and other Python PDF libraries survive malformed PDFs.

Every other benchmark here filters its corpus down to files the engines
handle. This one feeds them files built to break parsers — fuzzer-minimized
malformed inputs from the OSS-Fuzz public corpora — and the question is not
speed but survival: does the engine come back with a page count, a clean
Python exception, or does it take the process down with it.

Two of the engines are C libraries running in-process, so a crash is not
catchable: a segfault in an adapter kills whichever process called it. Every
(file, engine) pair therefore runs in a fresh interpreter (this same script
in worker mode, via isolation.py) under a wall-clock timeout, and the parent
classifies how the worker came back:

- ok      — the stage completed
- error   — the engine raised a clean Python exception
- crash   — the process died: signal, nonzero exit, or exit without a result
- timeout — still running after --timeout seconds; SIGKILLed

Each worker runs two stages, parse (open + page count) then render (page 1
to pixels), with a stage marker flushed before each so a crash or timeout is
attributed to the stage that was running.

Usage:
    python benchmarks/bench_robustness.py /path/to/corpus [--sample N]
                                          [--timeout S] [--jobs J]

The corpus comes from benchmarks/fetch_stress_corpus.sh, downloaded OUTSIDE
the repo and never committed.
"""

from __future__ import annotations

import argparse
import importlib
import json
import os
import sys
from concurrent.futures import ThreadPoolExecutor

import isolation


# --- engine adapters ---------------------------------------------------------
#
# Two functions per engine: parse returns the page count, render rasterizes
# the first page. Each opens the file itself, so the stages are independent
# and a worker can attribute a crash to exactly one of them.


def pdfboss_parse(path: str) -> int:
    import pdfboss

    return pdfboss.Document(path).page_count


def pdfboss_render(path: str) -> None:
    import pdfboss

    pdfboss.Document(path)[0].render(scale=1.0)


def pymupdf_parse(path: str) -> int:
    import fitz

    doc = fitz.open(path)
    try:
        return doc.page_count
    finally:
        doc.close()


def pymupdf_render(path: str) -> None:
    import fitz

    doc = fitz.open(path)
    try:
        doc[0].get_pixmap()
    finally:
        doc.close()


def pypdfium2_parse(path: str) -> int:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        return len(doc)
    finally:
        doc.close()


def pypdfium2_render(path: str) -> None:
    import pypdfium2

    doc = pypdfium2.PdfDocument(path)
    try:
        doc[0].render(scale=1.0)
    finally:
        doc.close()


def pdfplumber_parse(path: str) -> int:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        return len(pdf.pages)


def pdfplumber_render(path: str) -> None:
    import pdfplumber

    with pdfplumber.open(path) as pdf:
        pdf.pages[0].to_image(resolution=72)


# Engine display name -> (module to probe, parse, render). Order controls
# report order.
ENGINES = {
    "pdfboss": ("pdfboss", pdfboss_parse, pdfboss_render),
    "PyMuPDF": ("fitz", pymupdf_parse, pymupdf_render),
    "pypdfium2": ("pypdfium2", pypdfium2_parse, pypdfium2_render),
    "pdfplumber": ("pdfplumber", pdfplumber_parse, pdfplumber_render),
}

STAGES = ("parse", "render")
OUTCOMES = ("ok", "error", "crash", "timeout", "not_reached")


def worker(engine: str, path: str) -> None:
    parse, render = ENGINES[engine][1:]
    stages: dict[str, str] = {name: "not_reached" for name in STAGES}
    isolation.stage("parse")
    try:
        pages = parse(path)
    except Exception as exc:  # noqa: BLE001 - the classification is the result
        stages["parse"] = f"error: {type(exc).__name__}"
        isolation.finish({"stages": stages})
        return
    stages["parse"] = "ok"
    if not pages:
        stages["render"] = "error: no pages"
        isolation.finish({"stages": stages})
        return
    isolation.stage("render")
    try:
        render(path)
    except Exception as exc:  # noqa: BLE001 - the classification is the result
        stages["render"] = f"error: {type(exc).__name__}"
    else:
        stages["render"] = "ok"
    isolation.finish({"stages": stages})


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


def corpus_files(corpus: str) -> list[str]:
    """Every regular file under the corpus dir, minus the fetch leftovers."""
    files = sorted(
        os.path.join(root, name)
        for root, dirs, names in os.walk(corpus)
        for name in names
        if not name.startswith(".") and not name.endswith((".zip", ".log"))
    )
    if not files:
        raise SystemExit(f"no files found in {corpus}")
    return files


def sample_files(files: list[str], n: int) -> list[str]:
    if n <= 0:
        raise SystemExit("--sample must be a positive number of files")
    if n >= len(files):
        return files
    # Evenly spaced across the sorted corpus for a representative spread.
    step = len(files) / n
    return [files[int(i * step)] for i in range(n)]


def classify(result: dict[str, object]) -> tuple[dict[str, str], dict[str, str]]:
    """One worker result -> per-stage outcome, plus per-stage detail.

    The detail is the crash signal, the timeout note, or the exception type
    name — whatever names the failure, keyed by the stage it belongs to.
    """
    outcome = str(result["outcome"])
    if outcome in ("crash", "timeout"):
        at = str(result["stage"] or "parse")
        outcomes: dict[str, str] = {}
        details: dict[str, str] = {}
        passed = True
        for name in STAGES:
            if name == at:
                outcomes[name] = outcome
                details[name] = str(result["detail"])
                passed = False
                continue
            outcomes[name] = "ok" if passed else "not_reached"
        return outcomes, details
    payload = result["payload"]
    if not isinstance(payload, dict):
        raise RuntimeError(f"worker reported ok without stages: {result}")
    stages = payload["stages"]
    outcomes = {}
    details = {}
    for name in STAGES:
        value = str(stages.get(name, "not_reached"))
        if not value.startswith("error"):
            outcomes[name] = value
            continue
        outcomes[name] = "error"
        details[name] = value.partition(": ")[2] or "error"
    return outcomes, details


def subcorpus_counts(corpus: str, files: list[str]) -> dict[str, int]:
    """How many sampled files came from each top-level corpus subdir."""
    counts: dict[str, int] = {}
    for f in files:
        head = os.path.relpath(f, corpus).split(os.sep)[0]
        key = head if os.path.isdir(os.path.join(corpus, head)) else "."
        counts[key] = counts.get(key, 0) + 1
    return counts


def run(corpus: str, sample_n: int, timeout: float, jobs: int) -> dict[str, object]:
    engines = probe()
    if not engines:
        raise SystemExit("no engine is importable; nothing to measure")
    all_files = corpus_files(corpus)
    files = sample_files(all_files, sample_n)
    print(
        f"[corpus] {len(files)} of {len(all_files)} files sampled,"
        f" {len(engines)} engines, timeout {timeout:g}s, {jobs} workers"
    )

    counts = {name: {s: {o: 0 for o in OUTCOMES} for s in STAGES} for name in engines}
    details: dict[str, dict[str, dict[str, int]]] = {
        name: {s: {} for s in STAGES} for name in engines
    }
    survived = {name: 0 for name in engines}
    script = os.path.abspath(__file__)
    tasks = [(f, name) for f in files for name in engines]

    def one(task: tuple[str, str]) -> tuple[str, dict[str, object]]:
        path, name = task
        return name, isolation.run_worker(script, [name, path], timeout)

    done = 0
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for name, result in pool.map(one, tasks):
            outcomes, failure = classify(result)
            for s in STAGES:
                counts[name][s][outcomes[s]] += 1
                if s not in failure:
                    continue
                tally = details[name][s]
                tally[failure[s]] = tally.get(failure[s], 0) + 1
            if all(outcomes[s] not in ("crash", "timeout") for s in STAGES):
                survived[name] += 1
            done += 1
            if done % 200 == 0:
                crashes = sum(
                    counts[n][s]["crash"] for n in engines for s in STAGES
                )
                timeouts = sum(
                    counts[n][s]["timeout"] for n in engines for s in STAGES
                )
                print(
                    f"    [{done}/{len(tasks)}] crashes={crashes}"
                    f" timeouts={timeouts}",
                    flush=True,
                )

    print(f"[robustness] {len(files)} malformed files per engine")
    for name in engines:
        rate = 100.0 * survived[name] / len(files)
        p, r = counts[name]["parse"], counts[name]["render"]
        print(
            f"    {name:14} survives {rate:6.2f}%   parse"
            f" ok/err/crash/timeout {p['ok']}/{p['error']}/{p['crash']}/{p['timeout']}"
            f"   render {r['ok']}/{r['error']}/{r['crash']}/{r['timeout']}"
            f" (+{r['not_reached']} not reached)"
        )

    # The corpus is public (OSS-Fuzz), so naming it and its subdirs is fine —
    # individual seed files are hash-named and recorded nowhere.
    return {
        "corpus": "OSS-Fuzz public corpora (mupdf_pdf_fuzzer + poppler_pdf_fuzzer)",
        "files_total": len(all_files),
        "files_sampled": len(files),
        "sampled_from": subcorpus_counts(corpus, files),
        "timeout_seconds": timeout,
        "stages": {"parse": "open + page count", "render": "first page to pixels"},
        "engines": {
            name: {
                "survival_percent": round(100.0 * survived[name] / len(files), 2),
                "parse": counts[name]["parse"],
                "render": counts[name]["render"],
                "failure_detail": details[name],
            }
            for name in engines
        },
    }


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == "worker":
        worker(sys.argv[2], sys.argv[3])
        return
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus", help="directory of malformed PDFs (fetch_stress_corpus.sh)")
    ap.add_argument("--sample", type=int, default=2000, help="files to sample")
    ap.add_argument("--timeout", type=float, default=20.0, help="per-worker seconds")
    ap.add_argument("--jobs", type=int, default=4, help="concurrent workers")
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(args.corpus, args.sample, args.timeout, args.jobs)
    out = os.path.join(here, "results-robustness.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
