#!/usr/bin/env python3
"""Score pdfboss's rendering fidelity against a reference rasterizer.

The render benchmark's ink gate proves a page is not blank; it says nothing
about whether the pixels are *right*. This script quantifies closeness: every
engine renders the first page of each sampled file, and each engine's render
is compared against pypdfium2's — the fastest C engine in the render
benchmark — with windowed SSIM and mean absolute pixel difference. pdfboss is
scored in a field, not alone: pdfplumber rasterizes via pdfium, so its row is
a same-engine control (the practical ceiling of the metric), and PyMuPDF is
the honest cross-engine baseline that shows how far two independent
rasterizers agree at all.

Engines resample differently and page dimensions can drift by a pixel, so the
renders are aligned before scoring: all at scale 2.0, grayscale,
center-cropped to the common minimum dimensions, then downsampled to half
resolution with Lanczos — which also suppresses engine-specific anti-aliasing
phase differences.

SSIM is a quality metric, not a timing one, so the scores are insensitive to
machine load and published as measured.

Usage:
    python benchmarks/bench_fidelity.py /path/to/pdfs [--sample N] [--scale S]
                                        [--fonts TIER]
"""

from __future__ import annotations

import argparse
import glob
import io
import json
import os
import traceback

import numpy as np
from PIL import Image


# --- library adapters -------------------------------------------------------
#
# Each renders the first page of `path` at `scale` and returns PNG bytes.
# `fonts` is pdfboss's glyph-painting tier; the other libraries have no such
# knob and ignore it.


def pdfboss_render(path: str, scale: float, fonts: str) -> bytes:
    import pdfboss

    doc = pdfboss.Document(path)
    return doc[0].render(scale=scale, fonts=fonts, compression="none")


def pymupdf_render(path: str, scale: float, fonts: str) -> bytes:
    import fitz

    doc = fitz.open(path)
    try:
        matrix = fitz.Matrix(scale, scale)
        return doc[0].get_pixmap(matrix=matrix).tobytes("png")
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


def pdfplumber_render(path: str, scale: float, fonts: str) -> bytes:
    import pdfplumber

    # pdfplumber measures rasterization in DPI, not as a scale factor.
    resolution = 72.0 * scale
    with pdfplumber.open(path) as pdf:
        page = pdf.pages[0]
        buf = io.BytesIO()
        page.to_image(resolution=resolution).original.save(buf, format="PNG")
        page.close()
        return buf.getvalue()


# Library display name -> renderer. Order controls report order.
LIBS = {
    "pdfboss": pdfboss_render,
    "PyMuPDF": pymupdf_render,
    "pypdfium2": pypdfium2_render,
    "pdfplumber": pdfplumber_render,
}
REFERENCE = "pypdfium2"

# Standard SSIM constants: window size, stabilizers K1/K2, dynamic range L.
WINDOW = 8
K1 = 0.01
K2 = 0.03
LEVELS = 255.0


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


def certify(path: str, scale: float, fonts: str) -> str | None:
    """Why the file's first page cannot be scored, or None when it can.

    A file where pdfboss's ``render_reporting`` reports dropped or
    approximated content is excluded — comparing a knowingly incomplete
    render against a reference would measure the known gap, not fidelity.
    The reason keeps the report's own wording so the exclusion tally names
    what pdfboss cannot paint rather than hiding it.
    """
    import pdfboss

    try:
        doc = pdfboss.Document(path)
    except Exception as exc:  # noqa: BLE001 - the message is the result
        return f"unreadable: {type(exc).__name__}: {exc}"
    if not doc.page_count:
        return "no pages"
    try:
        warnings = doc[0].render_reporting(scale=scale, fonts=fonts)[1]
    except Exception as exc:  # noqa: BLE001 - the message is the result
        return f"render failed: {type(exc).__name__}: {exc}"
    if warnings:
        return warnings[0]
    return None


def grayscale(png: bytes) -> Image.Image:
    """The PNG decoded to 8-bit grayscale, alpha composited onto white."""
    image = Image.open(io.BytesIO(png))
    if "A" not in image.getbands():
        return image.convert("L")
    white = Image.new("RGBA", image.size, (255, 255, 255, 255))
    return Image.alpha_composite(white, image.convert("RGBA")).convert("L")


