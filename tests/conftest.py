"""Shared fixtures for the pdfboss pytest suite."""

from pathlib import Path

import pytest

FIXTURES = Path(__file__).parent / "fixtures"


@pytest.fixture
def fixtures_dir() -> Path:
    """Directory containing the committed fixture PDFs."""
    return FIXTURES


@pytest.fixture
def hello_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "hello.pdf"


@pytest.fixture
def three_pages_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "three-pages.pdf"


@pytest.fixture
def shapes_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "shapes.pdf"


@pytest.fixture
def xref_stream_pdf(fixtures_dir: Path) -> Path:
    return fixtures_dir / "xref-stream.pdf"
