use pdfboss_core::Document;
use pdfboss_markdown::{to_pdf, Options, Theme};
use pdfboss_render::{render_page_reporting, RenderOptions};

const SAMPLE: &str = "\
# Title\n\n\
A paragraph with **bold**, *italic*, `code` and a [link](https://example.com).\n\n\
- first\n- second\n  - nested\n\n\
1. one\n2. two\n\n\
> a quote\n\n\
    ```\nlet x = 1;\n```\n\n\
| h1 | h2 |\n|---|---|\n| a | b |\n\n\
---\n\n\
The end.\n";

#[test]
fn structure_survives_the_round_trip() {
    let (pdf, report) = to_pdf(SAMPLE, &Options::default()).unwrap();
    assert!(report.is_empty(), "{}", report.summary());
    let bytes = pdf.to_bytes().unwrap();
    let doc = Document::load(bytes).unwrap();
    let (md, _) =
        pdfboss_output::extract_markdown_reporting(&doc, pdfboss_output::ReadingOrder::Content)
            .unwrap();
    assert!(md.contains("Title"));
    assert!(md.contains("bold"));
    assert!(md.contains("let x = 1;"));
    assert!(md.contains("first"));
    assert!(md.contains("The end."));
}

#[test]
fn output_is_deterministic() {
    let first = to_pdf(SAMPLE, &Options::default())
        .unwrap()
        .0
        .to_bytes()
        .unwrap();
    let second = to_pdf(SAMPLE, &Options::default())
        .unwrap()
        .0
        .to_bytes()
        .unwrap();
    assert_eq!(first, second);
}

#[test]
fn links_reach_the_written_file() {
    // The annotation dict is packed into a compressed object stream under
    // the default write options, so its bytes never appear literally in
    // the file; read it back through the toolkit's own document model
    // instead, resolving /Annots -> Annot dict -> /A -> /URI.
    let (pdf, _) = to_pdf("[docs](https://example.com/x)\n", &Options::default()).unwrap();
    let bytes = pdf.to_bytes().unwrap();
    let doc = Document::load(bytes).unwrap();
    let page = doc.page(0).unwrap();
    let annots = page.dict().get_array("Annots").unwrap_or(&[]);
    let uris: Vec<Vec<u8>> = annots
        .iter()
        .filter_map(|annot| doc.resolve(annot).ok())
        .filter_map(|annot| annot.as_dict().cloned())
        .filter_map(|annot| annot.get_dict("A").cloned())
        .filter_map(|action| action.get("URI").cloned())
        .filter_map(|uri| uri.as_str_bytes().map(<[u8]>::to_vec))
        .collect();
    assert_eq!(uris, vec![b"https://example.com/x".to_vec()]);
}

#[test]
fn themed_page_renders_with_painted_regions() {
    let theme = Theme::parse("pre { background-color: #202020; }").unwrap();
    let options = Options {
        theme,
        ..Options::default()
    };
    let (pdf, _) = to_pdf("```\ncode\n```\n", &options).unwrap();
    let bytes = pdf.to_bytes().unwrap();
    let doc = Document::load(bytes).unwrap();
    let page = doc.page(0).unwrap();
    let (pix, report) = render_page_reporting(&doc, &page, 1.0, &RenderOptions::default()).unwrap();
    let mut dark = 0usize;
    for pixel in pix.data.as_chunks::<4>().0 {
        if pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 100 {
            dark += 1;
        }
    }
    assert!(
        dark > 100,
        "code background painted: {dark} dark pixels, skipped: {:?}",
        report.summary()
    );
}

#[test]
fn replacement_report_reaches_the_caller() {
    let (_, report) = to_pdf("emoji 🎉 here\n", &Options::default()).unwrap();
    assert_eq!(report.replaced.get(&'🎉'), Some(&1));
}

#[test]
fn empty_markdown_still_yields_one_page() {
    let (pdf, _) = to_pdf("", &Options::default()).unwrap();
    assert_eq!(pdf.pages.len(), 1);
    pdf.to_bytes().unwrap();
}
