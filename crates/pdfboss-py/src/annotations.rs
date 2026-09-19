//! Annotations and actions as Python sees them: a page's annotation
//! dictionaries with their markup entries, reply states, link targets and
//! attached files, and action dictionaries with their `/Next` chains and
//! trigger events, as frozen classes over the core types. Enumerations are
//! kebab-case strings, object references `(num, gen)` tuples, dates ISO
//! 8601 strings, and an action's `kind` is its `/S` name as written.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use pyo3::IntoPyObjectExt;

use pdfboss_core::{
    Action as CoreAction, ActionKind, Annotation as CoreAnnotation,
    AnnotationFlags as CoreAnnotationFlags, Border as CoreBorder, FileSpec as CoreFileSpec,
    Markup as CoreMarkup, Relationship, ReplyType, Target as CoreTarget, TargetAnnotation,
    TargetPage, Trigger, TriggeredAction as CoreTriggeredAction,
    WindowsLaunch as CoreWindowsLaunch,
};

use crate::catalog::{Destination, PageIndex};
use crate::{dict_to_py, rect_tuple, ref_tuple, repr_bool, repr_opt, repr_opt_str, repr_str};

fn trigger_str(trigger: Trigger) -> &'static str {
    match trigger {
        Trigger::CursorEnter => "cursor-enter",
        Trigger::CursorExit => "cursor-exit",
        Trigger::MouseDown => "mouse-down",
        Trigger::MouseUp => "mouse-up",
        Trigger::Focus => "focus",
        Trigger::Blur => "blur",
        Trigger::PageOpen => "page-open",
        Trigger::PageClose => "page-close",
        Trigger::PageVisible => "page-visible",
        Trigger::PageInvisible => "page-invisible",
        Trigger::Keystroke => "keystroke",
        Trigger::Format => "format",
        Trigger::Validate => "validate",
        Trigger::Calculate => "calculate",
        Trigger::Open => "open",
        Trigger::Close => "close",
        Trigger::WillClose => "will-close",
        Trigger::WillSave => "will-save",
        Trigger::DidSave => "did-save",
        Trigger::WillPrint => "will-print",
        Trigger::DidPrint => "did-print",
    }
}

fn reply_type_str(reply_type: ReplyType) -> &'static str {
    match reply_type {
        ReplyType::Reply => "reply",
        ReplyType::Group => "group",
    }
}

fn relationship_str(relationship: Relationship) -> &'static str {
    match relationship {
        Relationship::Parent => "parent",
        Relationship::Child => "child",
    }
}

/// The name an action's `/S` entry is written as, for the typed kinds.
fn kind_name(kind: &ActionKind) -> &str {
    match kind {
        ActionKind::GoTo { .. } => "GoTo",
        ActionKind::GoToR { .. } => "GoToR",
        ActionKind::GoToE { .. } => "GoToE",
        ActionKind::Launch { .. } => "Launch",
        ActionKind::Uri { .. } => "URI",
        ActionKind::Named { .. } => "Named",
        ActionKind::JavaScript { .. } => "JavaScript",
        ActionKind::Other { kind, .. } => kind,
    }
}

/// A file specification (ISO 32000-1 7.11.3): the file an action or a
/// file attachment annotation names, embedded or external.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct FileSpec {
    pub(crate) inner: CoreFileSpec,
}

impl From<CoreFileSpec> for FileSpec {
    fn from(inner: CoreFileSpec) -> FileSpec {
        FileSpec { inner }
    }
}

#[pymethods]
impl FileSpec {
    /// The name to show: the Unicode file name, else the file
    /// specification string decoded; empty when the specification gives
    /// none.
    #[getter]
    fn name(&self) -> String {
        self.inner.name()
    }

