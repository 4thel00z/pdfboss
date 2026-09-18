"""The annotation and action readers, sync and async: a page's annotation
dictionaries with their markup entries, reply states, link destinations,
attached files and actions, and the additional actions of pages and the
catalog. Every fixture is a PDF built inline."""

import asyncio

import pytest

from pdfboss import (
    Action,
    Annotation,
    AnnotationFlags,
    AsyncDocument,
    Border,
    Destination,
    Document,
    FileSpec,
    Markup,
    PdfError,
    Target,
    TriggeredAction,
    WindowsLaunch,
)
from test_spans import build_pdf, stream


@pytest.fixture
def annotated_pdf() -> bytes:
    """One page with six annotations: a link with a go-to action chained
    to a URI action, a link with a named destination, a square with every
    markup entry, a text reply that accepts the square, a file attachment
    with an embedded CSV, and a pop-up; the page and the catalog carry
    additional actions. Objects are numbered in one run because the
    builder writes a contiguous xref table."""
    return build_pdf(
        {
            1: (
                b"<< /Type /Catalog /Pages 2 0 R /Dests << /Here [3 0 R /FitH 500] >> "
                b"/AA << /WC << /S /JavaScript /JS (bye) >> >> >>"
            ),
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
                b"/Annots [4 0 R 5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] "
                b"/AA << /O << /S /Named /N /NextPage >> /C 12 0 R >> >>"
            ),
            4: (
                b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /Border [0 0 1 [3 2]] "
                b"/C [0 0 1] /F 4 /A << /S /GoTo /D [3 0 R /Fit] "
                b"/Next << /S /URI /URI (http://x.example/?q) /IsMap true >> >> >>"
            ),
            5: b"<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest /Here >>",
            6: (
                b"<< /Type /Annot /Subtype /Square /Rect [50 50 0 0] /NM (sq1) "
                b"/Contents (Check this) /M (D:20240102030405Z) /T (Ada) /Popup 9 0 R "
                b"/CA 0.5 /RC (<p>rich</p>) /CreationDate (D:20240101000000Z) "
                b"/Subj (Shape) /IT /SquareCloud /StructParent 3 /OC 13 0 R "
                b"/AP << /N 14 0 R >> /AS /On >>"
            ),
            7: (
                b"<< /Type /Annot /Subtype /Text /Rect [0 0 20 20] /IRT 6 0 R /T (Bob) "
                b"/StateModel (Review) /State (Accepted) /Open true /Name /Comment "
                b"/AA << /E << /S /Named /N /FirstPage >> >> >>"
            ),
            8: (
                b"<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS 10 0 R "
                b"/Name /Paperclip /Contents (The data) >>"
            ),
            9: b"<< /Type /Annot /Subtype /Popup /Rect [0 0 10 10] /Parent 6 0 R /Open false >>",
            10: b"<< /Type /Filespec /F (data.csv) /UF (data.csv) /EF << /F 11 0 R >> >>",
            11: stream(b"/Type /EmbeddedFile", b"a,b\n1,2\n"),
            12: (
                b"<< /S /GoToE /F (outer.pdf) /D [1 /Fit] /NewWindow true "
                b"/T << /R /C /N (child.pdf) /T << /R /C /P 3 /A (annotName) >> >> >>"
            ),
            13: b"<< /Type /OCG /Name (Layer) >>",
            14: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 50 50]", b""),
        }
    )


@pytest.fixture
def actions_pdf() -> bytes:
    """One page whose annotations carry a remote go-to, a launch with
    Windows parameters, a submit-form action kept as its dictionary, and
    a URL file specification."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
                b"/Annots [4 0 R 5 0 R 6 0 R 7 0 R] >>"
            ),
            4: (
                b"<< /Subtype /Link /Rect [0 0 10 10] "
                b"/A << /S /GoToR /F (other.pdf) /D [2 /XYZ 10 20 1.5] /NewWindow false >> >>"
            ),
            5: (
                b"<< /Subtype /Link /Rect [0 0 10 10] /A << /S /Launch /F (setup.exe) "
                b"/Win << /F (c:\\\\tools\\\\setup.exe) /O (print) /P (-q) >> >> >>"
            ),
            6: (
                b"<< /Subtype /Widget /Rect [0 0 10 10] "
                b"/A << /S /SubmitForm /F (http://x.example/post) /Flags 4 >> >>"
            ),
            7: (
                b"<< /Subtype /FileAttachment /Rect [0 0 10 10] "
                b"/FS << /FS /URL /F (http://x.example/a.csv) >> >>"
            ),
        }
    )


@pytest.fixture
def bare_pdf() -> bytes:
    """One page without annotations or additional actions."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
        }
    )


