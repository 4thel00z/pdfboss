//! Document-level structures read from the catalog as Python sees them:
//! the outline, destinations, page labels, embedded files, viewer
//! preferences and developer extensions, as frozen classes over the core
//! types. Enumerations are kebab-case strings and object references
//! `(num, gen)` tuples.

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use pdfboss_core::{
    Destination as CoreDestination, DestinationPage, DeveloperExtension as CoreExtension,
    Direction, Duplex, EmbeddedFile as CoreEmbeddedFile, FastMap, Fit, LabelStyle,
    NonFullScreenPageMode, ObjRef, OutlineItem as CoreOutlineItem, PageBoundary,
    PageLabel as CorePageLabel, PrintScaling, ViewerPreferences as CoreViewerPreferences,
};

use crate::{ref_tuple, repr_opt, repr_opt_str, repr_str};

/// Each page's object reference mapped to its 0-based index, for turning
/// a destination's page reference into a page number.
pub(crate) type PageIndex = FastMap<ObjRef, usize>;

/// Builds the [`PageIndex`] of a document with `count` pages from the
/// reference of the page at each index; a page inlined into the page tree
/// without an object of its own has none and is left out.
pub(crate) fn page_index(count: usize, page_ref: impl Fn(usize) -> Option<ObjRef>) -> PageIndex {
    (0..count)
        .filter_map(|index| Some((page_ref(index)?, index)))
        .collect()
}

fn label_style_str(style: LabelStyle) -> &'static str {
    match style {
        LabelStyle::Decimal => "decimal",
        LabelStyle::RomanUpper => "roman-upper",
        LabelStyle::RomanLower => "roman-lower",
        LabelStyle::LettersUpper => "letters-upper",
        LabelStyle::LettersLower => "letters-lower",
    }
}

fn fit_str(fit: Fit) -> &'static str {
    match fit {
        Fit::Xyz { .. } => "xyz",
        Fit::Fit => "fit",
        Fit::FitH { .. } => "fit-h",
        Fit::FitV { .. } => "fit-v",
        Fit::FitR { .. } => "fit-r",
        Fit::FitB => "fit-b",
        Fit::FitBH { .. } => "fit-bh",
        Fit::FitBV { .. } => "fit-bv",
    }
}

fn page_mode_str(mode: NonFullScreenPageMode) -> &'static str {
    match mode {
        NonFullScreenPageMode::UseNone => "use-none",
        NonFullScreenPageMode::UseOutlines => "use-outlines",
        NonFullScreenPageMode::UseThumbs => "use-thumbs",
        NonFullScreenPageMode::UseOC => "use-oc",
    }
}

fn direction_str(direction: Direction) -> &'static str {
    match direction {
        Direction::LeftToRight => "left-to-right",
        Direction::RightToLeft => "right-to-left",
    }
}

fn page_boundary_str(boundary: PageBoundary) -> &'static str {
    match boundary {
        PageBoundary::MediaBox => "media-box",
        PageBoundary::CropBox => "crop-box",
        PageBoundary::BleedBox => "bleed-box",
        PageBoundary::TrimBox => "trim-box",
        PageBoundary::ArtBox => "art-box",
    }
}

fn print_scaling_str(scaling: PrintScaling) -> &'static str {
    match scaling {
        PrintScaling::None => "none",
        PrintScaling::AppDefault => "app-default",
    }
}

fn duplex_str(duplex: Duplex) -> &'static str {
    match duplex {
        Duplex::Simplex => "simplex",
        Duplex::DuplexFlipShortEdge => "duplex-flip-short-edge",
        Duplex::DuplexFlipLongEdge => "duplex-flip-long-edge",
    }
}

/// One explicit destination: a page and how the viewer shows it.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct Destination {
    inner: CoreDestination,
    page: Option<usize>,
}

impl Destination {
    pub(crate) fn new(inner: CoreDestination, pages: &PageIndex) -> Destination {
        let page = match inner.page {
            DestinationPage::Object(page_ref) => pages.get(&page_ref).copied(),
            DestinationPage::Number(number) => usize::try_from(number).ok(),
        };
        Destination { inner, page }
    }
}

