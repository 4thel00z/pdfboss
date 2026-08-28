import warnings

import pytest

import pdfboss


def test_to_pdf_returns_pdf_bytes() -> None:
    data = pdfboss.md.to_pdf("# Title\n\nhello **world**\n")
    assert data.startswith(b"%PDF-")


def test_to_pdf_is_deterministic() -> None:
    md = "# Same\n\ninput\n"
    assert pdfboss.md.to_pdf(md) == pdfboss.md.to_pdf(md)


def test_theme_errors_raise() -> None:
    with pytest.raises(pdfboss.PdfError, match="12px"):
        pdfboss.md.to_pdf("x\n", theme="p { font-size: 12px; }")


def test_unknown_size_raises() -> None:
    with pytest.raises(pdfboss.PdfError, match="tabloid"):
        pdfboss.md.to_pdf("x\n", size="tabloid")


def test_replacements_warn() -> None:
    with pytest.warns(UserWarning, match="replaced"):
        pdfboss.md.to_pdf("emoji \U0001f389\n")


def test_clean_input_does_not_warn() -> None:
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        pdfboss.md.to_pdf("plain text\n")