    /// The file specification string as written, `/F`, or the whole
    /// specification when it was a bare string.
    #[getter]
    fn file<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner.file.as_deref().map(|f| PyBytes::new(py, f))
    }

    /// The Unicode form of the specification, `/UF`.
    #[getter]
    fn unicode_file(&self) -> Option<&str> {
        self.inner.unicode_file.as_deref()
    }

    /// The description shown next to the file, `/Desc`.
    #[getter]
    fn description(&self) -> Option<&str> {
        self.inner.description.as_deref()
    }

    /// The file system that interprets the specification, `URL` being the
    /// one the standard defines.
    #[getter]
    fn file_system(&self) -> Option<&str> {
        self.inner.file_system.as_deref()
    }

    /// The URL a `URL` specification names (ISO 32000-1 7.11.5); `None`
    /// for any other file system.
    #[getter]
    fn url(&self) -> Option<String> {
        self.inner.url()
    }

    /// Whether the file changes often enough that it must not be cached.
    #[getter]
    fn volatile(&self) -> bool {
        self.inner.volatile
    }

    /// The embedded file stream's `(num, gen)` reference; `None` when the
    /// specification embeds no stream. `Document.file_spec_data` decodes
    /// it.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> Option<(u32, u16)> {
        self.inner.embedded.map(ref_tuple)
    }

    fn __repr__(&self) -> String {
        format!(
            "FileSpec(name={}, embedded={})",
            repr_str(&self.inner.name()),
            repr_bool(self.inner.embedded.is_some())
        )
    }
}

/// The `/F` flag word of an annotation (ISO 32000-1 12.5.3, Table 165),
/// one boolean per flag.
#[pyclass(frozen)]
#[derive(Clone, Copy)]
pub(crate) struct AnnotationFlags {
    inner: CoreAnnotationFlags,
}

#[pymethods]
impl AnnotationFlags {
    /// The flag word as written, 0 when absent.
    #[getter]
    fn value(&self) -> u32 {
        self.inner.0
    }

    #[getter]
    fn invisible(&self) -> bool {
        self.inner.invisible()
    }

    #[getter]
    fn hidden(&self) -> bool {
        self.inner.hidden()
    }

    #[getter]
    fn print(&self) -> bool {
        self.inner.print()
    }

    #[getter]
    fn no_zoom(&self) -> bool {
        self.inner.no_zoom()
    }

    #[getter]
    fn no_rotate(&self) -> bool {
        self.inner.no_rotate()
    }

    #[getter]
    fn no_view(&self) -> bool {
        self.inner.no_view()
    }

    #[getter]
    fn read_only(&self) -> bool {
        self.inner.read_only()
    }

    #[getter]
    fn locked(&self) -> bool {
        self.inner.locked()
    }

    #[getter]
    fn toggle_no_view(&self) -> bool {
        self.inner.toggle_no_view()
    }

    #[getter]
    fn locked_contents(&self) -> bool {
        self.inner.locked_contents()
    }

    fn __repr__(&self) -> String {
        format!("AnnotationFlags({})", self.inner.0)
    }
}

/// An annotation's `/Border` array (ISO 32000-1 12.5.2): the corner radii
/// and width of the border, with its dash array.
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct Border {
    inner: CoreBorder,
}

#[pymethods]
impl Border {
    #[getter]
    fn horizontal_radius(&self) -> f32 {
        self.inner.horizontal_radius
    }

    #[getter]
    fn vertical_radius(&self) -> f32 {
        self.inner.vertical_radius
    }

    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

    /// The dash array in the graphics state's form; `None` for a solid
    /// border.
    #[getter]
    fn dash(&self) -> Option<Vec<f32>> {
        self.inner.dash.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "Border(width={:?}, dash={})",
            self.inner.width,
            repr_opt(self.inner.dash.as_ref().map(|d| format!("{d:?}")))
        )
    }
}

/// The entries every markup annotation may carry (ISO 32000-1 12.5.6.2,
/// Table 170).
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct Markup {
    inner: CoreMarkup,
}

#[pymethods]
impl Markup {
    /// The pop-up window's title, by convention the author, `/T`.
    #[getter]
    fn title(&self) -> Option<&str> {
        self.inner.title.as_deref()
    }

    /// The pop-up annotation's `(num, gen)` reference, `/Popup`.
    #[getter]
    fn popup(&self) -> Option<(u32, u16)> {
        self.inner.popup.map(ref_tuple)
    }

