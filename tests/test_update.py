"""Tests for `pdfboss.write.Update`: a metadata edit staged over an
existing document and serialized as an incremental append, leaving the
base document's own bytes untouched."""

from pathlib import Path

import pdfboss
from pdfboss.write import Metadata, Page, Pdf, Text, Update


def build_base(path: Path) -> None:
    page = Page(size="a4") | Text("Body copy", at=(72, 700))
    pdf = Pdf() | page | Metadata(title="Old", author="Keep")
    path.write_bytes(pdf.to_bytes())


def test_update_appends_and_preserves_bytes(tmp_path: Path) -> None:
    src = tmp_path / "src.pdf"
    build_base(src)

    update = Update(pdfboss.Document(str(src)))
    update.set_metadata(title="New")
    dst = tmp_path / "dst.pdf"
    update.save_appended(str(dst))

    assert dst.read_bytes().startswith(src.read_bytes())
    reread = pdfboss.Document(str(dst))
    assert reread.metadata["title"] == "New"
    assert reread.metadata["author"] == "Keep"


def test_update_to_bytes_matches_saved_file(tmp_path: Path) -> None:
    src = tmp_path / "src.pdf"
    build_base(src)

    update = Update(pdfboss.Document(str(src)))
    update.set_metadata(title="New")
    data = update.to_bytes()
    dst = tmp_path / "dst.pdf"
    update.save_appended(str(dst))

    assert data == dst.read_bytes()