#[pymethods]
impl Destination {
    /// The 0-based index of the page shown: the page the destination's
    /// reference names, or the page number a remote destination gives.
    /// `None` when the reference names no page of this document.
    #[getter]
    fn page(&self) -> Option<usize> {
        self.page
    }

    /// The `(num, gen)` reference of the page shown; `None` when the
    /// destination gives a page number instead.
    #[getter]
    fn page_ref(&self) -> Option<(u32, u16)> {
        match self.inner.page {
            DestinationPage::Object(page_ref) => Some(ref_tuple(page_ref)),
            DestinationPage::Number(_) => None,
        }
    }

    /// How the page is shown: `"xyz"`, `"fit"`, `"fit-h"`, `"fit-v"`,
    /// `"fit-r"`, `"fit-b"`, `"fit-bh"` or `"fit-bv"`. The coordinates
    /// each kind takes are `left`, `top`, `right`, `bottom` and `zoom`;
    /// the others are `None`, as is a coordinate the viewer keeps.
    #[getter]
    fn fit(&self) -> &'static str {
        fit_str(self.inner.fit)
    }

    #[getter]
    fn left(&self) -> Option<f32> {
        match self.inner.fit {
            Fit::Xyz { left, .. } | Fit::FitV { left } | Fit::FitBV { left } => left,
            Fit::FitR { left, .. } => Some(left),
            _ => None,
        }
    }

    #[getter]
    fn top(&self) -> Option<f32> {
        match self.inner.fit {
            Fit::Xyz { top, .. } | Fit::FitH { top } | Fit::FitBH { top } => top,
            Fit::FitR { top, .. } => Some(top),
            _ => None,
        }
    }

    #[getter]
    fn right(&self) -> Option<f32> {
        match self.inner.fit {
            Fit::FitR { right, .. } => Some(right),
            _ => None,
        }
    }

    #[getter]
    fn bottom(&self) -> Option<f32> {
        match self.inner.fit {
            Fit::FitR { bottom, .. } => Some(bottom),
            _ => None,
        }
    }

    #[getter]
    fn zoom(&self) -> Option<f32> {
        match self.inner.fit {
            Fit::Xyz { zoom, .. } => zoom,
            _ => None,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Destination(page={}, fit={})",
            repr_opt(self.page),
            repr_str(self.fit())
        )
    }
}

/// One entry of the outline (the bookmark panel) with its children.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct OutlineItem {
    title: String,
    destination: Option<Destination>,
    open: bool,
    color: (f32, f32, f32),
    italic: bool,
    bold: bool,
    structure_element: Option<ObjRef>,
    children: Vec<OutlineItem>,
}

/// Converts the core outline into Python items, resolving each
/// destination's page through `pages`.
pub(crate) fn outline_items(items: Vec<CoreOutlineItem>, pages: &PageIndex) -> Vec<OutlineItem> {
    items
        .into_iter()
        .map(|item| OutlineItem {
            title: item.title,
            destination: item
                .destination
                .map(|destination| Destination::new(destination, pages)),
            open: item.open,
            color: (item.color[0], item.color[1], item.color[2]),
            italic: item.italic,
            bold: item.bold,
            structure_element: item.structure_element,
            children: outline_items(item.children, pages),
        })
        .collect()
}

#[pymethods]
impl OutlineItem {
    /// The title shown in the panel.
    #[getter]
    fn title(&self) -> &str {
        &self.title
    }

    /// Where activating the item goes; `None` for an item whose action is
    /// not a go-to within the document.
    #[getter]
    fn destination(&self) -> Option<Destination> {
        self.destination.clone()
    }

    /// The 0-based index of the destination's page, a shortcut for
    /// `destination.page`; `None` without one.
    #[getter]
    fn page(&self) -> Option<usize> {
        self.destination.as_ref()?.page
    }

    /// Whether the item shows its children.
    #[getter]
    fn open(&self) -> bool {
        self.open
    }