def test_annotations_read_the_common_entries(annotated_pdf: bytes) -> None:
    doc = Document(data=annotated_pdf)
    annotations = doc[0].annotations()
    assert [a.subtype for a in annotations] == [
        "Link",
        "Link",
        "Square",
        "Text",
        "FileAttachment",
        "Popup",
    ]
    link = annotations[0]
    assert isinstance(link, Annotation)
    assert link.ref == (4, 0)
    assert link.rect == (10.0, 10.0, 100.0, 30.0)
    assert isinstance(link.flags, AnnotationFlags)
    assert link.flags.value == 4
    assert link.flags.print and not link.flags.hidden
    assert isinstance(link.border, Border)
    assert link.border.width == 1.0
    assert link.border.dash == [3.0, 2.0]
    assert link.color == [0.0, 0.0, 1.0]
    assert link.markup is None
    assert not link.has_appearance
    square = annotations[2]
    assert square.rect == (0.0, 0.0, 50.0, 50.0)
    assert square.name == "sq1"
    assert square.contents == "Check this"
    assert square.modified == "D:20240102030405Z"
    assert square.modified_date == "2024-01-02T03:04:05Z"
    assert square.struct_parent == 3
    assert square.optional_content == (13, 0)
    assert square.has_appearance
    assert square.appearance_state == "On"
    assert square.page_ref is None
    assert repr(link) == "Annotation(subtype='Link', rect=(10.0, 10.0, 100.0, 30.0), contents=None)"


def test_markup_entries_and_reply_states(annotated_pdf: bytes) -> None:
    doc = Document(data=annotated_pdf)
    annotations = doc[0].annotations()
    markup = annotations[2].markup
    assert isinstance(markup, Markup)
    assert markup.title == "Ada"
    assert markup.popup == (9, 0)
    assert markup.opacity == 0.5
    assert markup.rich_contents == "<p>rich</p>"
    assert markup.created == "2024-01-01T00:00:00Z"
    assert markup.subject == "Shape"
    assert markup.intent == "SquareCloud"
    assert markup.reply_type == "reply"
    assert markup.in_reply_to is None
    reply = annotations[3]
    assert reply.markup is not None
    assert reply.markup.in_reply_to == (6, 0)
    assert reply.markup.title == "Bob"
    assert reply.markup.opacity == 1.0
    assert reply.state == "Accepted"
    assert reply.state_model == "Review"
    assert reply.open is True
    assert reply.icon == "Comment"
    popup = annotations[5]
    assert popup.parent == (6, 0)
    assert popup.open is False
    assert popup.markup is None
    assert annotations[0].state is None


def test_link_destinations_and_action_chains(annotated_pdf: bytes) -> None:
    doc = Document(data=annotated_pdf)
    annotations = doc[0].annotations()
    action = annotations[0].action
    assert isinstance(action, Action)
    assert action.kind == "GoTo"
    assert action.ref is None
    assert isinstance(action.destination, Destination)
    assert action.destination.page == 0
    assert action.destination.fit == "fit"
    assert action.named_destination is None
    assert len(action.next) == 1
    uri = action.next[0]
    assert uri.kind == "URI"
    assert uri.uri == "http://x.example/?q"
    assert uri.is_map is True
    assert uri.next == []
    assert uri.entries is None
    assert repr(action) == "Action(kind='GoTo', next=1)"
    named = annotations[1]
    assert named.action is None
    assert named.destination is not None
    assert named.destination.page == 0
    assert named.destination.fit == "fit-h"
    assert named.destination.top == 500.0
    assert annotations[2].destination is None


def test_file_attachments_decode_through_file_spec_data(annotated_pdf: bytes) -> None:
    doc = Document(data=annotated_pdf)
    attachment = doc[0].annotations()[4]
    spec = attachment.file
    assert isinstance(spec, FileSpec)
    assert spec.name == "data.csv"
    assert spec.file == b"data.csv"
    assert spec.unicode_file == "data.csv"
    assert spec.ref == (11, 0)
    assert spec.url is None
    assert attachment.icon == "Paperclip"
    assert attachment.contents == "The data"
    assert doc.file_spec_data(spec) == b"a,b\n1,2\n"
    assert repr(spec) == "FileSpec(name='data.csv', embedded=True)"


