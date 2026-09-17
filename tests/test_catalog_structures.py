"""The requirement, legal attestation, measurement viewport and separation
readers, sync and async. Every fixture is a PDF built inline; the object
bodies follow the clause examples the core tests use."""

import asyncio

import pytest

from pdfboss import (
    AsyncDocument,
    Document,
    LegalAttestation,
    Measure,
    NumberFormat,
    Requirement,
    RequirementHandler,
    SeparationInfo,
    Viewport,
)
from test_spans import build_pdf

LEGAL_COUNTS = {
    "java_script_actions": 2,
    "launch_actions": 1,
    "uri_actions": 3,
    "movie_actions": 4,
    "sound_actions": 5,
    "hide_annotation_actions": 6,
    "go_to_remote_actions": 7,
    "alternate_images": 8,
    "external_streams": 9,
    "true_type_fonts": 10,
    "external_ref_xobjects": 11,
    "external_opi_dicts": 12,
    "non_embedded_fonts": 13,
    "dev_dep_gs_op": 14,
    "dev_dep_gs_ht": 15,
    "dev_dep_gs_tr": 16,
    "dev_dep_gs_ucr": 17,
    "dev_dep_gs_bg": 18,
    "dev_dep_gs_fl": 19,
    "annotations": 20,
    "optional_content": 21,
}


@pytest.fixture
def structures_pdf() -> bytes:
    """Two pages. The catalog lists two requirements (one by reference,
    one with two handlers) and a legal attestation with every count set;
    the first page carries two viewports (the map one by reference, with
    a rectilinear measure) and a cyan separation over both pages; the
    second page a spot-colour separation whose set names a page this
    document does not have. Objects are numbered in one run because the
    builder writes a contiguous xref table."""
    return build_pdf(
        {
            1: (
                b"<< /Type /Catalog /Pages 2 0 R "
                b"/Requirements [5 0 R << /S /Custom /RH [ << /Type /ReqHandler /S /NoOp >> 6 0 R ] >>] "
                b"/Legal << /JavaScriptActions 2 /LaunchActions 1 /URIActions 3 /MovieActions 4 "
                b"/SoundActions 5 /HideAnnotationActions 6 /GoToRemoteActions 7 /AlternateImages 8 "
                b"/ExternalStreams 9 /TrueTypeFonts 10 /ExternalRefXobjects 11 /ExternalOPIdicts 12 "
                b"/NonEmbeddedFonts 13 /DevDepGS_OP 14 /DevDepGS_HT 15 /DevDepGS_TR 16 /DevDepGS_UCR 17 "
                b"/DevDepGS_BG 18 /DevDepGS_FL 19 /Annotations 20 /OptionalContent 21 "
                b"/Attestation (Reviewed by counsel) >> >>"
            ),
            2: b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
            3: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                b"/VP [ 7 0 R << /BBox [100 10 10 100] >> ] "
                b"/SeparationInfo << /Pages [3 0 R 4 0 R] /DeviceColorant /Cyan "
                b"/ColorSpace [/Separation /Cyan /DeviceCMYK 8 0 R] >> >>"
            ),
            4: (
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                b"/SeparationInfo << /Pages [3 0 R 9 0 R] /DeviceColorant (PANTONE 300 C) >> >>"
            ),
            5: (
                b"<< /Type /Requirement /S /EnableJavaScripts "
                b"/RH << /Type /ReqHandler /S /JS /Script (init) >> >>"
            ),
            6: b"<< /Type /ReqHandler /S /JS /Script (fallback) >>",
            7: (
                b"<< /Type /Viewport /BBox [0 0 612 792] /Name (Map) "
                b"/Measure << /Type /Measure /Subtype /RL /R (1in = 0.1 mi) "
                b"/X [ << /Type /NumberFormat /U (mi) /C 0.00139 /D 100000 >> ] "
                b"/D [ << /U (mi) /C 1 >> << /U (feet) /C 5280 /F /F /D 8 >> ] "
                b"/A [ << /U (acres) /C 640 >> ] "
                b"/T [ << /U (deg) /C 1 /O /P /F /R /RT (.) /RD (,) /PS () /SS () /FD true >> ] "
                b"/O [5 6] /CYX 2 >> >>"
            ),
            8: b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0 0 1] /N 1 >>",
        }
    )


@pytest.fixture
def bare_pdf() -> bytes:
    """One page and a catalog with none of the four structures."""
    return build_pdf(
        {
            1: b"<< /Type /Catalog /Pages 2 0 R >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
        }
    )


def test_requirements_read_in_order_with_their_handlers(structures_pdf: bytes) -> None:
    requirements = Document(data=structures_pdf).requirements()
    assert [r.kind for r in requirements] == ["EnableJavaScripts", "Custom"]
    assert all(isinstance(r, Requirement) for r in requirements)
    (init,) = requirements[0].handlers
    assert isinstance(init, RequirementHandler)
    assert (init.kind, init.script) == ("JS", "init")
    assert [(h.kind, h.script) for h in requirements[1].handlers] == [
        ("NoOp", None),
        ("JS", "fallback"),
    ]
    assert repr(requirements[0]) == "Requirement(kind='EnableJavaScripts', handlers=1)"


