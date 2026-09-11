//! Document- and page-level structures as Python sees them: the
//! linearization parameter dictionary, output intents, page-piece data,
//! thumbnails, article threads, presentations and permission handlers, as
//! frozen classes over the core types. Enumerations are kebab-case strings
//! and object references `(num, gen)` tuples.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use pdfboss_core::{
    ArticleThread as CoreArticleThread, Bead as CoreBead, Dimension,
    Linearization as CoreLinearization, Motion, OutputIntent as CoreOutputIntent,
    PagePiece as CorePagePiece, PermissionHandlers as CorePermissionHandlers,
    Presentation as CorePresentation, Thumbnail as CoreThumbnail, Transition as CoreTransition,
    TransitionDirection, TransitionStyle,
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

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Linearization>()?;
    module.add_class::<OutputIntent>()?;
    module.add_class::<PagePiece>()?;
    module.add_class::<Thumbnail>()?;
    module.add_class::<Bead>()?;
    module.add_class::<ArticleThread>()?;
    module.add_class::<Transition>()?;
    module.add_class::<Presentation>()?;
    module.add_class::<PermissionHandlers>()?;
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