    /// The constant opacity an appearance pdfboss builds is painted with,
    /// `/CA`; 1 by default.
    #[getter]
    fn opacity(&self) -> f32 {
        self.inner.opacity
    }

    /// The rich text shown in the pop-up window, `/RC`, decoded.
    #[getter]
    fn rich_contents(&self) -> Option<&str> {
        self.inner.rich_contents.as_deref()
    }

    /// The creation date as an ISO 8601 string.
    #[getter]
    fn created(&self) -> Option<String> {
        self.inner.created.map(|date| date.to_iso8601())
    }

    /// The `(num, gen)` reference of the annotation this one replies to,
    /// `/IRT`.
    #[getter]
    fn in_reply_to(&self) -> Option<(u32, u16)> {
        self.inner.in_reply_to.map(ref_tuple)
    }

    /// A short description of the subject, `/Subj`.
    #[getter]
    fn subject(&self) -> Option<&str> {
        self.inner.subject.as_deref()
    }

    /// How this annotation relates to the one it replies to: `"reply"` or
    /// `"group"`.
    #[getter]
    fn reply_type(&self) -> &'static str {
        reply_type_str(self.inner.reply_type)
    }

    /// The intent that refines the subtype's behaviour, `/IT`, as written.
    #[getter]
    fn intent(&self) -> Option<&str> {
        self.inner.intent.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "Markup(title={}, subject={}, reply_type={})",
            repr_opt_str(self.inner.title.as_deref()),
            repr_opt_str(self.inner.subject.as_deref()),
            repr_str(reply_type_str(self.inner.reply_type))
        )
    }
}

/// The Windows launch parameters of a launch action (ISO 32000-1
/// 12.6.4.5, Table 204).
#[pyclass(frozen)]
#[derive(Clone)]
pub(crate) struct WindowsLaunch {
    inner: CoreWindowsLaunch,
}

#[pymethods]
impl WindowsLaunch {
    /// The application or document, a plain Windows path.
    #[getter]
    fn file<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.file)
    }

    /// The default directory.
    #[getter]
    fn directory<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner.directory.as_deref().map(|d| PyBytes::new(py, d))
    }

    /// `"open"` (the default) or `"print"`.
    #[getter]
    fn operation(&self) -> &str {
        &self.inner.operation
    }

    /// The parameter string passed to the application.
    #[getter]
    fn parameters<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner
            .parameters
            .as_deref()
            .map(|p| PyBytes::new(py, p))
    }

    fn __repr__(&self) -> String {
        format!(
            "WindowsLaunch(file={}, operation={})",
            repr_str(&String::from_utf8_lossy(&self.inner.file)),
            repr_str(&self.inner.operation)
        )
    }
}

/// One step of the path an embedded go-to action follows to its target
/// document (ISO 32000-1 12.6.4.4, Table 202).
#[pyclass(frozen)]
pub(crate) struct Target {
    inner: CoreTarget,
    next: Option<Py<Target>>,
}

impl Target {
    fn new(py: Python<'_>, inner: CoreTarget) -> PyResult<Target> {
        let next = match inner.next.as_deref() {
            Some(next) => Some(Py::new(py, Target::new(py, next.clone())?)?),
            None => None,
        };
        Ok(Target { inner, next })
    }
}

