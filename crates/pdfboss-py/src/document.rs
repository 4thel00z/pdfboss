//! Document- and page-level structures as Python sees them: the
//! linearization parameter dictionary, output intents, page-piece data,
//! thumbnails, article threads, presentations, permission handlers,
//! requirements, the legal attestation, measurement viewports and
//! separation dictionaries, as frozen classes over the core types.
//! Enumerations are kebab-case strings and object references `(num, gen)`
//! tuples.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use pdfboss_core::{
    ArticleThread as CoreArticleThread, Bead as CoreBead, Dimension, FractionFormat, LabelPosition,
    LegalAttestation as CoreLegalAttestation, Linearization as CoreLinearization,
    Measure as CoreMeasure, Motion, NumberFormat as CoreNumberFormat, Object, OcEvent,
    OcGroup as CoreOcGroup, OcUsage as CoreOcUsage, OutputIntent as CoreOutputIntent,
    PagePiece as CorePagePiece, PermissionHandlers as CorePermissionHandlers,
    Presentation as CorePresentation, Requirement as CoreRequirement,
    RequirementHandler as CoreRequirementHandler, SeparationInfo as CoreSeparationInfo,
    Thumbnail as CoreThumbnail, Transition as CoreTransition, TransitionDirection, TransitionStyle,
    Viewport as CoreViewport,
};

use crate::catalog::PageIndex;
use crate::forms::Signature;
use crate::{
    metadata_dict, object_to_py, rect_tuple, ref_tuple, repr_bool, repr_opt, repr_opt_float,
    repr_opt_str, repr_str,
};

fn transition_style_str(style: TransitionStyle) -> &'static str {
    match style {
        TransitionStyle::Split => "split",
        TransitionStyle::Blinds => "blinds",
        TransitionStyle::Box => "box",
        TransitionStyle::Wipe => "wipe",
        TransitionStyle::Dissolve => "dissolve",
        TransitionStyle::Glitter => "glitter",
        TransitionStyle::Replace => "replace",
        TransitionStyle::Fly => "fly",
        TransitionStyle::Push => "push",
        TransitionStyle::Cover => "cover",
        TransitionStyle::Uncover => "uncover",
        TransitionStyle::Fade => "fade",
    }
}

fn dimension_str(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Horizontal => "horizontal",
        Dimension::Vertical => "vertical",
    }
}

fn motion_str(motion: Motion) -> &'static str {
    match motion {
        Motion::Inward => "inward",
        Motion::Outward => "outward",
    }
}

fn direction_angle(direction: TransitionDirection) -> Option<u32> {
    match direction {
        TransitionDirection::Angle(angle) => Some(angle),
        TransitionDirection::None => None,
    }
}

/// The linearization parameter dictionary, the first object of a
/// linearized file, as written.
#[pyclass(frozen)]
pub(crate) struct Linearization {
    inner: CoreLinearization,
}

impl From<CoreLinearization> for Linearization {
    fn from(inner: CoreLinearization) -> Linearization {
        Linearization { inner }
    }
}

#[pymethods]
impl Linearization {
    /// The version of the linearization scheme, `/Linearized`.
    #[getter]
    fn version(&self) -> f64 {
        self.inner.version
    }

    /// The file length the dictionary declares, `/L`; a file whose real
    /// length differs has been updated since and is ordinary PDF.
    #[getter]
    fn file_length(&self) -> u64 {
        self.inner.file_length
    }

    /// The `(offset, length)` of each hint stream, `/H`.
    #[getter]
    fn hint_streams(&self) -> Vec<(u64, u64)> {
        self.inner.hint_streams.clone()
    }

    /// The object number of the first page's page object, `/O`.
    #[getter]
    fn first_page_object(&self) -> u32 {
        self.inner.first_page_object
    }

    /// The offset of the end of the first page, `/E`.
    #[getter]
    fn first_page_end(&self) -> u64 {
        self.inner.first_page_end
    }

    /// The number of pages, `/N`.
    #[getter]
    fn page_count(&self) -> u32 {
        self.inner.page_count
    }

    /// The offset of the main cross-reference table, `/T`.
    #[getter]
    fn main_xref_offset(&self) -> u64 {
        self.inner.main_xref_offset
    }

    /// The 0-based index of the page a viewer opens first, `/P`; 0 when
    /// absent.
    #[getter]
    fn first_page(&self) -> u32 {
        self.inner.first_page
    }

    fn __repr__(&self) -> String {
        format!(
            "Linearization(version={:?}, file_length={}, page_count={})",
            self.inner.version, self.inner.file_length, self.inner.page_count
        )
    }
}

/// One output intent: the colour characteristics of the device the
/// document was prepared for.
#[pyclass(frozen)]
pub(crate) struct OutputIntent {
    inner: CoreOutputIntent,
}

