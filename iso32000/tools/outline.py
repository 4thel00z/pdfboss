"""Derive `Iso32000/Outline.lean` from the text of ISO 32000-1:2008.

Usage:

    curl -sSLo PDF32000_2008.pdf \
        https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf
    pdfboss text PDF32000_2008.pdf > iso32000-1.txt
    python3 iso32000/tools/outline.py iso32000-1.txt > iso32000/Iso32000/Outline.lean

The body headings (`7.4.2 ASCIIHexDecode Filter`, `D.2 Latin Character Set
and Encodings`) and the first caption of every table (`Table 11 - Optional
parameters for the CCITTFaxDecode filter`) become Lean data. Table of
contents lines carry dot leaders and are skipped.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

CLAUSE_HEADING = re.compile(r"^(\d{1,2}(?:\.\d+)+)\s+([A-Z][^\n]*?)\s*$")
CHAPTER_TOC = re.compile(r"^(\d{1,2})\s+([A-Z][A-Za-z ,\-]+?)\s*(?:\. )+\.?\s*\d+\s*$")
ANNEX_HEADING = re.compile(r"^([A-L])\.(\d+(?:\.\d+)*)\s+([A-Z][^\n]*?)\s*$")
ANNEX_TITLE = re.compile(r"^Annex\s+([A-L])\s*$")
ANNEX_KIND = re.compile(r"^\((normative|informative)\)\s*$")
TABLE_CAPTION = re.compile(r"^Table (\d+) [–-]\s+(.*?)\s*$")
DOT_LEADER = re.compile(r"(?:\. ){3,}")
PAGE_HEADER_OR_FOOTER = re.compile(r"©|All rights reserved|^PDF 32000-1:2008")


@dataclass
class Heading:
    kind: str
    letter: str
    path: list[int]
    title: str
    line: int


@dataclass
class Table:
    number: int
    title: str
    within: Heading


@dataclass
class Outline:
    chapters: dict[int, str] = field(default_factory=dict)
    annexes: dict[str, str] = field(default_factory=dict)
    headings: list[Heading] = field(default_factory=list)
    tables: list[Table] = field(default_factory=list)


def clean_title(title: str) -> str:
    return re.sub(r"\s+", " ", title).strip()


def parse(lines: list[str]) -> Outline:
    outline = Outline()
    seen: set[tuple[str, str, tuple[int, ...]]] = set()
    seen_tables: set[int] = set()
    pending_annex = ""
    for number, raw in enumerate(lines, start=1):
        line = raw.rstrip("\n")
        if DOT_LEADER.search(line):
            chapter = CHAPTER_TOC.match(line)
            if chapter and int(chapter.group(1)) not in outline.chapters:
                outline.chapters[int(chapter.group(1))] = clean_title(chapter.group(2))
            continue
        annex_title = ANNEX_TITLE.match(line)
        if annex_title:
            pending_annex = annex_title.group(1)
            continue
        if pending_annex and (ANNEX_KIND.match(line) or PAGE_HEADER_OR_FOOTER.search(line)):
            continue
        if pending_annex and line.strip() and pending_annex not in outline.annexes:
            outline.annexes[pending_annex] = clean_title(line)
            pending_annex = ""
            continue
        pending_annex = ""
        table = TABLE_CAPTION.match(line)
        if table:
            table_number = int(table.group(1))
            if table_number in seen_tables or not outline.headings:
                continue
            seen_tables.add(table_number)
            outline.tables.append(Table(table_number, clean_title(table.group(2)), outline.headings[-1]))
            continue
        clause = CLAUSE_HEADING.match(line)
        if clause:
            path = tuple(int(part) for part in clause.group(1).split("."))
            key = ("clause", "", path)
            if key in seen or not 1 <= path[0] <= 14:
                continue
            seen.add(key)
            outline.headings.append(Heading("clause", "", list(path), clean_title(clause.group(2)), number))
            continue
        annex = ANNEX_HEADING.match(line)
        if not annex:
            continue
        path = tuple(int(part) for part in annex.group(2).split("."))
        key = ("annex", annex.group(1), path)
        if key in seen:
            continue
        seen.add(key)
        outline.headings.append(Heading("annex", annex.group(1), list(path), clean_title(annex.group(3)), number))
    return outline


def lean_string(text: str) -> str:
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def lean_ref(heading: Heading) -> str:
    path = "[" + ", ".join(str(part) for part in heading.path) + "]"
    if heading.kind == "clause":
        return f".clause {path}"
    return f".annex '{heading.letter}' {path}"


def render(outline: Outline) -> str:
    out: list[str] = []
    out.append("import Iso32000.Ref")
    out.append("")
    out.append("/-!")
    out.append("The clause outline of ISO 32000-1:2008, derived from the standard's text")
    out.append("by `tools/outline.py`. Regenerate rather than edit.")
    out.append("-/")
    out.append("")
    out.append("namespace Iso32000")
    out.append("")
    out.append("/-- Chapter titles by number. -/")
    out.append("def Outline.chapters : List (Nat × String) := [")
    out.append(",\n".join(f"  ({n}, {lean_string(t)})" for n, t in sorted(outline.chapters.items())))
    out.append("]")
    out.append("")
    out.append("/-- Annex titles by letter. -/")
    out.append("def Outline.annexes : List (Char × String) := [")
    out.append(",\n".join(f"  ('{letter}', {lean_string(t)})" for letter, t in sorted(outline.annexes.items())))
    out.append("]")
    out.append("")
    out.append("/-- Every numbered heading in the body of the standard, in document order. -/")
    out.append("def Outline.headings : List Heading := [")
    out.append(",\n".join(f"  ⟨{lean_ref(h)}, {lean_string(h.title)}⟩" for h in outline.headings))
    out.append("]")
    out.append("")
    out.append("/-- Every numbered table with the clause its caption appears in. -/")
    out.append("def Outline.tables : List Table := [")
    out.append(",\n".join(f"  ⟨{t.number}, {lean_ref(t.within)}⟩" for t in outline.tables))
    out.append("]")
    out.append("")
    out.append("end Iso32000")
    return "\n".join(out) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write("usage: outline.py <iso32000-1.txt>\n")
        return 2
    lines = Path(argv[1]).read_text(encoding="utf-8").splitlines()
    outline = parse(lines)
    sys.stdout.write(render(outline))
    sys.stderr.write(
        f"{len(outline.chapters)} chapters, {len(outline.annexes)} annexes, "
        f"{len(outline.headings)} headings, {len(outline.tables)} tables\n"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
