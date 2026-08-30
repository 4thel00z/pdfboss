//! Full-frame snapshot tests on a fixed 80x24 TestBackend: tree render,
//! inspector dict, hex pane and status bar over a testkit fixture.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pdfboss_core::elements::{Element, ElementOpts, Span};
use pdfboss_core::{Document, ObjRef, Object};
use pdfboss_tui::app::{App, Msg};
use pdfboss_tui::tree::TreeReq;
use pdfboss_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use ratatui::Terminal;

fn key(code: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn buffer_lines(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn assert_frame(terminal: &Terminal<TestBackend>, expected: &[&str]) {
    let lines = buffer_lines(terminal);
    assert_eq!(lines.len(), expected.len(), "frame height");
    for (index, want) in expected.iter().enumerate() {
        assert_eq!(
            lines[index].trim_end(),
            want.trim_end(),
            "frame line {index}"
        );
    }
}

fn draw(app: &App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    terminal.draw(|frame| ui::draw(app, frame)).expect("draw");
    terminal
}

/// Frame A: document overview after the physical pass — tree with counts,
/// document summary in the inspector, empty hex pane, breadcrumb status.
#[test]
fn document_overview_frame() {
    let data = pdfboss_testkit::simple_doc("Hello");
    let doc = Document::load(data).expect("fixture loads");
    let elements: Vec<Element> = doc
        .elements(ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        })
        .filter_map(Result::ok)
        .collect();
    let mut app = App::new(
        "fixture.pdf".to_string(),
        "fixture.pdf".to_string(),
        doc.version(),
        doc.page_count(),
        (80, 24),
    );
    app.update(Msg::TreeBatch {
        req: TreeReq::Physical,
        elements,
        errors: 0,
        done: true,
    });
    let terminal = draw(&app);
    assert_frame(
        &terminal,
        &[
            "┌Tree──────────────────────┐┌Inspector · Document──────────────────────────────┐",
            "│▾ Document · PDF 1.7      ││version: 1.7                                      │",
            "│  ▸ Pages (1)             ││pages: 1                                          │",
            "│  ▸ Objects (5)           ││                                                  │",
            "│  ▸ Xref (1 secs)         ││                                                  │",
            "│    Trailer               ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          │└──────────────────────────────────────────────────┘",
            "│                          │┌Hex───────────────────────────────────────────────┐",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "└──────────────────────────┘└──────────────────────────────────────────────────┘",
            "fixture.pdf · /Document · [/] search  [p] preview  [m] markdown  [q] quit",
        ],
    );
}

/// Frame C: an object selected — expanded tree, catalog dict in the
/// inspector, its bytes hexdumped, breadcrumb status. The element batch is
/// hand-built with test-chosen spans (0..15 header, 15..64 object 1) so the
/// hex gutter is static; the dumped bytes are the real fixture bytes.
#[test]
fn object_inspection_frame() {
    let data = pdfboss_testkit::simple_doc("Hello");
    let doc = Document::load(data.clone()).expect("fixture loads");
    let catalog = doc.get(ObjRef { num: 1, gen: 0 }).expect("object 1");
    let elements = vec![
        Element::Header {
            version: (1, 7),
            span: Span { start: 0, end: 15 },
        },
        Element::IndirectObject {
            r: ObjRef { num: 1, gen: 0 },
            object: Object::Null,
            span: Span { start: 15, end: 64 },
            in_objstm: None,
        },
    ];
    let mut app = App::new(
        "fixture.pdf".to_string(),
        "fixture.pdf".to_string(),
        (1, 7),
        1,
        (80, 24),
    );
    app.update(Msg::TreeBatch {
        req: TreeReq::Physical,
        elements,
        errors: 0,
        done: true,
    });
    app.update(key(KeyCode::Char('j'))); // Pages
    app.update(key(KeyCode::Char('j'))); // Objects
    app.update(key(KeyCode::Char('l'))); // expand Objects
    app.update(key(KeyCode::Char('j'))); // obj 1 0
    app.update(Msg::InspectorLoaded {
        generation: app.inspector_generation,
        payload: pdfboss_tui::inspector::InspectorPayload::Object {
            r: ObjRef { num: 1, gen: 0 },
            object: catalog,
        },
    });
    app.update(Msg::HexLoaded {
        generation: app.hex_generation,
        window_start: 0,
        total_len: 49,
        bytes: data[15..64].to_vec(),
    });
    let terminal = draw(&app);
    assert_frame(
        &terminal,
        &[
            "┌Tree──────────────────────┐┌Inspector · obj 1 0───────────────────────────────┐",
            "│▾ Document · PDF 1.7      ││<<                                                │",
            "│  ▸ Pages (1)             ││  /Pages 2 0 R                                    │",
            "│  ▾ Objects (1)           ││  /Type /Catalog                                  │",
            "│      obj 1 0             ││>>                                                │",
            "│  ▸ Xref (0 secs)         ││                                                  │",
            "│    Trailer               ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          │└──────────────────────────────────────────────────┘",
            "│                          │┌Hex 0xf..0x40─────────────────────────────────────┐",
            "│                          ││0000000f │ 31 20 30 20 6f 62 6a 0a │ 1 0 obj·     │",
            "│                          ││00000017 │ 3c 3c 20 2f 54 79 70 65 │ << /Type     │",
            "│                          ││0000001f │ 20 2f 43 61 74 61 6c 6f │  /Catalo     │",
            "│                          ││00000027 │ 67 20 2f 50 61 67 65 73 │ g /Pages     │",
            "│                          ││0000002f │ 20 32 20 30 20 52 20 3e │  2 0 R >     │",
            "│                          ││00000037 │ 3e 0a 65 6e 64 6f 62 6a │ >·endobj     │",
            "│                          ││0000003f │ 0a                      │ ·            │",
            "└──────────────────────────┘└──────────────────────────────────────────────────┘",
            // A deep breadcrumb pushes the hints past 80 columns; the status
            // bar clips like every other pane.
            "fixture.pdf · /Document/Objects/obj 1 0 · [/] search  [p] preview  [m] markdown",
        ],
    );
}

/// Frame D: the markdown pane toggled on with `m` — headings, list items
/// and a table row in place of the inspector, page-numbered title. The
/// extraction result is injected, so this frame exercises the pane and its
/// styling pass without running text extraction.
#[test]
fn markdown_pane_frame() {
    let data = pdfboss_testkit::simple_doc("Hello");
    let doc = Document::load(data).expect("fixture loads");
    let elements: Vec<Element> = doc
        .elements(ElementOpts {
            physical: true,
            logical: false,
            pages: None,
            content_ops: false,
        })
        .filter_map(Result::ok)
        .collect();
    let mut app = App::new(
        "fixture.pdf".to_string(),
        "fixture.pdf".to_string(),
        doc.version(),
        doc.page_count(),
        (80, 24),
    );
    app.update(Msg::TreeBatch {
        req: TreeReq::Physical,
        elements,
        errors: 0,
        done: true,
    });
    app.update(key(KeyCode::Char('m')));
    app.update(Msg::MarkdownReady {
        generation: app.markdown.generation,
        result: Ok("# Title\n\nBody with **bold**\n\n- one\n- two\n\n| a | b |".to_string()),
    });
    let terminal = draw(&app);
    assert_frame(
        &terminal,
        &[
            "┌Tree──────────────────────┐┌Markdown · page 1 (body only)─────────────────────┐",
            "│▾ Document · PDF 1.7      ││# Title                                           │",
            "│  ▸ Pages (1)             ││                                                  │",
            "│  ▸ Objects (5)           ││Body with bold                                    │",
            "│  ▸ Xref (1 secs)         ││                                                  │",
            "│    Trailer               ││  - one                                           │",
            "│                          ││  - two                                           │",
            "│                          ││                                                  │",
            "│                          ││| a | b |                                         │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          │└──────────────────────────────────────────────────┘",
            "│                          │┌Hex───────────────────────────────────────────────┐",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "│                          ││                                                  │",
            "└──────────────────────────┘└──────────────────────────────────────────────────┘",
            "fixture.pdf · /Document · [/] search  [p] preview  [m] markdown  [q] quit",
        ],
    );
    // The symbols alone cannot show the styling pass ran: the heading is
    // bold and the emphasis run inside the paragraph is too, while the
    // prose around it is not.
    let buffer = terminal.backend().buffer();
    assert!(buffer[(29, 1)].modifier.contains(Modifier::BOLD), "heading");
    assert!(!buffer[(29, 3)].modifier.contains(Modifier::BOLD), "prose");
    assert!(
        buffer[(39, 3)].modifier.contains(Modifier::BOLD),
        "the **bold** run keeps its emphasis once the markers are consumed"
    );
}