impl From<CoreOutputIntent> for OutputIntent {
    fn from(inner: CoreOutputIntent) -> OutputIntent {
        OutputIntent { inner }
    }
}

#[pymethods]
impl OutputIntent {
    /// The intent's subtype, such as `"GTS_PDFA1"` or `"GTS_PDFX"`.
    #[getter]
    fn subtype(&self) -> &str {
        &self.inner.subtype
    }

    /// The human-readable name of the output condition.
    #[getter]
    fn output_condition(&self) -> Option<&str> {
        self.inner.output_condition.as_deref()
    }

    /// The registry's identifier of the output condition, or `"Custom"`.
    #[getter]
    fn output_condition_identifier(&self) -> Option<&str> {
        self.inner.output_condition_identifier.as_deref()
    }

    /// The URI of the registry the identifier comes from.
    #[getter]
    fn registry_name(&self) -> Option<&str> {
        self.inner.registry_name.as_deref()
    }

    /// The human-readable description of the intended device.
    #[getter]
    fn info(&self) -> Option<&str> {
        self.inner.info.as_deref()
    }

    /// The ICC profile stream's `(num, gen)` reference.
    #[getter]
    fn destination_profile(&self) -> Option<(u32, u16)> {
        self.inner.destination_profile.map(ref_tuple)
    }

    fn __repr__(&self) -> String {
        format!(
            "OutputIntent(subtype={}, output_condition_identifier={})",
            repr_str(&self.inner.subtype),
            repr_opt_str(self.inner.output_condition_identifier.as_deref())
        )
    }
}

/// One product's private data in a page-piece dictionary.
#[pyclass(frozen)]
pub(crate) struct PagePiece {
    inner: CorePagePiece,
}

impl From<CorePagePiece> for PagePiece {
    fn from(inner: CorePagePiece) -> PagePiece {
        PagePiece { inner }
    }
}

#[pymethods]
impl PagePiece {
    /// The name of the product that owns the data.
    #[getter]
    fn product(&self) -> &str {
        &self.inner.product
    }

    /// When the product last changed the data, as an ISO 8601 string;
    /// `None` when absent or unreadable.
    #[getter]
    fn last_modified(&self) -> Option<String> {
        self.inner
            .last_modified_parsed()
            .map(|date| date.to_iso8601())
    }

    /// The product's private data as plain Python data, as written.
    #[getter]
    fn private<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .private
            .as_ref()
            .map(|value| object_to_py(py, value))
            .transpose()
    }

    fn __repr__(&self) -> String {
        format!(
            "PagePiece(product={}, last_modified={})",
            repr_str(&self.inner.product),
            repr_opt_str(self.last_modified().as_deref())
        )
    }
}

/// A page's thumbnail image as written; `Page.thumbnail_image` decodes it.
#[pyclass(frozen)]
pub(crate) struct Thumbnail {
    inner: CoreThumbnail,
}

impl From<CoreThumbnail> for Thumbnail {
    fn from(inner: CoreThumbnail) -> Thumbnail {
        Thumbnail { inner }
    }
}

#[pymethods]
impl Thumbnail {
    /// The image width in samples.
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width
    }

    /// The image height in samples.
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height
    }

    /// The bits per colour component.
    #[getter]
    fn bits_per_component(&self) -> Option<u32> {
        self.inner.bits_per_component
    }

    /// The colour space as plain Python data: a name such as
    /// `"DeviceRGB"`, or an array.
    #[getter]
    fn color_space<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .color_space
            .as_ref()
            .map(|value| object_to_py(py, value))
            .transpose()
    }

    /// The decode array mapping sample values to colour components.
    #[getter]
    fn decode(&self) -> Option<Vec<f64>> {
        self.inner.decode.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "Thumbnail(width={}, height={})",
            self.inner.width, self.inner.height
        )
    }
}

/// One bead of an article thread: a rectangle on a page.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct Bead {
    inner: CoreBead,
    page: Option<usize>,
}

impl Bead {
    fn new(inner: CoreBead, pages: &PageIndex) -> Bead {
        let page = inner
            .page
            .and_then(|page_ref| pages.get(&page_ref).copied());
        Bead { inner, page }
    }
}

#[pymethods]
impl Bead {
    /// The bead dictionary's `(num, gen)` reference.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> (u32, u16) {
        ref_tuple(self.inner.object)
    }

    /// The 0-based index of the page the bead is on; `None` when its
    /// reference names no page of this document.
    #[getter]
    fn page(&self) -> Option<usize> {
        self.page
    }

    /// The `(num, gen)` reference of the page the bead names, as written.
    #[getter]
    fn page_ref(&self) -> Option<(u32, u16)> {
        self.inner.page.map(ref_tuple)
    }

    /// The bead's rectangle on its page, `(x0, y0, x1, y1)`.
    #[getter]
    fn rect(&self) -> Option<(f32, f32, f32, f32)> {
        self.inner.rect.map(rect_tuple)
    }

    fn __repr__(&self) -> String {
        format!(
            "Bead(ref={:?}, page={})",
            ref_tuple(self.inner.object),
            repr_opt(self.page)
        )
    }
}