    /// The title's `(r, g, b)` colour in 0 to 1; black by default.
    #[getter]
    fn color(&self) -> (f32, f32, f32) {
        self.color
    }

    #[getter]
    fn italic(&self) -> bool {
        self.italic
    }

    #[getter]
    fn bold(&self) -> bool {
        self.bold
    }

    /// The structure element the item refers to as a `(num, gen)`
    /// reference.
    #[getter]
    fn structure_element(&self) -> Option<(u32, u16)> {
        self.structure_element.map(ref_tuple)
    }

    /// The item's children, in panel order.
    #[getter]
    fn children(&self) -> Vec<OutlineItem> {
        self.children.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "OutlineItem(title={}, page={}, children={})",
            repr_str(&self.title),
            repr_opt(self.page()),
            self.children.len()
        )
    }
}

/// One page-numbering range: how the pages from `first_page` on are
/// labelled until the next range starts.
#[pyclass(frozen)]
pub(crate) struct PageLabel {
    inner: CorePageLabel,
}

impl From<CorePageLabel> for PageLabel {
    fn from(inner: CorePageLabel) -> PageLabel {
        PageLabel { inner }
    }
}

#[pymethods]
impl PageLabel {
    /// The 0-based index of the first page the range labels.
    #[getter]
    fn first_page(&self) -> usize {
        self.inner.first_page
    }

    /// The numbering style: `"decimal"`, `"roman-upper"`, `"roman-lower"`,
    /// `"letters-upper"` or `"letters-lower"`; `None` for labels that are
    /// the prefix alone.
    #[getter]
    fn style(&self) -> Option<&'static str> {
        self.inner.style.map(label_style_str)
    }

    /// The text put before each label's number.
    #[getter]
    fn prefix(&self) -> Option<&str> {
        self.inner.prefix.as_deref()
    }

    /// The number the range's first page gets.
    #[getter]
    fn start_at(&self) -> u32 {
        self.inner.start_at
    }

    /// The label of the page at 0-based `index`, which must be in the
    /// range.
    fn label(&self, index: usize) -> String {
        self.inner.label(index)
    }

    fn __repr__(&self) -> String {
        format!(
            "PageLabel(first_page={}, style={}, prefix={}, start_at={})",
            self.inner.first_page,
            repr_opt_str(self.style()),
            repr_opt_str(self.inner.prefix.as_deref()),
            self.inner.start_at
        )
    }
}

/// One file embedded at document level, an attachment.
#[pyclass(frozen)]
pub(crate) struct EmbeddedFile {
    pub(crate) inner: CoreEmbeddedFile,
}

impl From<CoreEmbeddedFile> for EmbeddedFile {
    fn from(inner: CoreEmbeddedFile) -> EmbeddedFile {
        EmbeddedFile { inner }
    }
}

#[pymethods]
impl EmbeddedFile {
    /// The name the document files the attachment under.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// The file name the specification gives, which may differ from
    /// `name`; empty when it gives none.
    #[getter]
    fn file_name(&self) -> String {
        self.inner.spec.name()
    }

    /// The description shown next to the file.
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.spec.description.as_deref()
    }

    /// The file system that interprets the specification, `URL` being the
    /// one the standard defines.
    #[getter]
    fn file_system(&self) -> Option<&str> {
        self.inner.spec.file_system.as_deref()
    }

    /// Whether the file changes often enough that it must not be cached.
    #[getter]
    fn volatile(&self) -> bool {
        self.inner.spec.volatile
    }

    /// The embedded file stream's `(num, gen)` reference; `None` when the
    /// specification embeds no stream.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> Option<(u32, u16)> {
        self.inner.spec.embedded.map(ref_tuple)
    }

    /// The file's MIME type.
    #[getter]
    fn mime(&self) -> Option<&str> {
        self.inner.mime.as_deref()
    }

    /// The uncompressed size in bytes, as declared.
    #[getter]
    fn size(&self) -> Option<u64> {
        self.inner.size
    }

    /// The creation date as an ISO 8601 string.
    #[getter]
    fn created(&self) -> Option<String> {
        self.inner.created.map(|date| date.to_iso8601())
    }

    /// The modification date as an ISO 8601 string.
    #[getter]
    fn modified(&self) -> Option<String> {
        self.inner.modified.map(|date| date.to_iso8601())
    }

    /// The MD5 digest of the uncompressed bytes, as stored.
    #[getter]
    fn checksum<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner
            .checksum
            .as_deref()
            .map(|digest| PyBytes::new(py, digest))
    }

    fn __repr__(&self) -> String {
        format!(
            "EmbeddedFile(name={}, mime={}, size={})",
            repr_str(&self.inner.name),
            repr_opt_str(self.inner.mime.as_deref()),
            repr_opt(self.inner.size)
        )
    }
}

