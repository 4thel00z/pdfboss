"""Tests for pdfboss.write's assembly commands: merge, split, rotate and
rewrite. Thin bytes-in/bytes-out wrappers over the underlying library
functions, with 0-based page lists throughout (the 1-based convention is
CLI-only)."""

import asyncio
from collections.abc import AsyncIterator

import pytest

import pdfboss
from pdfboss.write import (
    Page,
    Pdf,
    SplitParts,
    Text,
    Update,
    encrypt,
    merge,
    rewrite,
    rotate,
    split,
    split_parts,
    split_stream,
)


def build_pdf(*texts: str) -> bytes:
    pdf = Pdf()
    for text in texts:
        pdf = pdf | (Page(size="a4") | Text(text, at=(72, 700)))
    return pdf.to_bytes()


def page_texts(data: bytes) -> list[str]:
    doc = pdfboss.Document(data=data)
    return [doc[i].extract_text() for i in range(doc.page_count)]


def xref_section_count(data: bytes) -> int:
    doc = pdfboss.Document(data=data)
    return sum(1 for element in doc.elements(physical=True, logical=False) if element.kind == "xref")


def test_merge_gathers_every_source_page_in_argument_order() -> None:
    a = build_pdf("a1", "a2")
    b = build_pdf("b1", "b2")
    merged = merge([a, b])
    assert pdfboss.Document(data=merged).page_count == 4
    texts = page_texts(merged)
    assert "a1" in texts[0]
    assert "a2" in texts[1]
    assert "b1" in texts[2]
    assert "b2" in texts[3]


def test_merge_selects_and_reorders_pages() -> None:
    a = build_pdf("one", "two", "three")
    merged = merge([(a, [2, 0])])
    texts = page_texts(merged)
    assert len(texts) == 2
    assert "three" in texts[0]
    assert "one" in texts[1]


def test_merge_mixes_whole_and_selected_inputs() -> None:
    a = build_pdf("one", "two")
    b = build_pdf("three")
    merged = merge([(a, [1]), b])
    texts = page_texts(merged)
    assert len(texts) == 2
    assert "two" in texts[0]
    assert "three" in texts[1]


def test_merge_rejects_a_negative_page_index_by_value() -> None:
    a = build_pdf("one", "two")
    with pytest.raises(ValueError, match="non-negative, got -1"):
        merge([(a, [0, -1])])


def test_merge_rejects_a_wrong_typed_input_by_type() -> None:
    with pytest.raises(TypeError, match=r"bytes or \(bytes, list\[int\]\), got int"):
        merge([7])


def test_split_round_trips_page_counts() -> None:
    data = build_pdf("one", "two", "three")
    parts = split(data, 2)
    assert len(parts) == 2
    assert pdfboss.Document(data=parts[0]).page_count == 2
    assert pdfboss.Document(data=parts[1]).page_count == 1


def test_split_parts_yields_the_same_parts_as_split() -> None:
    data = build_pdf("one", "two", "three", "four", "five")
    assert list(split_parts(data, 2)) == split(data, 2)


@pytest.mark.parametrize("every", [0, -1])
def test_split_and_split_parts_reject_every_below_one_with_value_error(every: int) -> None:
    data = build_pdf("one")
    with pytest.raises(ValueError, match="every"):
        split(data, every)
    with pytest.raises(ValueError, match="every"):
        split_parts(data, every)


def test_split_parts_raises_pdf_error_from_the_first_part() -> None:
    parts = split_parts(b"not a pdf", 1)
    with pytest.raises(pdfboss.PdfError):
        next(parts)
    assert list(parts) == []


def test_split_parts_takes_password_by_keyword_only() -> None:
    with pytest.raises(TypeError):
        split_parts(build_pdf("one"), 1, "secret")  # type: ignore[misc]


def test_split_parts_reports_its_public_module() -> None:
    assert SplitParts.__module__ == "pdfboss.write"


def test_split_parts_opens_a_protected_input_and_yields_plain_parts() -> None:
    data = build_pdf("one", "two", "three")
    locked = encrypt(data, user_password="secret")
    parts = list(split_parts(locked, 2, password="secret"))
    assert [page_texts(part) for part in parts] == [page_texts(part) for part in split(data, 2)]
    assert all(b"/Encrypt" not in part for part in parts)


@pytest.mark.parametrize("password", ["wrong", ""])
def test_split_parts_refuses_a_wrong_or_missing_password_from_the_first_part(
    password: str,
) -> None:
    locked = encrypt(build_pdf("one"), user_password="secret")
    parts = split_parts(locked, 1, password=password)
    with pytest.raises(pdfboss.PdfError):
        next(parts)