/// One article thread: its information dictionary and its beads in
/// reading order.
#[pyclass(frozen)]
pub(crate) struct ArticleThread {
    inner: CoreArticleThread,
    beads: Vec<Bead>,
}

impl ArticleThread {
    fn new(inner: CoreArticleThread, pages: &PageIndex) -> ArticleThread {
        let beads = inner
            .beads
            .iter()
            .cloned()
            .map(|bead| Bead::new(bead, pages))
            .collect();
        ArticleThread { inner, beads }
    }
}

/// Wraps each core thread with its beads' pages resolved through `pages`.
pub(crate) fn article_threads(
    threads: Vec<CoreArticleThread>,
    pages: &PageIndex,
) -> Vec<ArticleThread> {
    threads
        .into_iter()
        .map(|thread| ArticleThread::new(thread, pages))
        .collect()
}

#[pymethods]
impl ArticleThread {
    /// The thread dictionary's `(num, gen)` reference; `None` for a thread
    /// written directly into the catalog's array.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> Option<(u32, u16)> {
        self.inner.object.map(ref_tuple)
    }

    /// The thread's information dictionary with the keys of
    /// `Document.metadata`; only the entries present are included.
    #[getter]
    fn info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        metadata_dict(py, self.inner.info.clone())
    }

    /// The beads in reading order, from the first bead along each `/N`.
    #[getter]
    fn beads(&self) -> Vec<Bead> {
        self.beads.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ArticleThread(title={}, beads={})",
            repr_opt_str(self.inner.info.title.as_deref()),
            self.beads.len()
        )
    }
}

/// A page's transition in a presentation, with the standard's defaults
/// filled in for the entries the page leaves out.
#[pyclass(frozen)]
pub(crate) struct Transition {
    inner: CoreTransition,
}

impl From<CoreTransition> for Transition {
    fn from(inner: CoreTransition) -> Transition {
        Transition { inner }
    }
}

#[pymethods]
impl Transition {
    /// The transition style: `"split"`, `"blinds"`, `"box"`, `"wipe"`,
    /// `"dissolve"`, `"glitter"`, `"replace"`, `"fly"`, `"push"`,
    /// `"cover"`, `"uncover"` or `"fade"`.
    #[getter]
    fn style(&self) -> &'static str {
        transition_style_str(self.inner.style)
    }

    /// The effect's duration in seconds.
    #[getter]
    fn duration(&self) -> f64 {
        self.inner.duration
    }

    /// The dimension a split or blinds effect moves in: `"horizontal"`
    /// or `"vertical"`.
    #[getter]
    fn dimension(&self) -> &'static str {
        dimension_str(self.inner.dimension)
    }

    /// The direction of motion of a split or box effect: `"inward"` or
    /// `"outward"`.
    #[getter]
    fn motion(&self) -> &'static str {
        motion_str(self.inner.motion)
    }

    /// The direction of motion in degrees counterclockwise from left to
    /// right; `None` when the page names no direction, `/None`.
    #[getter]
    fn direction(&self) -> Option<u32> {
        direction_angle(self.inner.direction)
    }

    /// The starting or ending scale of a fly effect.
    #[getter]
    fn scale(&self) -> f64 {
        self.inner.scale
    }

    /// Whether a fly effect's area is rectangular and opaque.
    #[getter]
    fn opaque(&self) -> bool {
        self.inner.opaque
    }

    fn __repr__(&self) -> String {
        format!(
            "Transition(style={}, duration={:?})",
            repr_str(self.style()),
            self.inner.duration
        )
    }
}

/// How a page is shown in a presentation: its display duration and its
/// transition.
#[pyclass(frozen)]
pub(crate) struct Presentation {
    inner: CorePresentation,
}

impl From<CorePresentation> for Presentation {
    fn from(inner: CorePresentation) -> Presentation {
        Presentation { inner }
    }
}

#[pymethods]
impl Presentation {
    /// How long the page is displayed in seconds before the presentation
    /// advances; `None` when the page sets no duration.
    #[getter]
    fn duration(&self) -> Option<f64> {
        self.inner.duration
    }

    /// The transition the page is reached through; `None` when the page
    /// sets none.
    #[getter]
    fn transition(&self) -> Option<Transition> {
        self.inner.transition.clone().map(Transition::from)
    }

    fn __repr__(&self) -> String {
        format!(
            "Presentation(duration={}, transition={})",
            repr_opt_float(self.inner.duration),
            repr_opt_str(
                self.inner
                    .transition
                    .as_ref()
                    .map(|transition| transition_style_str(transition.style))
            )
        )
    }
}

