"""pdfboss Markdown adapter for the opendataloader-bench runner.

Copy this file into the benchmark's ``src/`` directory and register the
engine as described in this directory's README.
"""

from pathlib import Path

import pdfboss


def to_markdown(doc_paths, input_path, output_dir):
    output_dir = Path(output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    for doc_path in doc_paths:
        markdown = pdfboss.Document(str(doc_path)).extract_markdown()
        output_file = output_dir / f"{Path(doc_path).stem}.md"
        output_file.write_text(markdown, encoding="utf-8")