def test_legal_attestation_reads_every_count(structures_pdf: bytes) -> None:
    legal = Document(data=structures_pdf).legal_attestation()
    assert isinstance(legal, LegalAttestation)
    assert {name: getattr(legal, name) for name in LEGAL_COUNTS} == LEGAL_COUNTS
    assert legal.counts() == LEGAL_COUNTS
    assert legal.attestation == "Reviewed by counsel"
    assert repr(legal) == "LegalAttestation(attestation='Reviewed by counsel')"


def test_viewports_read_with_their_measures_and_defaults(structures_pdf: bytes) -> None:
    page = Document(data=structures_pdf)[0]
    viewports = page.viewports()
    assert len(viewports) == 2
    plain_map, plain = viewports
    assert isinstance(plain_map, Viewport)
    assert plain_map.bbox == (0.0, 0.0, 612.0, 792.0)
    assert plain_map.name == "Map"
    measure = plain_map.measure
    assert isinstance(measure, Measure)
    assert (measure.subtype, measure.scale_ratio) == ("RL", "1in = 0.1 mi")
    (miles,) = measure.x
    assert isinstance(miles, NumberFormat)
    assert (miles.unit, miles.conversion, miles.precision) == ("mi", 0.00139, 100000)
    assert (miles.fraction, miles.label) == ("decimal", "suffix")
    assert (miles.thousands, miles.radix) == (",", ".")
    assert (miles.prefix_spacing, miles.suffix_spacing) == (" ", " ")
    assert miles.fixed_denominator is False
    assert [(f.unit, f.conversion) for f in measure.distance] == [("mi", 1.0), ("feet", 5280.0)]
    assert (measure.distance[1].fraction, measure.distance[1].precision) == ("fraction", 8)
    assert [f.unit for f in measure.area] == ["acres"]
    (degrees,) = measure.angle
    assert (degrees.fraction, degrees.label) == ("round", "prefix")
    assert (degrees.thousands, degrees.radix) == (".", ",")
    assert (degrees.prefix_spacing, degrees.suffix_spacing) == ("", "")
    assert degrees.fixed_denominator is True
    assert measure.y == [] and measure.slope == []
    assert measure.origin == (5.0, 6.0)
    assert measure.y_to_x == 2.0
    assert plain.bbox == (10.0, 10.0, 100.0, 100.0)
    assert plain.name is None and plain.measure is None
    assert repr(plain_map) == "Viewport(bbox=(0.0, 0.0, 612.0, 792.0), name='Map')"


def test_separation_info_resolves_the_set_to_page_indices(structures_pdf: bytes) -> None:
    doc = Document(data=structures_pdf)
    cyan = doc[0].separation_info()
    assert isinstance(cyan, SeparationInfo)
    assert cyan.device_colorant == "Cyan"
    assert cyan.pages == [0, 1]
    assert cyan.page_refs == [(3, 0), (4, 0)]
    color_space = cyan.color_space
    assert isinstance(color_space, list) and len(color_space) == 4
    assert color_space[:2] == ["Separation", "Cyan"]
    spot = doc[1].separation_info()
    assert spot.device_colorant == "PANTONE 300 C"
    assert spot.pages == [0, None]
    assert spot.page_refs == [(3, 0), (9, 0)]
    assert spot.color_space is None
    assert repr(cyan) == "SeparationInfo(device_colorant='Cyan', pages=2)"


def test_absent_structures_read_as_empty_or_none(bare_pdf: bytes) -> None:
    doc = Document(data=bare_pdf)
    assert doc.requirements() == []
    assert doc.legal_attestation() is None
    assert doc[0].viewports() == []
    assert doc[0].separation_info() is None


def test_async_readers_match_the_sync_ones(structures_pdf: bytes) -> None:
    sync = Document(data=structures_pdf)

    async def read() -> tuple:
        doc = await AsyncDocument.from_bytes(structures_pdf)
        requirements = await doc.requirements()
        legal = await doc.legal_attestation()
        viewports = await doc[0].viewports()
        cyan = await doc[0].separation_info()
        spot = await doc[1].separation_info()
        return requirements, legal, viewports, cyan, spot

    requirements, legal, viewports, cyan, spot = asyncio.run(read())
    assert [(r.kind, [(h.kind, h.script) for h in r.handlers]) for r in requirements] == [
        (r.kind, [(h.kind, h.script) for h in r.handlers]) for r in sync.requirements()
    ]
    assert legal.counts() == sync.legal_attestation().counts()
    assert legal.attestation == "Reviewed by counsel"
    assert [(v.bbox, v.name) for v in viewports] == [(v.bbox, v.name) for v in sync[0].viewports()]
    assert [f.unit for f in viewports[0].measure.distance] == ["mi", "feet"]
    assert (cyan.device_colorant, cyan.pages, cyan.page_refs) == ("Cyan", [0, 1], [(3, 0), (4, 0)])
    assert (spot.device_colorant, spot.pages) == ("PANTONE 300 C", [0, None])