/// The catalog's permission handlers: the signatures that certify the
/// document and grant usage rights, read as data and not verified.
#[pyclass(frozen)]
pub(crate) struct PermissionHandlers {
    inner: CorePermissionHandlers,
}

impl From<CorePermissionHandlers> for PermissionHandlers {
    fn from(inner: CorePermissionHandlers) -> PermissionHandlers {
        PermissionHandlers { inner }
    }
}

#[pymethods]
impl PermissionHandlers {
    /// The certifying signature, `/DocMDP`; `None` without one.
    #[getter]
    fn doc_mdp(&self) -> Option<Signature> {
        self.inner.doc_mdp.clone().map(Signature::from)
    }

    /// The usage rights signature, `/UR3`; `None` without one.
    #[getter]
    fn usage_rights(&self) -> Option<Signature> {
        self.inner.usage_rights.clone().map(Signature::from)
    }

    fn __repr__(&self) -> String {
        format!(
            "PermissionHandlers(doc_mdp={}, usage_rights={})",
            repr_bool(self.inner.doc_mdp.is_some()),
            repr_bool(self.inner.usage_rights.is_some())
        )
    }
}

fn fraction_format_str(format: FractionFormat) -> &'static str {
    match format {
        FractionFormat::Decimal => "decimal",
        FractionFormat::Fraction => "fraction",
        FractionFormat::Round => "round",
        FractionFormat::Truncate => "truncate",
    }
}

fn label_position_str(position: LabelPosition) -> &'static str {
    match position {
        LabelPosition::Suffix => "suffix",
        LabelPosition::Prefix => "prefix",
    }
}

/// One requirement the catalog lists: a feature a reader needs to show
/// the document as intended, with the handlers for a reader that lacks it.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct Requirement {
    inner: CoreRequirement,
}

impl From<CoreRequirement> for Requirement {
    fn from(inner: CoreRequirement) -> Requirement {
        Requirement { inner }
    }
}

#[pymethods]
impl Requirement {
    /// The requirement type, `/S`: `"EnableJavaScripts"` is the one the
    /// standard defines; any other name is kept as written.
    #[getter]
    fn kind(&self) -> &str {
        &self.inner.kind
    }

    /// The handlers a reader that does not meet the requirement runs,
    /// `/RH`, in order.
    #[getter]
    fn handlers(&self) -> Vec<RequirementHandler> {
        self.inner
            .handlers
            .iter()
            .cloned()
            .map(RequirementHandler::from)
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Requirement(kind={}, handlers={})",
            repr_str(&self.inner.kind),
            self.inner.handlers.len()
        )
    }
}

/// One requirement handler, read as data: pdfboss runs no JavaScript, so
/// the handler is reported, not invoked.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct RequirementHandler {
    inner: CoreRequirementHandler,
}

impl From<CoreRequirementHandler> for RequirementHandler {
    fn from(inner: CoreRequirementHandler) -> RequirementHandler {
        RequirementHandler { inner }
    }
}

#[pymethods]
impl RequirementHandler {
    /// The handler type, `/S`: `"JS"` runs a document-level JavaScript,
    /// `"NoOp"` does nothing; any other name is kept as written.
    #[getter]
    fn kind(&self) -> &str {
        &self.inner.kind
    }

    /// The name of the document-level JavaScript a `"JS"` handler runs,
    /// `/Script`, as the catalog's name tree lists it.
    #[getter]
    fn script(&self) -> Option<&str> {
        self.inner.script.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "RequirementHandler(kind={}, script={})",
            repr_str(&self.inner.kind),
            repr_opt_str(self.inner.script.as_deref())
        )
    }
}

/// The catalog's legal attestation: how much content of each kind that a
/// certifying signature cannot vouch for the document holds, and the
/// signer's statement about it. Every count is read as written, an absent
/// one as 0; nothing is recounted.
#[pyclass(frozen)]
pub(crate) struct LegalAttestation {
    inner: CoreLegalAttestation,
}

impl From<CoreLegalAttestation> for LegalAttestation {
    fn from(inner: CoreLegalAttestation) -> LegalAttestation {
        LegalAttestation { inner }
    }
}

#[pymethods]
impl LegalAttestation {
    /// JavaScript actions, `/JavaScriptActions`.
    #[getter]
    fn java_script_actions(&self) -> u32 {
        self.inner.java_script_actions
    }

    /// Launch actions, `/LaunchActions`.
    #[getter]
    fn launch_actions(&self) -> u32 {
        self.inner.launch_actions
    }

    /// URI actions, `/URIActions`.
    #[getter]
    fn uri_actions(&self) -> u32 {
        self.inner.uri_actions
    }

    /// Movie actions, `/MovieActions`.
    #[getter]
    fn movie_actions(&self) -> u32 {
        self.inner.movie_actions
    }

    /// Sound actions, `/SoundActions`.
    #[getter]
    fn sound_actions(&self) -> u32 {
        self.inner.sound_actions
    }