def test_additional_actions_of_annotations_pages_and_the_document(annotated_pdf: bytes) -> None:
    doc = Document(data=annotated_pdf)
    reply = doc[0].annotations()[3]
    assert [t.trigger for t in reply.additional_actions] == ["cursor-enter"]
    triggered = reply.additional_actions[0]
    assert isinstance(triggered, TriggeredAction)
    assert triggered.action.kind == "Named"
    assert triggered.action.name == "FirstPage"
    page_actions = doc[0].additional_actions()
    assert [t.trigger for t in page_actions] == ["open", "close"]
    close = page_actions[1].action
    assert close.kind == "GoToE"
    assert close.ref == (12, 0)
    assert close.new_window is True
    assert close.file is not None and close.file.name == "outer.pdf"
    assert close.destination is not None
    assert close.destination.page == 1
    assert close.destination.page_ref is None
    target = close.target
    assert isinstance(target, Target)
    assert target.relationship == "child"
    assert target.name == b"child.pdf"
    assert target.page is None
    assert target.next is not None
    assert target.next.page == 3
    assert target.next.annotation == "annotName"
    assert target.next.next is None
    document_actions = doc.additional_actions()
    assert [t.trigger for t in document_actions] == ["will-close"]
    assert document_actions[0].action.kind == "JavaScript"
    assert document_actions[0].action.script == "bye"
    assert repr(triggered) == "TriggeredAction(trigger='cursor-enter', action=Action(kind='Named', next=0))"


def test_remote_launch_and_untyped_actions(actions_pdf: bytes) -> None:
    doc = Document(data=actions_pdf)
    annotations = doc[0].annotations()
    remote = annotations[0].action
    assert remote is not None
    assert remote.kind == "GoToR"
    assert remote.file is not None and remote.file.name == "other.pdf"
    assert remote.destination is not None
    assert remote.destination.page == 2
    assert remote.destination.fit == "xyz"
    assert remote.destination.zoom == 1.5
    assert remote.new_window is False
    launch = annotations[1].action
    assert launch is not None
    assert launch.kind == "Launch"
    assert launch.file is not None and launch.file.name == "setup.exe"
    windows = launch.windows
    assert isinstance(windows, WindowsLaunch)
    assert windows.file == b"c:\\tools\\setup.exe"
    assert windows.directory is None
    assert windows.operation == "print"
    assert windows.parameters == b"-q"
    assert launch.new_window is None
    submit = annotations[2].action
    assert submit is not None
    assert submit.kind == "SubmitForm"
    assert submit.entries == {"S": "SubmitForm", "F": "http://x.example/post", "Flags": 4}
    assert submit.uri is None and submit.script is None and submit.name is None
    url = annotations[3].file
    assert url is not None
    assert url.file_system == "URL"
    assert url.url == "http://x.example/a.csv"
    assert url.ref is None
    with pytest.raises(PdfError):
        doc.file_spec_data(url)


def test_pages_without_annotations_or_actions_read_as_empty(bare_pdf: bytes) -> None:
    doc = Document(data=bare_pdf)
    assert doc[0].annotations() == []
    assert doc[0].additional_actions() == []
    assert doc.additional_actions() == []


def test_async_twins_agree(annotated_pdf: bytes, bare_pdf: bytes) -> None:
    async def run() -> None:
        doc = await AsyncDocument.from_bytes(annotated_pdf)
        annotations = await doc[0].annotations()
        assert [a.subtype for a in annotations] == [
            "Link",
            "Link",
            "Square",
            "Text",
            "FileAttachment",
            "Popup",
        ]
        assert annotations[0].action is not None
        assert annotations[0].action.next[0].uri == "http://x.example/?q"
        assert annotations[3].state == "Accepted"
        spec = annotations[4].file
        assert spec is not None
        assert await doc.file_spec_data(spec) == b"a,b\n1,2\n"
        page_actions = await doc[0].additional_actions()
        assert [t.trigger for t in page_actions] == ["open", "close"]
        document_actions = await doc.additional_actions()
        assert [t.trigger for t in document_actions] == ["will-close"]
        bare = await AsyncDocument.from_bytes(bare_pdf)
        assert await bare[0].annotations() == []
        assert await bare[0].additional_actions() == []
        assert await bare.additional_actions() == []

    asyncio.run(run())