#[pymethods]
impl Target {
    /// `"parent"` or `"child"`: which way this step goes.
    #[getter]
    fn relationship(&self) -> &'static str {
        relationship_str(self.inner.relationship)
    }

    /// The child's name in the embedded files name tree, `/N`.
    #[getter]
    fn name<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        self.inner.name.as_deref().map(|n| PyBytes::new(py, n))
    }

    /// The page whose file attachment annotation holds the child, `/P`: a
    /// 0-based page number, or the bytes of a named destination.
    #[getter]
    fn page<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.inner.page {
            Some(TargetPage::Number(n)) => n.into_bound_py_any(py),
            Some(TargetPage::Named(name)) => Ok(PyBytes::new(py, name).into_any()),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// Which annotation of that page holds the child, `/A`: its 0-based
    /// index in the page's annotations, or its name.
    #[getter]
    fn annotation<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.inner.annotation {
            Some(TargetAnnotation::Index(n)) => n.into_bound_py_any(py),
            Some(TargetAnnotation::Name(name)) => name.into_bound_py_any(py),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// The next step; `None` when this step's document is the target.
    #[getter]
    fn next(&self, py: Python<'_>) -> Option<Py<Target>> {
        self.next.as_ref().map(|next| next.clone_ref(py))
    }

    fn __repr__(&self) -> String {
        format!(
            "Target(relationship={}, next={})",
            repr_str(relationship_str(self.inner.relationship)),
            repr_bool(self.next.is_some())
        )
    }
}

/// One action dictionary (ISO 32000-1 12.6.2) with the actions its `/Next`
/// entry chains after it. `kind` is the `/S` name as written; the go-to
/// family, launch, URI, named and JavaScript actions fill their own
/// attributes, every other kind keeps its dictionary in `entries`. No
/// action is ever executed.
#[pyclass(frozen)]
pub(crate) struct Action {
    inner: CoreAction,
    destination: Option<Destination>,
    file: Option<FileSpec>,
    target: Option<Py<Target>>,
    windows: Option<WindowsLaunch>,
    next: Vec<Py<Action>>,
}

impl Action {
    pub(crate) fn new(py: Python<'_>, inner: CoreAction, pages: &PageIndex) -> PyResult<Action> {
        let (destination, file, target, windows) = match &inner.kind {
            ActionKind::GoTo { destination, .. } => (*destination, None, None, None),
            ActionKind::GoToR {
                file, destination, ..
            } => (*destination, file.clone(), None, None),
            ActionKind::GoToE {
                file,
                destination,
                target,
                ..
            } => (*destination, file.clone(), target.clone(), None),
            ActionKind::Launch { file, windows, .. } => (None, file.clone(), None, windows.clone()),
            _ => (None, None, None, None),
        };
        let target = match target {
            Some(target) => Some(Py::new(py, Target::new(py, target)?)?),
            None => None,
        };
        let next = inner
            .next
            .iter()
            .map(|next| Py::new(py, Action::new(py, next.clone(), pages)?))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Action {
            destination: destination.map(|d| Destination::new(d, pages)),
            file: file.map(FileSpec::from),
            target,
            windows: windows.map(|inner| WindowsLaunch { inner }),
            next,
            inner,
        })
    }
}