    /// Hide actions, `/HideAnnotationActions`.
    #[getter]
    fn hide_annotation_actions(&self) -> u32 {
        self.inner.hide_annotation_actions
    }

    /// Remote go-to actions, `/GoToRemoteActions`.
    #[getter]
    fn go_to_remote_actions(&self) -> u32 {
        self.inner.go_to_remote_actions
    }

    /// Alternate images, `/AlternateImages`.
    #[getter]
    fn alternate_images(&self) -> u32 {
        self.inner.alternate_images
    }

    /// Streams read from outside the file, `/ExternalStreams`.
    #[getter]
    fn external_streams(&self) -> u32 {
        self.inner.external_streams
    }

    /// TrueType fonts, `/TrueTypeFonts`.
    #[getter]
    fn true_type_fonts(&self) -> u32 {
        self.inner.true_type_fonts
    }

    /// Reference XObjects, `/ExternalRefXobjects`.
    #[getter]
    fn external_ref_xobjects(&self) -> u32 {
        self.inner.external_ref_xobjects
    }

    /// OPI dictionaries, `/ExternalOPIdicts`.
    #[getter]
    fn external_opi_dicts(&self) -> u32 {
        self.inner.external_opi_dicts
    }

    /// Fonts without an embedded program, `/NonEmbeddedFonts`.
    #[getter]
    fn non_embedded_fonts(&self) -> u32 {
        self.inner.non_embedded_fonts
    }

    /// Graphics state parameter dictionaries setting overprint,
    /// `/DevDepGS_OP`.
    #[getter]
    fn dev_dep_gs_op(&self) -> u32 {
        self.inner.dev_dep_gs_op
    }

    /// Graphics state parameter dictionaries with a halftone, `/DevDepGS_HT`.
    #[getter]
    fn dev_dep_gs_ht(&self) -> u32 {
        self.inner.dev_dep_gs_ht
    }

    /// Graphics state parameter dictionaries with a transfer function,
    /// `/DevDepGS_TR`.
    #[getter]
    fn dev_dep_gs_tr(&self) -> u32 {
        self.inner.dev_dep_gs_tr
    }

    /// Graphics state parameter dictionaries with undercolour removal,
    /// `/DevDepGS_UCR`.
    #[getter]
    fn dev_dep_gs_ucr(&self) -> u32 {
        self.inner.dev_dep_gs_ucr
    }

    /// Graphics state parameter dictionaries with black generation,
    /// `/DevDepGS_BG`.
    #[getter]
    fn dev_dep_gs_bg(&self) -> u32 {
        self.inner.dev_dep_gs_bg
    }

    /// Graphics state parameter dictionaries with a flatness tolerance,
    /// `/DevDepGS_FL`.
    #[getter]
    fn dev_dep_gs_fl(&self) -> u32 {
        self.inner.dev_dep_gs_fl
    }

    /// Annotations, `/Annotations`.
    #[getter]
    fn annotations(&self) -> u32 {
        self.inner.annotations
    }

    /// Optional content groups, `/OptionalContent`.
    #[getter]
    fn optional_content(&self) -> u32 {
        self.inner.optional_content
    }

    /// The signer's statement about the counted content, `/Attestation`.
    #[getter]
    fn attestation(&self) -> Option<&str> {
        self.inner.attestation.as_deref()
    }

    /// Every count keyed by its Python attribute name, for callers that
    /// want the whole table at once.
    fn counts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        let legal = &self.inner;
        for (name, count) in [
            ("java_script_actions", legal.java_script_actions),
            ("launch_actions", legal.launch_actions),
            ("uri_actions", legal.uri_actions),
            ("movie_actions", legal.movie_actions),
            ("sound_actions", legal.sound_actions),
            ("hide_annotation_actions", legal.hide_annotation_actions),
            ("go_to_remote_actions", legal.go_to_remote_actions),
            ("alternate_images", legal.alternate_images),
            ("external_streams", legal.external_streams),
            ("true_type_fonts", legal.true_type_fonts),
            ("external_ref_xobjects", legal.external_ref_xobjects),
            ("external_opi_dicts", legal.external_opi_dicts),
            ("non_embedded_fonts", legal.non_embedded_fonts),
            ("dev_dep_gs_op", legal.dev_dep_gs_op),
            ("dev_dep_gs_ht", legal.dev_dep_gs_ht),
            ("dev_dep_gs_tr", legal.dev_dep_gs_tr),
            ("dev_dep_gs_ucr", legal.dev_dep_gs_ucr),
            ("dev_dep_gs_bg", legal.dev_dep_gs_bg),
            ("dev_dep_gs_fl", legal.dev_dep_gs_fl),
            ("annotations", legal.annotations),
            ("optional_content", legal.optional_content),
        ] {
            dict.set_item(name, count)?;
        }
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        format!(
            "LegalAttestation(attestation={})",
            repr_opt_str(self.inner.attestation.as_deref())
        )
    }
}