def align(images: dict[str, Image.Image]) -> dict[str, np.ndarray]:
    """Every engine's render center-cropped to the common minimum dimensions
    and Lanczos-downsampled to half resolution, as float64 arrays."""
    width = min(image.width for image in images.values())
    height = min(image.height for image in images.values())
    if width < 2 * WINDOW or height < 2 * WINDOW:
        raise ValueError(f"page too small to score: {width}x{height}")
    aligned = {}
    for name, image in images.items():
        left = (image.width - width) // 2
        top = (image.height - height) // 2
        cropped = image.crop((left, top, left + width, top + height))
        half = cropped.resize((width // 2, height // 2), Image.LANCZOS)
        aligned[name] = np.asarray(half, dtype=np.float64)
    return aligned


def box_mean(values: np.ndarray, k: int) -> np.ndarray:
    """Uniform k*k window mean over all fully-inside windows, via the
    integral image — no scipy needed."""
    integral = np.zeros((values.shape[0] + 1, values.shape[1] + 1))
    integral[1:, 1:] = values.cumsum(axis=0).cumsum(axis=1)
    sums = (
        integral[k:, k:]
        - integral[:-k, k:]
        - integral[k:, :-k]
        + integral[:-k, :-k]
    )
    return sums / (k * k)


def ssim(x: np.ndarray, y: np.ndarray) -> float:
    """Mean structural similarity over uniform WINDOW*WINDOW windows."""
    c1 = (K1 * LEVELS) ** 2
    c2 = (K2 * LEVELS) ** 2
    mean_x = box_mean(x, WINDOW)
    mean_y = box_mean(y, WINDOW)
    # E[x^2] - E[x]^2 can dip epsilon-negative in float; clamp at zero.
    var_x = np.maximum(box_mean(x * x, WINDOW) - mean_x * mean_x, 0.0)
    var_y = np.maximum(box_mean(y * y, WINDOW) - mean_y * mean_y, 0.0)
    cov = box_mean(x * y, WINDOW) - mean_x * mean_y
    numerator = (2.0 * mean_x * mean_y + c1) * (2.0 * cov + c2)
    denominator = (mean_x**2 + mean_y**2 + c1) * (var_x + var_y + c2)
    return float(np.mean(numerator / denominator))


def mean_abs_diff(x: np.ndarray, y: np.ndarray) -> float:
    """Mean absolute pixel difference on the 0-255 grayscale range."""
    return float(np.mean(np.abs(x - y)))


def score_file(path: str, scale: float, fonts: str) -> dict[str, dict[str, float]] | str:
    """Each non-reference engine's scores against the reference, or the
    reason the file cannot be compared."""
    renders = {}
    for name, fn in LIBS.items():
        try:
            renders[name] = grayscale(fn(path, scale, fonts))
        except Exception as exc:  # noqa: BLE001 - the message is the result
            return f"{name} failed: {type(exc).__name__}"
    try:
        aligned = align(renders)
    except ValueError as exc:
        return str(exc)
    reference = aligned[REFERENCE]
    return {
        name: {
            "ssim": ssim(aligned[name], reference),
            "mad": mean_abs_diff(aligned[name], reference),
        }
        for name in LIBS
        if name != REFERENCE
    }


def run(corpus: str, sample_n: int, scale: float, fonts: str) -> dict:
    files = sample_files(corpus, sample_n)

    excluded: dict[str, str] = {}
    per_engine: dict[str, list[dict[str, float]]] = {
        name: [] for name in LIBS if name != REFERENCE
    }
    print(f"[fidelity] scoring first pages against {REFERENCE}:")
    for f in files:
        reason = certify(f, scale, fonts)
        if reason is not None:
            excluded[f] = reason
            continue
        scored = score_file(f, scale, fonts)
        if isinstance(scored, str):
            excluded[f] = scored
            continue
        for name, metrics in scored.items():
            per_engine[name].append(metrics)
        line = "  ".join(
            f"{name} ssim={metrics['ssim']:.4f}" for name, metrics in scored.items()
        )
        print(f"    {os.path.basename(f):20} {line}")

    compared = len(files) - len(excluded)
    print(
        f"[certify] {compared} of {len(files)} sampled files scored at"
        f" fonts={fonts}; {len(excluded)} excluded:"
    )
    for f, reason in excluded.items():
        print(f"    {os.path.basename(f):20} {reason}")
    if not compared:
        raise SystemExit("no file passed certification; nothing to score")

    engines = {}
    for name, rows in per_engine.items():
        ssims = sorted(round(row["ssim"], 4) for row in rows)
        mads = sorted(round(row["mad"], 4) for row in rows)
        engines[name] = {
            "ssim_median": round(float(np.median(ssims)), 4),
            "ssim_p10": round(float(np.percentile(ssims, 10)), 4),
            "mad_median": round(float(np.median(mads)), 4),
            "ssim": ssims,
            "mad": mads,
        }

    # Tally exclusions by their leading words only and record no file names:
    # the corpus is not public.
    reasons: dict[str, int] = {}
    for reason in excluded.values():
        key = reason.split(":")[0].lstrip("0123456789 ")
        if key.endswith("s skipped"):
            key = key.removesuffix("s skipped") + " skipped"
        reasons[key] = reasons.get(key, 0) + 1

    print(f"[fidelity] vs {REFERENCE} at scale {scale}, {WINDOW}x{WINDOW} SSIM window")
    for name, r in sorted(engines.items(), key=lambda kv: -kv[1]["ssim_median"]):
        print(
            f"    {name:14} ssim median {r['ssim_median']:.4f}"
            f"   p10 {r['ssim_p10']:.4f}   mad median {r['mad_median']:.2f}"
        )
    return {
        "corpus": os.path.basename(corpus.rstrip("/")),
        "files_sampled": len(files),
        "files_compared": compared,
        "excluded": reasons,
        "scale": scale,
        "fonts": fonts,
        "reference": REFERENCE,
        "ssim_window": WINDOW,
        "engines": engines,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("corpus", help="directory of .pdf files")
    ap.add_argument("--sample", type=int, default=40, help="files to sample")
    ap.add_argument("--scale", type=float, default=2.0, help="render scale factor")
    ap.add_argument(
        "--fonts",
        default="full",
        choices=("embedded-only", "all-embedded", "full"),
        help="pdfboss glyph-painting tier (full = substitute like the others)",
    )
    args = ap.parse_args()

    here = os.path.dirname(os.path.abspath(__file__))
    results = run(args.corpus, args.sample, args.scale, args.fonts)
    out = os.path.join(here, "results-fidelity.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=2)
    print(f"wrote {out}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        traceback.print_exc()