#[pymethods]
impl Action {
    /// The `(num, gen)` reference of the action dictionary; `None` for a
    /// direct dictionary.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> Option<(u32, u16)> {
        self.inner.object.map(ref_tuple)
    }

    /// The `/S` name as written: `"GoTo"`, `"GoToR"`, `"GoToE"`,
    /// `"Launch"`, `"URI"`, `"Named"`, `"JavaScript"`, or any other type
    /// such as `"SubmitForm"`.
    #[getter]
    fn kind(&self) -> &str {
        kind_name(&self.inner.kind)
    }

    /// The explicit destination of a go-to action; for a remote or
    /// embedded one its page is a number in the other document.
    #[getter]
    fn destination(&self) -> Option<Destination> {
        self.destination.clone()
    }

    /// The name or string a go-to action's `/D` gives; for a `GoTo` it is
    /// looked up into `destination` as well.
    #[getter]
    fn named_destination<'py>(&self, py: Python<'py>) -> Option<Bound<'py, PyBytes>> {
        let named = match &self.inner.kind {
            ActionKind::GoTo {
                named_destination, ..
            }
            | ActionKind::GoToR {
                named_destination, ..
            }
            | ActionKind::GoToE {
                named_destination, ..
            } => named_destination.as_deref(),
            _ => None,
        };
        named.map(|n| PyBytes::new(py, n))
    }

    /// The file a remote go-to, embedded go-to or launch action names.
    #[getter]
    fn file(&self) -> Option<FileSpec> {
        self.file.clone()
    }

    /// Whether the destination opens in a new window; `None` when the
    /// action leaves it to the viewer.
    #[getter]
    fn new_window(&self) -> Option<bool> {
        match &self.inner.kind {
            ActionKind::GoToR { new_window, .. }
            | ActionKind::GoToE { new_window, .. }
            | ActionKind::Launch { new_window, .. } => *new_window,
            _ => None,
        }
    }

    /// The path to an embedded go-to action's target document.
    #[getter]
    fn target(&self, py: Python<'_>) -> Option<Py<Target>> {
        self.target.as_ref().map(|t| t.clone_ref(py))
    }

    /// A launch action's Windows parameters.
    #[getter]
    fn windows(&self) -> Option<WindowsLaunch> {
        self.windows.clone()
    }

    /// A URI action's URI.
    #[getter]
    fn uri(&self) -> Option<&str> {
        match &self.inner.kind {
            ActionKind::Uri { uri, .. } => Some(uri),
            _ => None,
        }
    }

    /// Whether a URI action appends the mouse position to its URI.
    #[getter]
    fn is_map(&self) -> bool {
        matches!(&self.inner.kind, ActionKind::Uri { is_map: true, .. })
    }

    /// A named action's name, such as `"NextPage"`.
    #[getter]
    fn name(&self) -> Option<&str> {
        match &self.inner.kind {
            ActionKind::Named { name } => Some(name),
            _ => None,
        }
    }

    /// A JavaScript action's script, decoded and never run.
    #[getter]
    fn script(&self) -> Option<&str> {
        match &self.inner.kind {
            ActionKind::JavaScript { script } => script.as_deref(),
            _ => None,
        }
    }

    /// The whole dictionary of an action of any other kind, as plain
    /// Python data; `None` for the typed kinds.
    #[getter]
    fn entries<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.inner.kind {
            ActionKind::Other { entries, .. } => Ok(Some(dict_to_py(py, entries)?)),
            _ => Ok(None),
        }
    }

    /// The actions performed after this one, in order, each with its own
    /// chain.
    #[getter]
    fn next(&self, py: Python<'_>) -> Vec<Py<Action>> {
        self.next.iter().map(|next| next.clone_ref(py)).collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Action(kind={}, next={})",
            repr_str(kind_name(&self.inner.kind)),
            self.next.len()
        )
    }
}

/// One entry of an additional-actions dictionary (ISO 32000-1 12.6.3):
/// the trigger event and the action it fires.
#[pyclass(frozen)]
pub(crate) struct TriggeredAction {
    trigger: Trigger,
    action: Py<Action>,
}

impl TriggeredAction {
    pub(crate) fn new(
        py: Python<'_>,
        inner: CoreTriggeredAction,
        pages: &PageIndex,
    ) -> PyResult<TriggeredAction> {
        Ok(TriggeredAction {
            trigger: inner.trigger,
            action: Py::new(py, Action::new(py, inner.action, pages)?)?,
        })
    }
}

/// Every triggered action of `inner`, as Python objects.
pub(crate) fn triggered_actions(
    py: Python<'_>,
    inner: Vec<CoreTriggeredAction>,
    pages: &PageIndex,
) -> PyResult<Vec<TriggeredAction>> {
    inner
        .into_iter()
        .map(|action| TriggeredAction::new(py, action, pages))
        .collect()
}

#[pymethods]
impl TriggeredAction {
    /// The event, kebab-case: `"cursor-enter"`, `"cursor-exit"`,
    /// `"mouse-down"`, `"mouse-up"`, `"focus"`, `"blur"`, `"page-open"`,
    /// `"page-close"`, `"page-visible"`, `"page-invisible"`,
    /// `"keystroke"`, `"format"`, `"validate"`, `"calculate"` on an
    /// annotation; `"open"`, `"close"` on a page; `"will-close"`,
    /// `"will-save"`, `"did-save"`, `"will-print"`, `"did-print"` on the
    /// document.
    #[getter]
    fn trigger(&self) -> &'static str {
        trigger_str(self.trigger)
    }

    /// The action the event fires.
    #[getter]
    fn action(&self, py: Python<'_>) -> Py<Action> {
        self.action.clone_ref(py)
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        format!(
            "TriggeredAction(trigger={}, action={})",
            repr_str(trigger_str(self.trigger)),
            self.action.borrow(py).__repr__()
        )
    }
}

