"""The interactive form API: the form dictionary, the field tree with its
inherited entries, every field type's typed readers, the widgets, and the
async twins.

The fixture is one page carrying a form with a text field container and
its two merged kids, a check box, a radio group with two widgets, a combo
box, a push button with captions, and a signed signature field.
"""

import asyncio
from pathlib import Path

import pytest

from pdfboss import AsyncDocument, Document, FieldFlags, FormField, InteractiveForm
from test_spans import build_pdf, stream

ON_APPEARANCE = 19
OFF_APPEARANCE = 20
FIELD_NAMES = [
    "person",
    "person.first",
    "person.last",
    "agree",
    "color",
    "size",
    "submit",
    "sig",
]


def widget(body: bytes) -> bytes:
    return b"<< /Subtype /Widget /Rect [0 0 10 10] " + body + b" >>"


@pytest.fixture
def form_pdf() -> bytes:
    """One page whose form covers every field type. The required flag is
    bit 2 (value 2), radio bit 16 (32768), push button bit 17 (65536) and
    combo bit 18 (131072)."""
    appearances = b"/AP << /N << /%s %d 0 R /Off %d 0 R >> >>"
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
                b"/Annots [6 0 R 7 0 R 8 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R] >>"
            ),
            4: (
                b"<< /Fields [5 0 R 8 0 R 9 0 R 12 0 R 13 0 R 14 0 R] "
                b"/NeedAppearances true /SigFlags 3 /CO [12 0 R] "
                b"/DR << /Font << /Helv 18 0 R >> >> /DA (/Helv 0 Tf 0 g) /Q 1 >>"
            ),
            5: b"<< /T (person) /FT /Tx /Ff 2 /Kids [6 0 R 7 0 R] /V (inherited) >>",
            6: widget(
                b"/Parent 5 0 R /T (first) /V (Ada) /MaxLen 40 /TU (First name) /TM (first_name)"
            ),
            7: widget(b"/Parent 5 0 R /T (last)"),
            8: widget(
                b"/T (agree) /FT /Btn /V /Yes /AS /Yes /Opt [(agreed)] "
                + appearances % (b"Yes", ON_APPEARANCE, OFF_APPEARANCE)
            ),
            9: b"<< /T (color) /FT /Btn /Ff 32768 /V /Red /Kids [10 0 R 11 0 R] /Opt [(red) (blue)] >>",
            10: widget(
                b"/Parent 9 0 R /AS /Red "
                + appearances % (b"Red", ON_APPEARANCE, OFF_APPEARANCE)
            ),
            11: widget(
                b"/Parent 9 0 R /AS /Off "
                + appearances % (b"Blue", ON_APPEARANCE, OFF_APPEARANCE)
            ),
            12: widget(
                b"/T (size) /FT /Ch /Ff 131072 /Opt [[(S) (Small)] (Medium) [(L) (Large)]] "
                b"/V (Medium) /TI 1 /I [1]"
            ),
            13: widget(
                b"/T (submit) /FT /Btn /Ff 65536 "
                b"/MK << /CA (Send) /RC (Sending) /AC (Sent) /I 19 0 R /TP 2 >> "
                b"/AA << /D << /S /JavaScript /JS (go()) >> >>"
            ),
            14: widget(b"/T (sig) /FT /Sig /V 15 0 R /Lock 16 0 R /SV 17 0 R"),
            15: (
                b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached "
                b"/ByteRange [0 100 200 50] /Contents <DEADBEEF> /Name (Ada Lovelace) "
                b"/M (D:20260911120000Z) /Location (London) /Reason (Approval) "
                b"/ContactInfo (ada@example.com) >>"
            ),
            16: b"<< /Type /SigFieldLock /Action /All >>",
            17: b"<< /Type /SV /Ff 1 >>",
            18: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            19: stream(
                b"/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"0 0 10 10 re f"
            ),
            20: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b""),
        }
    )


