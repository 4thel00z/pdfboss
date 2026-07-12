//! Writes the committed test fixtures into `tests/fixtures/` at the
//! workspace root: `hello.pdf`, `three-pages.pdf`, `shapes.pdf`, and
//! `xref-stream.pdf` (object-stream + xref-stream variant of hello).

use std::path::Path;

use pdfboss_testkit::{doc_with_graphics, multi_page_doc, objstm_payload, simple_doc, PdfBuilder};

/// Three filled rectangles in different RGB colors, a stroked Bezier path,
/// and a q/Q-wrapped `cm` transform block.
const SHAPES_CONTENT: &str = "\
1 0 0 rg 72 600 100 80 re f
0 0.5 1 rg 200 600 120 60 re f
0.2 0.8 0.2 rg 340 590 90 90 re f
0 0 0 RG 2 w
100 300 m 150 400 250 400 300 300 c S
q
0.5 0 0 0.5 300 100 cm
0.8 0 0.8 rg
0 0 200 200 re f
Q";

/// The hello fixture rebuilt with its non-stream objects packed into a
/// `/Type /ObjStm` object stream and located via a `/Type /XRef`
/// cross-reference stream.
fn xref_stream_hello() -> Vec<u8> {
    let (dict, payload) = objstm_payload(&[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        ),
        (
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /Encoding /WinAnsiEncoding >>",
        ),
    ]);
    let mut b = PdfBuilder::new();
    b.stream(4, "", b"BT /F1 12 Tf 72 720 Td (Hello, world!) Tj ET");
    b.stream(6, &dict, &payload);
    b.build_xref_stream(1)
}

fn main() -> std::io::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    std::fs::create_dir_all(&dir)?;

    let fixtures: [(&str, Vec<u8>); 4] = [
        ("hello.pdf", simple_doc("Hello, world!")),
        (
            "three-pages.pdf",
            multi_page_doc(&["Page one", "Page two", "Page three"]),
        ),
        ("shapes.pdf", doc_with_graphics(SHAPES_CONTENT)),
        ("xref-stream.pdf", xref_stream_hello()),
    ];
    for (name, bytes) in fixtures {
        let path = dir.join(name);
        std::fs::write(&path, &bytes)?;
        println!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}
