"""Tests for pdfboss.write.encrypt and pdfboss.write.decrypt: AES-256
protecting and removing protection from a document's fresh bytes."""

import pytest

import pdfboss
from pdfboss.write import Page, Pdf, Text, decrypt, encrypt


def build_pdf(*texts: str) -> bytes:
    pdf = Pdf()
    for text in texts:
        pdf = pdf | (Page(size="a4") | Text(text, at=(72, 700)))
    return pdf.to_bytes()


def test_encrypt_round_trips_under_the_user_password() -> None:
    data = build_pdf("secret contents")
    encrypted = encrypt(data, user_password="hunter2")
    assert encrypted != data
    doc = pdfboss.Document(data=encrypted, password="hunter2")
    assert "secret contents" in doc.extract_text()


def test_decrypt_returns_a_plainly_loadable_document() -> None:
    data = build_pdf("secret contents")
    encrypted = encrypt(data, user_password="hunter2")
    decrypted = decrypt(encrypted, password="hunter2")
    doc = pdfboss.Document(data=decrypted)
    assert "secret contents" in doc.extract_text()


def test_decrypt_raises_pdf_error_for_a_wrong_password() -> None:
    data = build_pdf("secret contents")
    encrypted = encrypt(data, user_password="hunter2")
    with pytest.raises(pdfboss.PdfError):
        decrypt(encrypted, password="wrong password")


def test_encrypt_rejects_an_unknown_allow_value() -> None:
    data = build_pdf("secret contents")
    with pytest.raises(ValueError, match="bogus"):
        encrypt(data, user_password="hunter2", allow=["bogus"])


def test_encrypt_requires_at_least_one_password() -> None:
    data = build_pdf("secret contents")
    with pytest.raises(ValueError, match="user_password or owner_password"):
        encrypt(data)


def test_encrypt_accepts_only_an_owner_password() -> None:
    data = build_pdf("secret contents")
    encrypted = encrypt(data, owner_password="ownersecret")
    doc = pdfboss.Document(data=encrypted)
    assert "secret contents" in doc.extract_text()
