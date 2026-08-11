"""pdfboss plain-text adapter for the opendataloader-bench runner.

The default `extract_text` output scored as if it were Markdown: it earns
nothing on headings or table structure, so its only competitive number is
NID, the reading-order metric. That is the second pdfboss row of the
top-level README's quality table.
"""

from pathlib import Path

import pdfboss


def to_markdown(doc_paths, input_path, output_dir):
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    for doc_path in doc_paths:
        text = pdfboss.Document(str(doc_path)).extract_text()
        output_file = output_dir / f"{Path(doc_path).stem}.md"
        output_file.write_text(text, encoding="utf-8")