/// One annotation of a page (ISO 32000-1 12.5.2, Table 164) with the
/// entries of the subtypes pdfboss reads beyond the common ones; returned
/// by `Page.annotations`.
#[pyclass(frozen)]
pub(crate) struct Annotation {
    inner: CoreAnnotation,
    destination: Option<Destination>,
    action: Option<Py<Action>>,
    additional_actions: Vec<Py<TriggeredAction>>,
}

impl Annotation {
    pub(crate) fn new(
        py: Python<'_>,
        inner: CoreAnnotation,
        pages: &PageIndex,
    ) -> PyResult<Annotation> {
        let action = match inner.action.clone() {
            Some(action) => Some(Py::new(py, Action::new(py, action, pages)?)?),
            None => None,
        };
        let additional_actions = inner
            .additional_actions
            .iter()
            .map(|action| Py::new(py, TriggeredAction::new(py, action.clone(), pages)?))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Annotation {
            destination: inner.destination.map(|d| Destination::new(d, pages)),
            action,
            additional_actions,
            inner,
        })
    }
}

/// Every annotation of `inner`, as Python objects.
pub(crate) fn annotations(
    py: Python<'_>,
    inner: Vec<CoreAnnotation>,
    pages: &PageIndex,
) -> PyResult<Vec<Annotation>> {
    inner
        .into_iter()
        .map(|annotation| Annotation::new(py, annotation, pages))
        .collect()
}

#[pymethods]
impl Annotation {
    /// The `(num, gen)` reference of the annotation dictionary; `None`
    /// for a direct dictionary in the page's `/Annots`.
    #[getter]
    #[pyo3(name = "ref")]
    fn object_ref(&self) -> Option<(u32, u16)> {
        self.inner.object.map(ref_tuple)
    }

    /// The annotation type as written, `/Subtype`: `"Link"`, `"Text"`,
    /// `"Widget"`, `"Highlight"` and the rest of Table 169.
    #[getter]
    fn subtype(&self) -> &str {
        &self.inner.subtype
    }

    /// The annotation rectangle `(x0, y0, x1, y1)` in default user space,
    /// normalized; `None` when missing or malformed.
    #[getter]
    fn rect(&self) -> Option<(f32, f32, f32, f32)> {
        self.inner.rect.map(rect_tuple)
    }

    /// The annotation's text, or an alternate description, `/Contents`.
    #[getter]
    fn contents(&self) -> Option<&str> {
        self.inner.contents.as_deref()
    }

    /// The `(num, gen)` reference of the page the annotation belongs to,
    /// `/P`.
    #[getter]
    fn page_ref(&self) -> Option<(u32, u16)> {
        self.inner.page.map(ref_tuple)
    }

    /// The name unique among the page's annotations, `/NM`.
    #[getter]
    fn name(&self) -> Option<&str> {
        self.inner.name.as_deref()
    }

    /// When the annotation last changed, `/M`, as written.
    #[getter]
    fn modified(&self) -> Option<&str> {
        self.inner.modified.as_deref()
    }

    /// `/M` as an ISO 8601 string when it parses as a date; `None` when it
    /// is in another format.
    #[getter]
    fn modified_date(&self) -> Option<String> {
        self.inner.modified_date.map(|date| date.to_iso8601())
    }

    /// The flag word, one boolean per flag.
    #[getter]
    fn flags(&self) -> AnnotationFlags {
        AnnotationFlags {
            inner: self.inner.flags,
        }
    }

    /// Whether an appearance dictionary with a normal appearance is
    /// present, `/AP /N`.
    #[getter]
    fn has_appearance(&self) -> bool {
        self.inner.has_appearance
    }

    /// The appearance state that picks the appearance stream, `/AS`.
    #[getter]
    fn appearance_state(&self) -> Option<&str> {
        self.inner.appearance_state.as_deref()
    }