@pytest.fixture
def fields(form_pdf: bytes) -> dict[str, FormField]:
    return {field.name: field for field in Document(data=form_pdf).form_fields()}


class TestInteractiveForm:
    def test_every_entry_is_read(self, form_pdf: bytes) -> None:
        form = Document(data=form_pdf).interactive_form()
        assert isinstance(form, InteractiveForm)
        assert form.fields == [(5, 0), (8, 0), (9, 0), (12, 0), (13, 0), (14, 0)]
        assert form.need_appearances is True
        assert form.signatures_exist is True
        assert form.append_only is True
        assert form.calculation_order == [(12, 0)]
        assert form.default_resources == {"Font": {"Helv": {"ref": (18, 0)}}}
        assert form.default_appearance == "/Helv 0 Tf 0 g"
        assert form.quadding == "centered"
        assert form.xfa is False
        assert repr(form) == "InteractiveForm(fields=6, need_appearances=True, xfa=False)"

    def test_a_document_without_a_form(self, hello_pdf: Path) -> None:
        doc = Document(hello_pdf)
        assert doc.interactive_form() is None
        assert doc.form_fields() == []


class TestFieldTree:
    def test_fields_come_depth_first_with_qualified_names(self, form_pdf: bytes) -> None:
        names = [field.name for field in Document(data=form_pdf).form_fields()]
        assert names == FIELD_NAMES

    def test_a_container_field_has_kids_and_no_widgets(self, fields: dict[str, FormField]) -> None:
        person = fields["person"]
        assert person.ref == (5, 0)
        assert person.parent is None
        assert person.kids == [(6, 0), (7, 0)]
        assert person.widgets == []
        assert person.field_type == "text"
        assert person.partial_name == "person"
        assert person.value == "inherited"
        assert person.text == "inherited"
        assert repr(person) == "FormField(name='person', field_type='text', widgets=0)"

    def test_kids_inherit_what_they_lack_and_keep_what_they_set(
        self, fields: dict[str, FormField]
    ) -> None:
        first = fields["person.first"]
        assert first.parent == (5, 0)
        assert first.partial_name == "first"
        assert first.field_type == "text"
        assert first.text == "Ada"
        assert first.max_len == 40
        assert first.alternate_name == "First name"
        assert first.mapping_name == "first_name"
        assert first.flags.required is True
        assert [w.ref for w in first.widgets] == [(6, 0)]
        last = fields["person.last"]
        assert last.text == "inherited"
        assert last.max_len is None


class TestFlags:
    def test_names_and_booleans_agree(self, fields: dict[str, FormField]) -> None:
        flags = fields["person"].flags
        assert isinstance(flags, FieldFlags)
        assert flags.bits == 2
        assert int(flags) == 2
        assert flags.names == ["required"]
        assert flags.required is True
        assert flags.read_only is False
        assert repr(flags) == "FieldFlags(required)"
        assert fields["color"].flags.names == ["radio"]
        assert fields["size"].flags.combo is True
        assert fields["submit"].flags.pushbutton is True
        assert fields["sig"].flags.names == []