@pytest.mark.asyncio
async def test_split_parts_iterates_asynchronously() -> None:
    data = build_pdf("one", "two", "three")
    parts = [part async for part in split_parts(data, 2)]
    assert parts == split(data, 2)


def many_pages() -> bytes:
    return build_pdf(*[f"page{index}" for index in range(8)])


@pytest.mark.asyncio
async def test_a_cancelled_step_loses_no_part() -> None:
    data = many_pages()
    parts = split_parts(data, 1)
    step = asyncio.ensure_future(anext(parts))
    await asyncio.sleep(0)
    step.cancel()
    with pytest.raises(asyncio.CancelledError):
        await step
    assert [part async for part in parts] == split(data, 1)


@pytest.mark.asyncio
async def test_a_timed_out_step_loses_no_part() -> None:
    data = many_pages()
    parts = split_parts(data, 1)
    with pytest.raises(TimeoutError):
        await asyncio.wait_for(anext(parts), timeout=0)
    assert [part async for part in parts] == split(data, 1)


@pytest.mark.asyncio
async def test_a_step_never_awaited_takes_no_part() -> None:
    data = many_pages()
    parts = split_parts(data, 1)
    anext(parts).close()
    assert [part async for part in parts] == split(data, 1)


@pytest.mark.asyncio
async def test_overlapping_steps_are_refused_so_parts_stay_in_order() -> None:
    data = many_pages()
    parts = split_parts(data, 1)
    first = asyncio.ensure_future(anext(parts))
    second = asyncio.ensure_future(anext(parts))
    with pytest.raises(RuntimeError, match="already advancing"):
        await second
    assert await first == split(data, 1)[0]


async def chunked(data: bytes, size: int) -> AsyncIterator[bytes]:
    for start in range(0, len(data), size):
        yield data[start : start + size]


async def counted(data: bytes, pulled: list[int]) -> AsyncIterator[bytes]:
    for start in range(0, len(data), 4):
        pulled.append(start)
        yield data[start : start + 4]


@pytest.mark.asyncio
@pytest.mark.parametrize("size", [1, 7, 4096, 1 << 20])
async def test_split_stream_matches_split_for_any_chunk_size(size: int) -> None:
    data = build_pdf("one", "two", "three", "four", "five")
    parts = [part async for part in split_stream(chunked(data, size), 2)]
    assert parts == split(data, 2)
    assert "five" in page_texts(parts[2])[0]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("every", "password", "error"),
    [(0, "", ValueError), (-1, "", ValueError), (2.0, "", TypeError), (1, None, TypeError)],
)
async def test_split_stream_checks_its_arguments_before_reading(
    every: object, password: object, error: type[Exception]
) -> None:
    pulled: list[int] = []
    stream = split_stream(counted(build_pdf("one"), pulled), every, password=password)  # type: ignore[arg-type]
    with pytest.raises(error):
        await anext(stream)
    assert pulled == []


@pytest.mark.asyncio
async def test_split_stream_opens_a_protected_input_with_its_password() -> None:
    data = build_pdf("one", "two", "three")
    locked = encrypt(data, user_password="secret")
    parts = [part async for part in split_stream(chunked(locked, 64), 2, password="secret")]
    assert [page_texts(part) for part in parts] == [page_texts(part) for part in split(data, 2)]


def test_rotate_append_prefixes_the_input_and_updates_rotation() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 90, pages=[0])
    assert rotated.startswith(data)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 90
    assert doc[1].rotation == 0


def test_rotate_rewrite_updates_rotation_without_prefixing_the_input() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 90, pages=[0], rewrite=True)
    assert not rotated.startswith(data)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 90
    assert doc[1].rotation == 0


def test_rotate_defaults_to_every_page() -> None:
    data = build_pdf("one", "two")
    rotated = rotate(data, 180)
    doc = pdfboss.Document(data=rotated)
    assert doc[0].rotation == 180
    assert doc[1].rotation == 180


def test_rotate_rejects_an_unsupported_angle() -> None:
    data = build_pdf("one")
    with pytest.raises(ValueError, match="90, 180 or 270"):
        rotate(data, 45)


def test_rewrite_collapses_an_appended_update_chain() -> None:
    base = build_pdf("one")
    update = Update(pdfboss.Document(data=base))
    update.set_metadata(title="Chained")
    appended = update.to_bytes()
    assert xref_section_count(appended) == 2

    rewritten = rewrite(appended)
    assert xref_section_count(rewritten) == 1
    assert pdfboss.Document(data=rewritten).metadata.get("title") == "Chained"