/// The viewer preferences the catalog declares; every entry a missing
/// one defaults holds its default.
#[pyclass(frozen)]
pub(crate) struct ViewerPreferences {
    inner: CoreViewerPreferences,
}

impl From<CoreViewerPreferences> for ViewerPreferences {
    fn from(inner: CoreViewerPreferences) -> ViewerPreferences {
        ViewerPreferences { inner }
    }
}

#[pymethods]
impl ViewerPreferences {
    #[getter]
    fn hide_toolbar(&self) -> bool {
        self.inner.hide_toolbar
    }

    #[getter]
    fn hide_menubar(&self) -> bool {
        self.inner.hide_menubar
    }

    #[getter]
    fn hide_window_ui(&self) -> bool {
        self.inner.hide_window_ui
    }

    #[getter]
    fn fit_window(&self) -> bool {
        self.inner.fit_window
    }

    #[getter]
    fn center_window(&self) -> bool {
        self.inner.center_window
    }

    /// Whether the window title shows the document's title rather than
    /// its file name.
    #[getter]
    fn display_doc_title(&self) -> bool {
        self.inner.display_doc_title
    }

    /// The panel shown on leaving full-screen mode: `"use-none"`,
    /// `"use-outlines"`, `"use-thumbs"` or `"use-oc"`.
    #[getter]
    fn non_full_screen_page_mode(&self) -> &'static str {
        page_mode_str(self.inner.non_full_screen_page_mode)
    }

    /// The reading order that places pages shown side by side:
    /// `"left-to-right"` or `"right-to-left"`.
    #[getter]
    fn direction(&self) -> &'static str {
        direction_str(self.inner.direction)
    }

    /// The page box shown on screen: `"media-box"`, `"crop-box"`,
    /// `"bleed-box"`, `"trim-box"` or `"art-box"`.
    #[getter]
    fn view_area(&self) -> &'static str {
        page_boundary_str(self.inner.view_area)
    }

    /// The page box the screen view is clipped to, one of the `view_area`
    /// names.
    #[getter]
    fn view_clip(&self) -> &'static str {
        page_boundary_str(self.inner.view_clip)
    }

    /// The page box printed, one of the `view_area` names.
    #[getter]
    fn print_area(&self) -> &'static str {
        page_boundary_str(self.inner.print_area)
    }

    /// The page box printing is clipped to, one of the `view_area` names.
    #[getter]
    fn print_clip(&self) -> &'static str {
        page_boundary_str(self.inner.print_clip)
    }

    /// The page scaling the print dialog starts with: `"none"` or
    /// `"app-default"`.
    #[getter]
    fn print_scaling(&self) -> &'static str {
        print_scaling_str(self.inner.print_scaling)
    }

    /// The paper handling the print dialog starts with: `"simplex"`,
    /// `"duplex-flip-short-edge"` or `"duplex-flip-long-edge"`; `None`
    /// when left to the viewer.
    #[getter]
    fn duplex(&self) -> Option<&'static str> {
        self.inner.duplex.map(duplex_str)
    }

    /// Whether the printer picks the paper tray by the page size.
    #[getter]
    fn pick_tray_by_pdf_size(&self) -> Option<bool> {
        self.inner.pick_tray_by_pdf_size
    }

    /// The `(first, last)` 1-based page ranges the print dialog starts
    /// with.
    #[getter]
    fn print_page_range(&self) -> Vec<(u32, u32)> {
        self.inner.print_page_range.clone()
    }

    /// The number of copies the print dialog starts with.
    #[getter]
    fn num_copies(&self) -> Option<u32> {
        self.inner.num_copies
    }

    fn __repr__(&self) -> String {
        format!(
            "ViewerPreferences(direction={}, print_scaling={}, duplex={})",
            repr_str(self.direction()),
            repr_str(self.print_scaling()),
            repr_opt_str(self.duplex())
        )
    }
}