class TestButtons:
    def test_check_box(self, fields: dict[str, FormField]) -> None:
        agree = fields["agree"]
        assert agree.field_type == "button"
        assert agree.button_kind == "check-box"
        assert agree.value == "Yes"
        assert agree.state == "Yes"
        assert agree.checked is True
        assert agree.on_widgets == [0]
        assert [(o.export_value, o.name) for o in agree.options] == [("agreed", "agreed")]
        (w,) = agree.widgets
        assert w.ref == (8, 0)
        assert w.appearance_state == "Yes"
        assert w.on_state == "Yes"
        assert w.characteristics is None
        assert repr(w) == "Widget(ref=(8, 0), appearance_state='Yes', on_state='Yes')"

    def test_radio_buttons(self, fields: dict[str, FormField]) -> None:
        color = fields["color"]
        assert color.button_kind == "radio-buttons"
        assert color.state == "Red"
        assert color.checked is None
        assert color.on_widgets == [0]
        assert [w.on_state for w in color.widgets] == ["Red", "Blue"]
        assert [w.appearance_state for w in color.widgets] == ["Red", "Off"]
        assert [o.export_value for o in color.options] == ["red", "blue"]
        assert color.options[0].export_value == color.options[color.on_widgets[0]].export_value

    def test_push_button(self, fields: dict[str, FormField]) -> None:
        submit = fields["submit"]
        assert submit.button_kind == "push-button"
        assert submit.state is None
        assert submit.checked is None
        assert submit.on_widgets == []
        (w,) = submit.widgets
        mk = w.characteristics
        assert mk is not None
        assert mk.caption == "Send"
        assert mk.rollover_caption == "Sending"
        assert mk.alternate_caption == "Sent"
        assert mk.icon == (19, 0)
        assert mk.rollover_icon is None
        assert mk.caption_position == "below"
        assert submit.additional_actions == {"D": {"S": "JavaScript", "JS": "go()"}}


class TestChoice:
    def test_combo_box(self, fields: dict[str, FormField]) -> None:
        size = fields["size"]
        assert size.field_type == "choice"
        assert [(o.export_value, o.name) for o in size.options] == [
            ("S", "Small"),
            ("Medium", "Medium"),
            ("L", "Large"),
        ]
        assert size.value == "Medium"
        assert size.selected == ["Medium"]
        assert size.top_index == 1
        assert size.selected_indices == [1]
        assert size.default_value is None
        assert repr(size.options[0]) == "ChoiceOption(export_value='S', name='Small')"


class TestSignature:
    def test_signature_dictionary_is_read_as_data(self, fields: dict[str, FormField]) -> None:
        sig = fields["sig"]
        assert sig.field_type == "signature"
        assert sig.lock == (16, 0)
        assert sig.seed_value == (17, 0)
        assert sig.text is None
        assert sig.selected == []
        signature = sig.signature
        assert signature is not None
        assert signature.filter == "Adobe.PPKLite"
        assert signature.sub_filter == "adbe.pkcs7.detached"
        assert signature.byte_range == [(0, 100), (200, 50)]
        assert signature.contents == b"\xde\xad\xbe\xef"
        assert signature.name == "Ada Lovelace"
        assert signature.signing_time == "D:20260911120000Z"
        assert signature.location == "London"
        assert signature.reason == "Approval"
        assert signature.contact_info == "ada@example.com"
        assert fields["agree"].signature is None


class TestAsync:
    def test_async_twins_read_the_same_form(self, form_pdf: bytes) -> None:
        async def run() -> tuple[InteractiveForm | None, list[FormField]]:
            doc = await AsyncDocument.from_bytes(form_pdf)
            return await doc.interactive_form(), await doc.form_fields()

        form, fields = asyncio.run(run())
        assert form is not None
        assert form.fields == [(5, 0), (8, 0), (9, 0), (12, 0), (13, 0), (14, 0)]
        assert [field.name for field in fields] == FIELD_NAMES
        by_name = {field.name: field for field in fields}
        assert by_name["person.first"].text == "Ada"
        assert by_name["color"].state == "Red"
        assert by_name["sig"].signature.name == "Ada Lovelace"

    def test_async_document_without_a_form(self, hello_pdf: Path) -> None:
        async def run() -> tuple[InteractiveForm | None, list[FormField]]:
            doc = await AsyncDocument.open(hello_pdf)
            return await doc.interactive_form(), await doc.form_fields()

        assert asyncio.run(run()) == (None, [])


class TestVariableText:
    def test_the_forms_defaults_reach_every_field(self, fields: dict[str, FormField]) -> None:
        first = fields["person.first"]
        assert first.default_appearance == "/Helv 0 Tf 0 g"
        assert first.quadding == "centered"
        assert first.default_style is None
        assert first.rich_text is None
