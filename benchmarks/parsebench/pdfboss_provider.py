"""Provider for pdfboss PARSE.

Drop this file into a run-llama/ParseBench checkout as
``src/parse_bench/inference/providers/parse/pdfboss.py`` and register it as
described in this directory's README.
"""

from datetime import datetime
from pathlib import Path
from typing import Any

from parse_bench.inference.providers.base import (
    Provider,
    ProviderConfigError,
    ProviderPermanentError,
)
from parse_bench.inference.providers.registry import register_provider
from parse_bench.schemas.parse_output import PageIR, ParseOutput
from parse_bench.schemas.pipeline import PipelineSpec
from parse_bench.schemas.pipeline_io import (
    InferenceRequest,
    InferenceResult,
    RawInferenceResult,
)
from parse_bench.schemas.product import ProductType


def convert_md_tables_to_html(content: str) -> str:
    """
    Convert markdown pipe tables to HTML tables.

    The Tables dimension's similarity metrics (TEDS/GriTS/TableRecordMatch)
    consume HTML ``<table>`` markup only, while the rule metrics parse both
    forms — the same normalization the pdf_inspector provider applies.
    """
    import markdown2

    parts: list[str] = []
    table_lines: list[str] = []

    def flush() -> None:
        nonlocal table_lines
        lines, table_lines = table_lines, []
        if len(lines) < 2:
            parts.extend(lines)
            return
        html = markdown2.markdown("\n".join(lines), extras=["tables"]).strip()
        if "<table>" not in html.lower():
            parts.extend(lines)
            return
        parts.append(html)

    for line in content.split("\n"):
        if line.strip().startswith("|"):
            table_lines.append(line)
            continue
        flush()
        parts.append(line)
    flush()
    return "\n".join(parts)


@register_provider("pdfboss")
class PdfbossProvider(Provider):
    """
    Provider for pdfboss PARSE.

    Extracts per-page markdown from the embedded text layer using the
    pdfboss library (Rust engine, Python bindings): ATX headings ranked by
    font size, lists, and pipe tables inferred from layout. No OCR.
    """

    def extract_markdown_pages(self, pdf_path: str) -> dict[str, Any]:
        """
        Extract per-page markdown from a PDF using pdfboss.

        :param pdf_path: Path to the PDF file
        :return: Raw extraction result with page-level markdown
        :raises ProviderError: For any extraction errors
        """
        try:
            import pdfboss
        except ImportError as e:
            raise ProviderConfigError("pdfboss package not installed. Run: pip install pdfboss") from e

        try:
            doc = pdfboss.Document(pdf_path)
        except Exception as e:
            raise ProviderPermanentError(f"Cannot read PDF: {e}") from e

        pages: list[dict[str, Any]] = []
        for page_index in range(doc.page_count):
            try:
                markdown = doc[page_index].extract_markdown()
            except Exception as e:
                pages.append({"page_index": page_index, "text": "", "error": str(e)})
                continue
            pages.append({"page_index": page_index, "text": markdown})

        return {
            "pages": pages,
            "num_pages": doc.page_count,
            "metadata": doc.metadata,
        }

    def run_inference(self, pipeline: PipelineSpec, request: InferenceRequest) -> RawInferenceResult:
        """
        Run inference and return raw results.

        :param pipeline: Pipeline specification
        :param request: Inference request
        :return: Raw inference result
        :raises ProviderError: For any provider-related failures
        """
        if request.product_type != ProductType.PARSE:
            raise ProviderPermanentError(
                f"PdfbossProvider only supports PARSE product type, got {request.product_type}"
            )

        pdf_path = Path(request.source_file_path)
        if pdf_path.suffix.lower() != ".pdf":
            raise ProviderPermanentError(f"PdfbossProvider only supports .pdf files, got {pdf_path.suffix}")

        if not pdf_path.exists():
            raise ProviderPermanentError(f"PDF file not found: {pdf_path}")

        started_at = datetime.now()

        try:
            raw_output = self.extract_markdown_pages(str(pdf_path))
        except (ProviderPermanentError, ProviderConfigError):
            raise
        except Exception as e:
            raise ProviderPermanentError(f"Unexpected error during inference: {e}") from e

        completed_at = datetime.now()
        latency_ms = int((completed_at - started_at).total_seconds() * 1000)

        return RawInferenceResult(
            request=request,
            pipeline=pipeline,
            pipeline_name=pipeline.pipeline_name,
            product_type=request.product_type,
            raw_output=raw_output,
            started_at=started_at,
            completed_at=completed_at,
            latency_in_ms=latency_ms,
        )

    def normalize(self, raw_result: RawInferenceResult) -> InferenceResult:
        """
        Normalize raw inference result to produce ParseOutput.

        :param raw_result: Raw inference result from run_inference()
        :return: Inference result with both raw and normalized outputs
        :raises ProviderError: For any normalization failures
        """
        if raw_result.product_type != ProductType.PARSE:
            raise ProviderPermanentError(
                f"PdfbossProvider only supports PARSE product type, got {raw_result.product_type}"
            )

        pages: list[PageIR] = []
        page_texts: list[str] = []
        for page_data in raw_result.raw_output.get("pages", []):
            page_index = page_data.get("page_index", 0)
            text = convert_md_tables_to_html(page_data.get("text", ""))
            pages.append(PageIR(page_index=page_index, markdown=text))
            page_texts.append(text)

        output = ParseOutput(
            task_type="parse",
            example_id=raw_result.request.example_id,
            pipeline_name=raw_result.pipeline_name,
            pages=pages,
            markdown="\n\n".join(page_texts),
        )

        return InferenceResult(
            request=raw_result.request,
            pipeline_name=raw_result.pipeline_name,
            product_type=raw_result.product_type,
            raw_output=raw_result.raw_output,
            output=output,
            started_at=raw_result.started_at,
            completed_at=raw_result.completed_at,
            latency_in_ms=raw_result.latency_in_ms,
        )
