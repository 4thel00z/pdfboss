"""Markdown composed into themed PDFs."""

from pdfboss._pdfboss import md_to_pdf


def to_pdf(
    markdown: str,
    theme: str | None = None,
    size: str = "a4",
    landscape: bool = False,
    base_dir: str | None = None,
) -> bytes:
    """Composes CommonMark+GFM markdown into a PDF and returns the file bytes.

    theme is CSS source text; base_dir anchors relative image paths.
    """
    return md_to_pdf(markdown, theme, size, landscape, base_dir)