/// A viewport: a page rectangle with its own measurement scale. Where
/// viewports overlap, the last one in the array whose box contains a
/// point applies to it.
#[pyclass(frozen)]
pub(crate) struct Viewport {
    inner: CoreViewport,
}

impl From<CoreViewport> for Viewport {
    fn from(inner: CoreViewport) -> Viewport {
        Viewport { inner }
    }
}

#[pymethods]
impl Viewport {
    /// The rectangle in default user space, `/BBox`, as `(x0, y0, x1, y1)`.
    #[getter]
    fn bbox(&self) -> (f32, f32, f32, f32) {
        rect_tuple(self.inner.bbox)
    }

    /// A descriptive title, `/Name`.
    #[getter]
    fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// The units of the viewport's coordinate system, `/Measure`; `None`
    /// without one.
    #[getter]
    fn measure(&self) -> Option<Measure> {
        self.inner.measure.clone().map(Measure::from)
    }

    fn __repr__(&self) -> String {
        format!(
            "Viewport(bbox={:?}, name={})",
            rect_tuple(self.inner.bbox),
            repr_opt_str(self.inner.name.as_deref())
        )
    }
}

/// A measure dictionary: how distances, areas and angles in a viewport
/// convert to real-world units. Each axis is a chain of number formats
/// from the coarsest unit to the finest.
#[pyclass(frozen)]
pub(crate) struct Measure {
    inner: CoreMeasure,
}

impl From<CoreMeasure> for Measure {
    fn from(inner: CoreMeasure) -> Measure {
        Measure { inner }
    }
}

fn number_formats(formats: &[CoreNumberFormat]) -> Vec<NumberFormat> {
    formats.iter().cloned().map(NumberFormat::from).collect()
}

#[pymethods]
impl Measure {
    /// `/Subtype`: `"RL"`, rectilinear, the one kind the standard defines
    /// and the default; any other name is kept as written.
    #[getter]
    fn subtype(&self) -> &str {
        &self.inner.subtype
    }

    /// The scale ratio as text, `/R`, in the `1in = 0.1 mi` style.
    #[getter]
    fn scale_ratio(&self) -> Option<&str> {
        self.inner.scale_ratio.as_deref()
    }

    /// The number formats for x distances, `/X`.
    #[getter]
    fn x(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.x)
    }

    /// The formats for y distances, `/Y`; empty when they share `x`'s.
    #[getter]
    fn y(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.y)
    }

    /// The formats for distances, `/D`.
    #[getter]
    fn distance(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.distance)
    }

    /// The formats for areas, `/A`.
    #[getter]
    fn area(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.area)
    }

    /// The formats for angles, `/T`.
    #[getter]
    fn angle(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.angle)
    }

    /// The formats for slopes, `/S`.
    #[getter]
    fn slope(&self) -> Vec<NumberFormat> {
        number_formats(&self.inner.slope)
    }

    /// The origin of the measurement coordinate system in default user
    /// space, `/O`; `(0.0, 0.0)` when absent.
    #[getter]
    fn origin(&self) -> (f64, f64) {
        self.inner.origin
    }

    /// The factor that converts y units to x units when the two differ,
    /// `/CYX`.
    #[getter]
    fn y_to_x(&self) -> Option<f64> {
        self.inner.y_to_x
    }

    fn __repr__(&self) -> String {
        format!(
            "Measure(subtype={}, scale_ratio={})",
            repr_str(&self.inner.subtype),
            repr_opt_str(self.inner.scale_ratio.as_deref())
        )
    }
}

/// One unit of a measurement chain and how its value is shown.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct NumberFormat {
    inner: CoreNumberFormat,
}

impl From<CoreNumberFormat> for NumberFormat {
    fn from(inner: CoreNumberFormat) -> NumberFormat {
        NumberFormat { inner }
    }
}

#[pymethods]
impl NumberFormat {
    /// The unit label, `/U`.
    #[getter]
    fn unit(&self) -> &str {
        &self.inner.unit
    }

    /// The factor that converts the previous unit in the chain to this
    /// one, `/C`.
    #[getter]
    fn conversion(&self) -> f64 {
        self.inner.conversion
    }

