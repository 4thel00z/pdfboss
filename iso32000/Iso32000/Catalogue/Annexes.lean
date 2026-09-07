import Iso32000.Feature

/-!
Ledger rows for the normative annexes of ISO 32000-1.
-/

namespace Iso32000.Catalogue

def annexes : List Feature := [
  { ref := .annex 'A' [2], title := "PDF Content Stream Operators", status := .implemented,
    note := "Every operator in Table A.1 parses into an Op (BI, ID and EI become one InlineImage op; f and F share Op::Fill) and none is unparsed; the rasterizer treats ri and i as an explicit no-op (executor.rs) and lets MP, DP, BX, EX, ET and d0 fall to the empty arm at executor.rs, while d1 is only checked for presence at executor.rs to choose the uncolored Type3 glyph path." },
  { ref := .annex 'B' [2], title := "Arithmetic Operators", status := .implemented,
    note := "All 21 arithmetic operators (abs add atan ceiling cos cvi cvr div exp floor idiv ln log mod mul neg round sin sqrt sub truncate) are recognized at shading.rs and evaluated by run_calculator; the tests feed add, mul, exp, log, sqrt, div, ln, atan, idiv, mod, round, truncate, floor, ceiling and cvi, leaving abs, neg, sub, sin, cos and cvr without a direct test." },
  { ref := .annex 'B' [3], title := "Relational, Boolean and Bitwise Operators", status := .implemented,
    note := "and bitshift eq false ge gt le lt ne not or true xor are all recognized; and, or, xor and not split on boolean versus integer operands, bitshift shifts in both directions, and eq/ne handle mixed-type operands." },
  { ref := .annex 'B' [4], title := "Conditional Operators", status := .implemented,
    note := "if and ifelse compile procedure blocks into forward jumps, nested conditionals branch correctly, and a block not followed by if or ifelse is a load error (shading.rs)." },
  { ref := .annex 'B' [5], title := "Stack Operators", status := .implemented,
    note := "copy dup exch index pop roll are recognized, roll rotates in both directions, and the operand stack is capped at 100 entries (CALC_STACK, shading.rs) as §7.10.5.1 requires, with a 10,000-step runaway limit." },
  { ref := .annex 'C' [2], title := "Architectural Limits", status := .implemented,
    note := "The parser meets or exceeds every Table C.1 minimum (integers are i64 and reals f64 with integer overflow degrading to a real, no cap on string, name, array or dictionary size, CID range 0 to 0xFFFF) and sets its own caps above the spec's minima: object nesting 128 (spec 28), q/Q depth 64 (spec 28), indirect-reference chain 32 (MAX_RESOLVE_DEPTH), page-tree depth 256, form nesting 16; it never rejects a file for exceeding Table C.1." },
  { ref := .annex 'C' [3], title := "Memory Limits", status := .implemented,
    note := "Memory is bounded per stream rather than per document: each decoded stream is capped at 256 MiB (MAX_DECODED_LEN) so a decompression bomb is an error, CCITT checks its packed size against that cap and JBIG2 budgets its regions up front, and the aio tail scan stops at 64 KiB; there is no document-wide object-count or total-memory cap." },
  { ref := .annex 'D' [2], title := "Latin Character Set and Encodings", status := .incomplete,
    note := "The StandardEncoding, MacRomanEncoding and WinAnsiEncoding columns exist as code-to-Unicode tables (glyph-name tables only for Standard and WinAnsi), the PDFDocEncoding column decodes text strings (crates/pdfboss-core/src/object.rs pdf_doc_char) and pdfboss-write encodes text as WinAnsi; missing are a MacRoman glyph-name table, the WinAnsi footnote mapping unused codes above 0x27 to bullet (0x81, 0x8D, 0x8F, 0x90 and 0x9D return None), and /StandardEncoding named explicitly is only honored because it falls through the default arm at font.rs." },
  { ref := .annex 'D' [3], title := "PDFDocEncoding Character Set", status := .implemented,
    note := "The full PDFDocEncoding table decodes text strings without a BOM (crates/pdfboss-core/src/object.rs pdf_doc_char: accents at 0x18-0x1F, punctuation and ligatures at 0x80-0x9E, Euro at 0xA0, undefined codes to U+FFFD); the writer emits text strings as ASCII or UTF-16BE, which the clause allows, so no encoder is needed." },
  { ref := .annex 'D' [4], title := "Expert Set and MacExpertEncoding", status := .notImplemented,
    note := "No MacExpertEncoding table exists anywhere; a font naming /MacExpertEncoding falls through the default arm at crates/pdfboss-text/src/font.rs and is decoded as StandardEncoding." },
  { ref := .annex 'D' [5], title := "Symbol Set and Encoding", status := .implemented,
    note := "crates/pdfboss-encoding/src/symbol.rs SYMBOL holds the 189 code-to-glyph-name pairs of the annex (no apple at 0o360), written by a script from the standard's text and cross-checked against unicode.org's Adobe Symbol mapping; lib.rs symbol_glyph_name and symbol expose them, the character coming through the Adobe Glyph List, so Omega, Delta and mu read as the ohm, increment and micro signs as pdf.js reads them (test lib.rs symbol_encoding_follows_annex_d5). Text extraction takes the table as the base encoding of a font whose /BaseFont family is Symbol when the file names no encoding and embeds no Type 1 program, /Differences on top (crates/pdfboss-text/src/font.rs standard_symbol_encoding, test symbol_and_zapf_dingbats_decode_through_their_built_in_encodings); the writer encodes Symbol text through the same table (crates/pdfboss-write/src/font.rs Standard14::decoder, test symbol_faces_encode_through_their_built_in_encodings). Rendering still leaves non-embedded Symbol unpainted, there being no license-clean substitute face (crates/pdfboss-render/src/substitute.rs), and no metrics are bundled for it (9.6.2.2)." },
  { ref := .annex 'D' [6], title := "ZapfDingbats Set and Encoding", status := .implemented,
    note := "crates/pdfboss-encoding/src/symbol.rs ZAPF_DINGBATS holds the 188 codes of the annex (space and 187 dingbats; the annex leaves 0o200 to 0o215 out) with their glyph names and, since the Adobe Glyph List does not carry the a1 to a191 names, the Unicode characters of unicode.org's Adobe ZapfDingbats mapping; lib.rs zapf_dingbats_glyph_name and zapf_dingbats expose them (test lib.rs zapf_dingbats_encoding_follows_annex_d6). Text extraction and the writer use the table the same way as Symbol's (crates/pdfboss-text/src/font.rs standard_symbol_encoding, crates/pdfboss-write/src/font.rs Standard14::decoder). Rendering still leaves non-embedded ZapfDingbats unpainted and no metrics are bundled for it (9.6.2.2)." },
  { ref := .annex 'E' [2], title := "Name Registry", status := .outOfScope,
    note := "pdfboss neither registers names nor checks second-class or third-class prefixes; unknown dictionary keys pass through as plain names, and registration is a publishing process rather than a parser feature." },
  { ref := .annex 'F' [3], title := "Linearized PDF Document Structure", status := .notImplemented,
    note := "No crate reads the linearization parameter dictionary: pdfboss-aio locates objects by scanning the tail for startxref and following the xref chain (crates/pdfboss-aio/src/document.rs find_tail) and fetches byte ranges per object, and pdfboss-write emits no /Linearized dictionary or first-page section (crates/pdfboss-write/src/writer.rs writes a plain header)." },
  { ref := .annex 'F' [4], title := "Hint Tables", status := .notImplemented,
    note := "No hint stream is parsed or produced; the /H offsets and the page offset and shared object hint tables are never read." },
  { ref := .annex 'I' [], title := "PDF Versions and Compatibility", status := .incomplete,
    note := "The %PDF-M.m header is parsed within the first 1 KiB with one to three digit components, a missing or malformed header silently defaults to 1.4, any parsed version including future ones is accepted without a warning, unknown operators and keys are skipped as Annex I asks, and pdfboss-write emits a configurable header; missing is the catalog /Version override, which no crate reads." },
  { ref := .annex 'F' [2], title := "Background and Assumptions", status := .outOfScope,
    note := "Design background for linearization with no requirement of its own; the file structure it motivates is assessed under F.3 and F.4." }
]

end Iso32000.Catalogue