    /// The border array as written; `None` when absent (the standard's
    /// default is a 1-point solid border) or malformed.
    #[getter]
    fn border(&self) -> Option<Border> {
        self.inner.border.clone().map(|inner| Border { inner })
    }

    /// The colour of the icon background, pop-up title bar or link
    /// border, `/C`: 0 (transparent), 1, 3 or 4 components as written.
    #[getter]
    fn color(&self) -> Option<Vec<f32>> {
        self.inner.color.clone()
    }

    /// The key under which the structure tree's parent tree lists the
    /// annotation, `/StructParent`.
    #[getter]
    fn struct_parent(&self) -> Option<i64> {
        self.inner.struct_parent
    }

    /// The `(num, gen)` reference of the optional content group or
    /// membership dictionary that controls visibility, `/OC`.
    #[getter]
    fn optional_content(&self) -> Option<(u32, u16)> {
        self.inner.optional_content.map(ref_tuple)
    }

    /// The markup entries; `None` for a subtype that is not a markup
    /// annotation (Link, Popup, Widget and the like).
    #[getter]
    fn markup(&self) -> Option<Markup> {
        self.inner.markup.clone().map(|inner| Markup { inner })
    }

    /// The state a text annotation sets on the annotation it replies to,
    /// or the state model's default; `None` when it sets none.
    #[getter]
    fn state(&self) -> Option<&str> {
        self.inner.state.as_ref().map(|s| s.state.as_str())
    }

    /// The state model, `"Marked"` or `"Review"` as written.
    #[getter]
    fn state_model(&self) -> Option<&str> {
        self.inner.state.as_ref().and_then(|s| s.model.as_deref())
    }

    /// Whether a text or pop-up annotation starts open, `/Open`.
    #[getter]
    fn open(&self) -> Option<bool> {
        self.inner.open
    }

    /// The icon an annotation shows without an appearance stream, `/Name`
    /// as written; text, file attachment and sound annotations name one.
    #[getter]
    fn icon(&self) -> Option<&str> {
        self.inner.icon.as_deref()
    }

    /// The `(num, gen)` reference of the markup annotation a pop-up
    /// belongs to, `/Parent`.
    #[getter]
    fn parent(&self) -> Option<(u32, u16)> {
        self.inner.parent.map(ref_tuple)
    }

    /// The file a file attachment annotation carries, `/FS`;
    /// `Document.file_spec_data` decodes an embedded one.
    #[getter]
    fn file(&self) -> Option<FileSpec> {
        self.inner.file.clone().map(FileSpec::from)
    }

    /// A link's `/Dest`, explicit or looked up by name, with the page
    /// resolved to a 0-based index.
    #[getter]
    fn destination(&self) -> Option<Destination> {
        self.destination.clone()
    }

    /// The action performed when the annotation is activated, `/A`, with
    /// its `/Next` chain.
    #[getter]
    fn action(&self, py: Python<'_>) -> Option<Py<Action>> {
        self.action.as_ref().map(|a| a.clone_ref(py))
    }

    /// The actions the annotation's trigger events fire, `/AA`, in the
    /// order of Tables 194 and 196.
    #[getter]
    fn additional_actions(&self, py: Python<'_>) -> Vec<Py<TriggeredAction>> {
        self.additional_actions
            .iter()
            .map(|a| a.clone_ref(py))
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Annotation(subtype={}, rect={}, contents={})",
            repr_str(&self.inner.subtype),
            repr_opt(self.inner.rect.map(|r| format!("{:?}", rect_tuple(r)))),
            repr_opt_str(self.inner.contents.as_deref())
        )
    }
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Annotation>()?;
    module.add_class::<AnnotationFlags>()?;
    module.add_class::<Border>()?;
    module.add_class::<Markup>()?;
    module.add_class::<FileSpec>()?;
    module.add_class::<Action>()?;
    module.add_class::<Target>()?;
    module.add_class::<WindowsLaunch>()?;
    module.add_class::<TriggeredAction>()?;
    Ok(())
}