    /// How the fractional part is shown, `/F`: `"decimal"`, `"fraction"`,
    /// `"round"` or `"truncate"`.
    #[getter]
    fn fraction(&self) -> &'static str {
        fraction_format_str(self.inner.fraction)
    }

    /// The precision or denominator, `/D`, as the fraction format reads it.
    #[getter]
    fn precision(&self) -> u32 {
        self.inner.precision
    }

    /// Whether a fraction keeps the denominator as written instead of
    /// reducing it, `/FD`.
    #[getter]
    fn fixed_denominator(&self) -> bool {
        self.inner.fixed_denominator
    }

    /// The thousands separator, `/RT`.
    #[getter]
    fn thousands(&self) -> &str {
        &self.inner.thousands
    }

    /// The decimal point, `/RD`.
    #[getter]
    fn radix(&self) -> &str {
        &self.inner.radix
    }

    /// The text between the label and the value when the label precedes,
    /// `/PS`.
    #[getter]
    fn prefix_spacing(&self) -> &str {
        &self.inner.prefix_spacing
    }

    /// The text between the value and the label when the label follows,
    /// `/SS`.
    #[getter]
    fn suffix_spacing(&self) -> &str {
        &self.inner.suffix_spacing
    }

    /// Where the label goes, `/O`: `"suffix"` or `"prefix"`.
    #[getter]
    fn label(&self) -> &'static str {
        label_position_str(self.inner.label)
    }

    fn __repr__(&self) -> String {
        format!(
            "NumberFormat(unit={}, conversion={:?})",
            repr_str(&self.inner.unit),
            self.inner.conversion
        )
    }
}

/// A page's separation dictionary: what a page that is one colour
/// separation of a composite page prints, and the other pages of the same
/// separation set.
#[pyclass(frozen)]
pub(crate) struct SeparationInfo {
    inner: CoreSeparationInfo,
    pages: Vec<Option<usize>>,
}

impl SeparationInfo {
    pub(crate) fn new(inner: CoreSeparationInfo, index: &PageIndex) -> SeparationInfo {
        let pages = inner
            .pages
            .iter()
            .map(|page_ref| index.get(page_ref).copied())
            .collect();
        SeparationInfo { inner, pages }
    }
}

#[pymethods]
impl SeparationInfo {
    /// The 0-based indices of the pages in the separation set, this page
    /// among them, in the order written; `None` for a reference that names
    /// no page of this document.
    #[getter]
    fn pages(&self) -> Vec<Option<usize>> {
        self.pages.clone()
    }

    /// The `(num, gen)` references of the pages in the separation set, as
    /// written.
    #[getter]
    fn page_refs(&self) -> Vec<(u32, u16)> {
        self.inner.pages.iter().copied().map(ref_tuple).collect()
    }

    /// The colorant this page prints, `/DeviceColorant`.
    #[getter]
    fn device_colorant(&self) -> &str {
        &self.inner.device_colorant
    }

    /// The Separation or DeviceN colour space array whose tint transform
    /// approximates the colorant on a display, `/ColorSpace`, as plain
    /// Python data; `None` without one.
    #[getter]
    fn color_space<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner
            .color_space
            .as_ref()
            .map(|items| object_to_py(py, &Object::Array(items.clone())))
            .transpose()
    }

    fn __repr__(&self) -> String {
        format!(
            "SeparationInfo(device_colorant={}, pages={})",
            repr_str(&self.inner.device_colorant),
            self.inner.pages.len()
        )
    }
}

/// The usage application event a Python string names: `"view"`,
/// `"print"` or `"export"`, or `None` for the default configuration alone.
pub(crate) fn event_from_py(event: Option<&str>) -> PyResult<Option<OcEvent>> {
    match event {
        None => Ok(None),
        Some("view") => Ok(Some(OcEvent::View)),
        Some("print") => Ok(Some(OcEvent::Print)),
        Some("export") => Ok(Some(OcEvent::Export)),
        Some(other) => Err(PyValueError::new_err(format!(
            "event must be \"view\", \"print\", \"export\" or None, not {other:?}"
        ))),
    }
}

/// One optional content group (a PDF layer) with its state under the
/// usage application event it was read for.
#[pyclass(frozen)]
pub(crate) struct OptionalContentGroup {
    inner: CoreOcGroup,
}

impl From<CoreOcGroup> for OptionalContentGroup {
    fn from(inner: CoreOcGroup) -> OptionalContentGroup {
        OptionalContentGroup { inner }
    }
}

#[pymethods]
impl OptionalContentGroup {
    /// The group dictionary's `(num, gen)` reference, its identity in
    /// `/OC` entries and the configuration.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> (u32, u16) {
        ref_tuple(self.inner.reference)
    }

    /// The group's name for a user interface, `/Name`.
    #[getter]
    fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// The group's intents, `/Intent`: `"View"`, `"Design"`, or names an
    /// extension defines; `["View"]` when absent.
    #[getter]
    fn intent(&self) -> Vec<String> {
        self.inner.intent.clone()
    }

    /// The group's usage dictionary, every field `None` when absent.
    #[getter]
    fn usage(&self) -> OptionalContentUsage {
        OptionalContentUsage {
            inner: self.inner.usage.clone(),
        }
    }

    /// Whether the group is on under the state it was read for.
    #[getter]
    fn visible(&self) -> bool {
        self.inner.visible
    }

    fn __repr__(&self) -> String {
        format!(
            "OptionalContentGroup(name={}, visible={})",
            repr_opt_str(self.inner.name.as_deref()),
            repr_bool(self.inner.visible)
        )
    }
}