/// One developer extension the catalog declares.
#[pyclass(frozen)]
pub(crate) struct DeveloperExtension {
    inner: CoreExtension,
}

impl From<CoreExtension> for DeveloperExtension {
    fn from(inner: CoreExtension) -> DeveloperExtension {
        DeveloperExtension { inner }
    }
}

#[pymethods]
impl DeveloperExtension {
    /// The developer's registered prefix, such as `ADBE`.
    #[getter]
    fn prefix(&self) -> &str {
        &self.inner.prefix
    }

    /// The PDF version the extension builds on, such as `"1.7"`.
    #[getter]
    fn base_version(&self) -> &str {
        &self.inner.base_version
    }

    /// The developer's extension level.
    #[getter]
    fn extension_level(&self) -> i64 {
        self.inner.extension_level
    }

    fn __repr__(&self) -> String {
        format!(
            "DeveloperExtension(prefix={}, base_version={}, extension_level={})",
            repr_str(&self.inner.prefix),
            repr_str(&self.inner.base_version),
            self.inner.extension_level
        )
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Destination>()?;
    module.add_class::<OutlineItem>()?;
    module.add_class::<PageLabel>()?;
    module.add_class::<EmbeddedFile>()?;
    module.add_class::<ViewerPreferences>()?;
    module.add_class::<DeveloperExtension>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{fit_str, label_style_str, page_index, Destination};
    use pdfboss_core::{Destination as CoreDestination, DestinationPage, Fit, LabelStyle, ObjRef};

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    #[test]
    fn destinations_resolve_page_references_through_the_index() {
        let pages = page_index(3, |index| (index != 1).then(|| r(10 + index as u32)));
        let by_ref = Destination::new(
            CoreDestination {
                page: DestinationPage::Object(r(12)),
                fit: Fit::Fit,
            },
            &pages,
        );
        assert_eq!(by_ref.page, Some(2));
        assert_eq!(by_ref.page_ref(), Some((12, 0)));
        let unknown = Destination::new(
            CoreDestination {
                page: DestinationPage::Object(r(99)),
                fit: Fit::Fit,
            },
            &pages,
        );
        assert_eq!(unknown.page, None);
        let by_number = Destination::new(
            CoreDestination {
                page: DestinationPage::Number(4),
                fit: Fit::FitR {
                    left: 1.0,
                    bottom: 2.0,
                    right: 3.0,
                    top: 4.0,
                },
            },
            &pages,
        );
        assert_eq!(by_number.page, Some(4));
        assert_eq!(by_number.page_ref(), None);
        assert_eq!(
            (
                by_number.left(),
                by_number.bottom(),
                by_number.right(),
                by_number.top(),
                by_number.zoom()
            ),
            (Some(1.0), Some(2.0), Some(3.0), Some(4.0), None)
        );
    }

    #[test]
    fn enumerations_map_to_kebab_case_names() {
        assert_eq!(fit_str(Fit::FitBH { top: None }), "fit-bh");
        assert_eq!(label_style_str(LabelStyle::LettersLower), "letters-lower");
    }

    #[test]
    fn pyclasses_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::Destination>();
        assert_send_sync::<super::OutlineItem>();
        assert_send_sync::<super::PageLabel>();
        assert_send_sync::<super::EmbeddedFile>();
        assert_send_sync::<super::ViewerPreferences>();
        assert_send_sync::<super::DeveloperExtension>();
    }
}