/// An optional content usage dictionary: what a group's content is for.
#[pyclass(frozen)]
pub(crate) struct OptionalContentUsage {
    inner: CoreOcUsage,
}

/// An optional bool as Python's `repr` writes it.
fn repr_opt_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(value) => repr_bool(value),
        None => "None",
    }
}

#[pymethods]
impl OptionalContentUsage {
    /// `/View /ViewState`: whether the group should be on when the
    /// document is opened on screen.
    #[getter]
    fn view(&self) -> Option<bool> {
        self.inner.view
    }

    /// `/Print /PrintState`: whether the group should be on when printed.
    #[getter]
    fn print(&self) -> Option<bool> {
        self.inner.print
    }

    /// `/Print /Subtype`: the kind of print content, such as
    /// `"Watermark"`, `"Trapping"` or `"PrintersMarks"`.
    #[getter]
    fn print_subtype(&self) -> Option<&str> {
        self.inner.print_subtype.as_deref()
    }

    /// `/Export /ExportState`: whether the group should be on when
    /// exported to a format without optional content.
    #[getter]
    fn export(&self) -> Option<bool> {
        self.inner.export
    }

    /// `/Zoom /min`: the magnification the group is on from.
    #[getter]
    fn zoom_min(&self) -> Option<f64> {
        self.inner.zoom_min
    }

    /// `/Zoom /max`: the magnification below which the group is on.
    #[getter]
    fn zoom_max(&self) -> Option<f64> {
        self.inner.zoom_max
    }

    /// `/Language /Lang`: the content's language tag, such as `"es-MX"`.
    #[getter]
    fn language(&self) -> Option<&str> {
        self.inner.language.as_deref()
    }

    /// `/Language /Preferred`: whether the group is preferred on a partial
    /// language match.
    #[getter]
    fn language_preferred(&self) -> bool {
        self.inner.language_preferred
    }

    /// `/PageElement /Subtype`: `"HF"` (header or footer), `"FG"`, `"BG"`
    /// or `"L"` (logo).
    #[getter]
    fn page_element(&self) -> Option<&str> {
        self.inner.page_element.as_deref()
    }

    /// `/CreatorInfo /Creator`: the application that created the group.
    #[getter]
    fn creator(&self) -> Option<&str> {
        self.inner.creator.as_deref()
    }

    /// `/CreatorInfo /Subtype`: the kind of content, such as `"Artwork"`
    /// or `"Technical"`.
    #[getter]
    fn creator_subtype(&self) -> Option<&str> {
        self.inner.creator_subtype.as_deref()
    }

    /// `/User /Type`: `"Ind"`, `"Ttl"` or `"Org"`.
    #[getter]
    fn user_type(&self) -> Option<&str> {
        self.inner.user_type.as_deref()
    }

    /// `/User /Name`: the individuals, titles or organizations named.
    #[getter]
    fn user_names(&self) -> Vec<String> {
        self.inner.user_names.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "OptionalContentUsage(view={}, print={}, export={})",
            repr_opt_bool(self.inner.view),
            repr_opt_bool(self.inner.print),
            repr_opt_bool(self.inner.export)
        )
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Linearization>()?;
    module.add_class::<OutputIntent>()?;
    module.add_class::<OptionalContentGroup>()?;
    module.add_class::<OptionalContentUsage>()?;
    module.add_class::<PagePiece>()?;
    module.add_class::<Thumbnail>()?;
    module.add_class::<Bead>()?;
    module.add_class::<ArticleThread>()?;
    module.add_class::<Transition>()?;
    module.add_class::<Presentation>()?;
    module.add_class::<PermissionHandlers>()?;
    module.add_class::<Requirement>()?;
    module.add_class::<RequirementHandler>()?;
    module.add_class::<LegalAttestation>()?;
    module.add_class::<Viewport>()?;
    module.add_class::<Measure>()?;
    module.add_class::<NumberFormat>()?;
    module.add_class::<SeparationInfo>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{dimension_str, direction_angle, motion_str, transition_style_str};
    use pdfboss_core::{Dimension, Motion, TransitionDirection, TransitionStyle};

    #[test]
    fn enumerations_are_kebab_case_strings() {
        assert_eq!(transition_style_str(TransitionStyle::Replace), "replace");
        assert_eq!(transition_style_str(TransitionStyle::Uncover), "uncover");
        assert_eq!(dimension_str(Dimension::Vertical), "vertical");
        assert_eq!(motion_str(Motion::Outward), "outward");
        assert_eq!(direction_angle(TransitionDirection::Angle(270)), Some(270));
        assert_eq!(direction_angle(TransitionDirection::None), None);
    }
}
