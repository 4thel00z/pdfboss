//! Annotations (ISO 32000-1 §12.5) and actions (§12.6), read as data: the
//! entries every annotation dictionary shares, the markup entries, the
//! state a reply sets, a link's destination, an attachment's file
//! specification, and action dictionaries with their `/Next` chains and
//! the trigger events that name them. No action is ever executed.

use crate::article::rectangle;
use crate::date::Date;
use crate::destination::{destination_with, named_destination_with, Destination};
use crate::document::Page;
use crate::embedded_file::{bytes_entry, file_spec_with, FileSpec};
use crate::geom::Rect;
use crate::hash::FastSet;
use crate::object::{decode_text_string, Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// Maximum action dictionaries one `/A` or `/AA` entry reads, every
/// `/Next` branch included; past it the rest of the tree is dropped.
pub const MAX_ACTIONS: usize = 256;

/// Maximum nesting of target dictionaries (`/T` inside `/T`) an embedded
/// go-to action reads.
pub const MAX_TARGET_DEPTH: usize = 32;

/// The subtypes Table 169 marks as markup annotations, whose dictionaries
/// carry the Table 170 entries.
const MARKUP_SUBTYPES: [&str; 17] = [
    "Text",
    "FreeText",
    "Line",
    "Square",
    "Circle",
    "Polygon",
    "PolyLine",
    "Highlight",
    "Underline",
    "Squiggly",
    "StrikeOut",
    "Stamp",
    "Caret",
    "Ink",
    "FileAttachment",
    "Sound",
    "Redact",
];

/// The `/F` flag word of an annotation (ISO 32000-1 §12.5.3, Table 165),
/// bit 1 being the lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnnotationFlags(pub u32);

impl AnnotationFlags {
    /// Whether the flag at `position` (1-based, as Table 165 numbers them)
    /// is set.
    ///
    /// Covers ISO 32000-1 §12.5.3.
    pub fn bit(self, position: u32) -> bool {
        (1..=32).contains(&position) && self.0 & (1 << (position - 1)) != 0
    }

    /// Bit 1: do not show an annotation of an unknown subtype.
    pub fn invisible(self) -> bool {
        self.bit(1)
    }

    /// Bit 2: never show or print the annotation.
    pub fn hidden(self) -> bool {
        self.bit(2)
    }

    /// Bit 3: print the annotation with the page.
    pub fn print(self) -> bool {
        self.bit(3)
    }

    /// Bit 4: keep the appearance at its size whatever the magnification.
    pub fn no_zoom(self) -> bool {
        self.bit(4)
    }

    /// Bit 5: keep the appearance upright whatever the page rotation.
    pub fn no_rotate(self) -> bool {
        self.bit(5)
    }

    /// Bit 6: do not show the annotation on screen or let it interact.
    pub fn no_view(self) -> bool {
        self.bit(6)
    }

    /// Bit 7: do not let the user interact with the annotation.
    pub fn read_only(self) -> bool {
        self.bit(7)
    }

    /// Bit 8: do not let the user delete or move the annotation.
    pub fn locked(self) -> bool {
        self.bit(8)
    }

    /// Bit 9: invert the NoView flag for certain events.
    pub fn toggle_no_view(self) -> bool {
        self.bit(9)
    }

    /// Bit 10: do not let the user change the annotation's contents.
    pub fn locked_contents(self) -> bool {
        self.bit(10)
    }
}

/// An annotation's `/Border` array (ISO 32000-1 §12.5.2, Table 164): the
/// corner radii and width of the border, with the dash array PDF 1.1
/// allows as a fourth element.
#[derive(Debug, Clone, PartialEq)]
pub struct Border {
    pub horizontal_radius: f32,
    pub vertical_radius: f32,
    pub width: f32,
    /// The dash array in the form of the graphics state's line dash
    /// pattern (§8.4.3.6); `None` for a solid border.
    pub dash: Option<Vec<f32>>,
}

/// How a reply relates to the annotation it replies to (`/RT`, ISO
/// 32000-1 §12.5.6.2, Table 170).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyType {
    /// `R`: a reply, shown threaded under the original (the default).
    #[default]
    Reply,
    /// `Group`: a subordinate annotation of a group the original leads.
    Group,
}

impl ReplyType {
    fn from_name(name: &str) -> Option<ReplyType> {
        match name {
            "R" => Some(ReplyType::Reply),
            "Group" => Some(ReplyType::Group),
            _ => None,
        }
    }
}

/// The entries every markup annotation may carry (ISO 32000-1 §12.5.6.2,
/// Table 170).
#[derive(Debug, Clone, PartialEq)]
pub struct Markup {
    /// `/T`: the label of the pop-up window's title bar, by convention the
    /// author.
    pub title: Option<String>,
    /// `/Popup`: the pop-up annotation that shows the text, by reference.
    pub popup: Option<ObjRef>,
    /// `/CA`: the constant opacity the annotation is painted with when it
    /// has no appearance stream; 1 by default.
    pub opacity: f32,
    /// `/RC`: the rich text shown in the pop-up window, a text string or a
    /// text stream, decoded.
    pub rich_contents: Option<String>,
    /// `/CreationDate`, parsed.
    pub created: Option<Date>,
    /// `/IRT`: the annotation this one replies to, by reference.
    pub in_reply_to: Option<ObjRef>,
    /// `/Subj`: a short description of the subject.
    pub subject: Option<String>,
    /// `/RT`: how this annotation relates to `in_reply_to`.
    pub reply_type: ReplyType,
    /// `/IT`: the intent that refines the subtype's behaviour, as written;
    /// `None` when absent.
    pub intent: Option<String>,
}

/// The state a text annotation sets on the annotation it replies to (ISO
/// 32000-1 §12.5.6.3, Tables 171 and 172).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationState {
    /// `/StateModel`: `Marked` or `Review`, as written; `None` when the
    /// annotation names a state without its model.
    pub model: Option<String>,
    /// `/State`, or the model's default when absent: `Unmarked` under
    /// `Marked`, `None` under `Review`.
    pub state: String,
}

/// The event that fires an action in an additional-actions dictionary
/// (ISO 32000-1 §12.6.3, Tables 194 to 197).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `E`: the cursor enters the annotation's active area.
    CursorEnter,
    /// `X`: the cursor leaves the annotation's active area.
    CursorExit,
    /// `D`: the mouse button goes down inside the active area.
    MouseDown,
    /// `U`: the mouse button comes up inside the active area.
    MouseUp,
    /// `Fo`: a widget receives the input focus.
    Focus,
    /// `Bl`: a widget loses the input focus.
    Blur,
    /// `PO`: the page holding the annotation opens.
    PageOpen,
    /// `PC`: the page holding the annotation closes.
    PageClose,
    /// `PV`: the page holding the annotation becomes visible.
    PageVisible,
    /// `PI`: the page holding the annotation stops being visible.
    PageInvisible,
    /// `K` on a field: a keystroke changes a text or choice field.
    Keystroke,
    /// `F` on a field: the field is about to be formatted for display.
    Format,
    /// `V` on a field: the field's value changed.
    Validate,
    /// `C` on a field: another field's value changed.
    Calculate,
    /// `O` on a page: the page opens.
    Open,
    /// `C` on a page: the page closes.
    Close,
    /// `WC` on the catalog: the document is about to close.
    WillClose,
    /// `WS` on the catalog: the document is about to be saved.
    WillSave,
    /// `DS` on the catalog: the document was saved.
    DidSave,
    /// `WP` on the catalog: the document is about to be printed.
    WillPrint,
    /// `DP` on the catalog: the document was printed.
    DidPrint,
}

/// The keys of an annotation's additional-actions dictionary (Table 194),
/// with the field triggers (Table 196) a merged widget and field
/// dictionary also carries.
const ANNOTATION_TRIGGERS: [(&str, Trigger); 14] = [
    ("E", Trigger::CursorEnter),
    ("X", Trigger::CursorExit),
    ("D", Trigger::MouseDown),
    ("U", Trigger::MouseUp),
    ("Fo", Trigger::Focus),
    ("Bl", Trigger::Blur),
    ("PO", Trigger::PageOpen),
    ("PC", Trigger::PageClose),
    ("PV", Trigger::PageVisible),
    ("PI", Trigger::PageInvisible),
    ("K", Trigger::Keystroke),
    ("F", Trigger::Format),
    ("V", Trigger::Validate),
    ("C", Trigger::Calculate),
];

/// The keys of a page's additional-actions dictionary (Table 195).
const PAGE_TRIGGERS: [(&str, Trigger); 2] = [("O", Trigger::Open), ("C", Trigger::Close)];

/// The keys of the catalog's additional-actions dictionary (Table 197).
const DOCUMENT_TRIGGERS: [(&str, Trigger); 5] = [
    ("WC", Trigger::WillClose),
    ("WS", Trigger::WillSave),
    ("DS", Trigger::DidSave),
    ("WP", Trigger::WillPrint),
    ("DP", Trigger::DidPrint),
];

/// One entry of an additional-actions dictionary: the event and the action
/// it fires.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggeredAction {
    pub trigger: Trigger,
    pub action: Action,
}

/// Whether a target document is the parent or a child of the current one
/// (`/R`, ISO 32000-1 §12.6.4.4, Table 202).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relationship {
    Parent,
    Child,
}

/// The page a target dictionary's `/P` names: a 0-based page number, or a
/// named destination that resolves to one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPage {
    Number(u32),
    Named(Vec<u8>),
}

/// The file attachment annotation a target dictionary's `/A` names: its
/// 0-based index in the page's `/Annots`, or its `/NM` name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetAnnotation {
    Index(u32),
    Name(String),
}

/// One step of the path from the current document to the document an
/// embedded go-to action jumps into (ISO 32000-1 §12.6.4.4, Table 202).
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// `/R`: whether this step goes to the parent document or to a child.
    pub relationship: Relationship,
    /// `/N`: the child's name in the `/EmbeddedFiles` name tree.
    pub name: Option<Vec<u8>>,
    /// `/P`: the page whose file attachment annotation holds the child.
    pub page: Option<TargetPage>,
    /// `/A`: which annotation of that page holds the child.
    pub annotation: Option<TargetAnnotation>,
    /// `/T`: the next step; `None` when this step's document is the target.
    pub next: Option<Box<Target>>,
}

/// The Windows launch parameters of a launch action (ISO 32000-1
/// §12.6.4.5, Table 204).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsLaunch {
    /// `/F`: the application or document, a plain Windows path.
    pub file: Vec<u8>,
    /// `/D`: the default directory.
    pub directory: Option<Vec<u8>>,
    /// `/O`: `open` (the default) or `print`.
    pub operation: String,
    /// `/P`: the parameter string passed to the application.
    pub parameters: Option<Vec<u8>>,
}

/// What one action dictionary asks for (ISO 32000-1 §12.6.4, Table 198).
/// The go-to family, launch, URI, named and JavaScript actions are read
/// into their own entries; every other type keeps its dictionary.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionKind {
    /// `GoTo` (§12.6.4.2): a destination in this document. A named `/D` is
    /// looked up; `named_destination` keeps the name either way.
    GoTo {
        destination: Option<Destination>,
        named_destination: Option<Vec<u8>>,
    },
    /// `GoToR` (§12.6.4.3): a destination in another PDF file, whose
    /// explicit form numbers pages from 0 and whose named form cannot be
    /// looked up here.
    GoToR {
        file: Option<FileSpec>,
        destination: Option<Destination>,
        named_destination: Option<Vec<u8>>,
        new_window: Option<bool>,
    },
    /// `GoToE` (§12.6.4.4): a destination in an embedded PDF file reached
    /// through `target`.
    GoToE {
        file: Option<FileSpec>,
        destination: Option<Destination>,
        named_destination: Option<Vec<u8>>,
        new_window: Option<bool>,
        target: Option<Target>,
    },
    /// `Launch` (§12.6.4.5): an application to start or a document to
    /// open or print.
    Launch {
        file: Option<FileSpec>,
        windows: Option<WindowsLaunch>,
        new_window: Option<bool>,
    },
    /// `URI` (§12.6.4.7): a resource to resolve.
    Uri {
        /// `/URI`, decoded as a text string (7-bit ASCII as the clause
        /// requires, so the decoding is the identity).
        uri: String,
        /// `/IsMap`: whether the mouse position is appended to the URI.
        is_map: bool,
    },
    /// `Named` (§12.6.4.11): a viewer action by name (`/N`).
    Named { name: String },
    /// `JavaScript` (§12.6.4.16): the script (`/JS`), a text string or a
    /// text stream, decoded and never run.
    JavaScript { script: Option<String> },
    /// Any other `/S` value, its dictionary kept as written.
    Other { kind: String, entries: Dict },
}

/// One action dictionary (ISO 32000-1 §12.6.2, Table 193) and the actions
/// its `/Next` entry chains after it, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// The dictionary's reference; `None` for a direct dictionary.
    pub object: Option<ObjRef>,
    pub kind: ActionKind,
    /// `/Next`: the actions performed after this one, a single dictionary
    /// or an array, each with its own chain. A dictionary already read in
    /// this tree is not entered again.
    pub next: Vec<Action>,
}

/// One annotation of a page (ISO 32000-1 §12.5.2, Table 164), with the
/// entries of the subtypes pdfboss reads beyond the common ones: the
/// markup entries (§12.5.6.2), a reply's state (§12.5.6.3), a text or
/// pop-up annotation's open flag and icon, a link's destination
/// (§12.5.6.5), an attachment's file (§12.5.6.15), and the `/A` and `/AA`
/// actions (§12.6).
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    /// The dictionary's reference in the page's `/Annots`; `None` for a
    /// direct dictionary.
    pub object: Option<ObjRef>,
    /// `/Subtype`, as written.
    pub subtype: String,
    /// `/Rect`, normalized; `None` when the entry is missing or malformed.
    pub rect: Option<Rect>,
    /// `/Contents`: the annotation's text, or an alternate description.
    pub contents: Option<String>,
    /// `/P`: the page the annotation belongs to, by reference.
    pub page: Option<ObjRef>,
    /// `/NM`: the name unique among the page's annotations.
    pub name: Option<String>,
    /// `/M`: when the annotation last changed, as written.
    pub modified: Option<String>,
    /// `/M` parsed as a date (§7.9.4); `None` when it is in another format.
    pub modified_date: Option<Date>,
    /// `/F`, 0 when absent.
    pub flags: AnnotationFlags,
    /// Whether an `/AP` dictionary with a normal appearance is present.
    pub has_appearance: bool,
    /// `/AS`: the appearance state that picks the appearance stream.
    pub appearance_state: Option<String>,
    /// `/Border`, as written; `None` when absent (the clause's default is
    /// `[0 0 1]`) or malformed.
    pub border: Option<Border>,
    /// `/C`: the colour of the icon background, pop-up title bar or link
    /// border, 0 (transparent), 1, 3 or 4 components as written.
    pub color: Option<Vec<f32>>,
    /// `/StructParent`: the key under which the structure tree's parent
    /// tree lists the annotation (§14.7.4.4).
    pub struct_parent: Option<i64>,
    /// `/OC`: the optional content group or membership dictionary that
    /// controls the annotation's visibility, by reference.
    pub optional_content: Option<ObjRef>,
    /// The Table 170 entries; `Some` for the markup subtypes of Table 169.
    pub markup: Option<Markup>,
    /// The state a text annotation sets on the annotation it replies to;
    /// `Some` when `/StateModel` or `/State` is present.
    pub state: Option<AnnotationState>,
    /// `/Open` of a text or pop-up annotation.
    pub open: Option<bool>,
    /// `/Name`: the icon an annotation shows without an appearance stream,
    /// as written; text, file attachment and sound annotations name one.
    pub icon: Option<String>,
    /// `/Parent`: the markup annotation a pop-up belongs to, by reference.
    pub parent: Option<ObjRef>,
    /// `/FS`: the file a file attachment annotation carries.
    pub file: Option<FileSpec>,
    /// A link's `/Dest`, explicit or looked up by name.
    pub destination: Option<Destination>,
    /// `/A`: the action performed when the annotation is activated, with
    /// its `/Next` chain.
    pub action: Option<Action>,
    /// `/AA`: the actions the annotation's trigger events fire, in the
    /// order of Tables 194 and 196.
    pub additional_actions: Vec<TriggeredAction>,
}

/// The annotations of `page`'s `/Annots` array in order, each read as
/// [`Annotation`]. An item that is no dictionary or names no `/Subtype` is
/// skipped; a missing or malformed `/Annots` reads as empty. `trailer` is
/// the document trailer, needed to look up named destinations.
///
/// Covers ISO 32000-1 §12.5.2.
pub async fn annotations_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    page: &Page,
) -> Vec<Annotation> {
    let Some(entry) = page.dict().get("Annots") else {
        return Vec::new();
    };
    let Ok(Object::Array(items)) = src.resolve(entry).await else {
        return Vec::new();
    };
    let mut annotations = Vec::new();
    for item in &items {
        let Some(dict) = resolved_dict(src, item).await else {
            continue;
        };
        let Some(subtype) = dict.get_name("Subtype") else {
            continue;
        };
        let subtype = subtype.0.clone();
        annotations.push(read_annotation(src, trailer, item.as_ref(), &dict, subtype).await);
    }
    annotations
}

/// The Table 164 entries of one annotation dictionary plus the subtype
/// entries pdfboss reads.
///
/// Covers ISO 32000-1 §12.5.2, §12.5.6.4, §12.5.6.5, §12.5.6.14 and §12.5.6.15.
async fn read_annotation<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    object: Option<ObjRef>,
    dict: &Dict,
    subtype: String,
) -> Annotation {
    let entries = Entries { src, dict };
    let modified = entries.text("M").await;
    let modified_date = modified.as_deref().and_then(Date::parse_pdf);
    let flags = entries
        .value("F")
        .await
        .and_then(|f| f.as_int())
        .and_then(|f| u32::try_from(f).ok())
        .map_or(AnnotationFlags::default(), AnnotationFlags);
    let has_appearance = match dict.get("AP") {
        Some(ap) => resolved_dict(src, ap)
            .await
            .is_some_and(|ap| ap.get("N").is_some()),
        None => false,
    };
    let appearance_state = entries.named("AS", |n| Some(n.to_string())).await;
    let markup = if MARKUP_SUBTYPES.contains(&subtype.as_str()) {
        Some(read_markup(src, dict).await)
    } else {
        None
    };
    let file = match dict.get("FS") {
        Some(fs) if subtype == "FileAttachment" => file_spec_with(src, fs).await,
        _ => None,
    };
    let destination = match dict.get("Dest") {
        Some(dest) if subtype == "Link" => {
            let (explicit, _) = destination_entry(src, trailer, dest, false).await;
            explicit
        }
        _ => None,
    };
    let action = match dict.get("A") {
        Some(a) => action_with(src, trailer, a).await,
        None => None,
    };
    let additional_actions = triggered_actions(src, trailer, dict, &ANNOTATION_TRIGGERS).await;
    Annotation {
        object,
        subtype,
        rect: rectangle(src, dict.get("Rect")).await,
        contents: entries.text("Contents").await,
        page: dict.get("P").and_then(Object::as_ref),
        name: entries.text("NM").await,
        modified,
        modified_date,
        flags,
        has_appearance,
        appearance_state,
        border: read_border(src, dict.get("Border")).await,
        color: number_array(src, dict.get("C")).await,
        struct_parent: entries.value("StructParent").await.and_then(|v| v.as_int()),
        optional_content: dict.get("OC").and_then(Object::as_ref),
        markup,
        state: read_state(&entries).await,
        open: entries.flag("Open").await,
        icon: entries.named("Name", |n| Some(n.to_string())).await,
        parent: dict.get("Parent").and_then(Object::as_ref),
        file,
        destination,
        action,
        additional_actions,
    }
}

/// The Table 170 entries of a markup annotation.
///
/// Covers ISO 32000-1 §12.5.6.2.
async fn read_markup<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Markup {
    let entries = Entries { src, dict };
    let opacity = entries
        .value("CA")
        .await
        .and_then(|ca| ca.as_f64())
        .filter(|ca| ca.is_finite())
        .map_or(1.0, |ca| ca as f32);
    Markup {
        title: entries.text("T").await,
        popup: dict.get("Popup").and_then(Object::as_ref),
        opacity,
        rich_contents: text_or_stream(src, dict.get("RC")).await,
        created: match entries.text("CreationDate").await {
            Some(date) => Date::parse_pdf(&date),
            None => None,
        },
        in_reply_to: dict.get("IRT").and_then(Object::as_ref),
        subject: entries.text("Subj").await,
        reply_type: entries
            .named("RT", ReplyType::from_name)
            .await
            .unwrap_or_default(),
        intent: entries.named("IT", |n| Some(n.to_string())).await,
    }
}

/// The `/StateModel` and `/State` of a reply, the state defaulted per
/// model when only the model is given.
///
/// Covers ISO 32000-1 §12.5.6.3.
async fn read_state<S: AsyncObjectSource>(entries: &Entries<'_, S>) -> Option<AnnotationState> {
    let model = entries.text("StateModel").await;
    let state = entries.text("State").await;
    let state = match (state, model.as_deref()) {
        (Some(state), _) => state,
        (None, Some("Marked")) => "Unmarked".to_string(),
        (None, Some("Review")) => "None".to_string(),
        (None, _) => return None,
    };
    Some(AnnotationState { model, state })
}

/// A text string given directly or as a text stream (`/RC`, `/JS`),
/// decoded (§7.9.2.2 and §7.9.3).
async fn text_or_stream<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<String> {
    match src.resolve(value?).await.ok()? {
        Object::String(bytes) => Some(decode_text_string(&bytes)),
        Object::Stream(stream) => src
            .stream_data(&stream)
            .await
            .ok()
            .map(|bytes| decode_text_string(&bytes)),
        _ => None,
    }
}

/// An array of numbers, each possibly indirect; `None` when the value is
/// no array or holds anything else.
async fn number_array<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<Vec<f32>> {
    let array = src.resolve(value?).await.ok()?;
    let items = array.as_array()?;
    let mut numbers = Vec::with_capacity(items.len());
    for item in items {
        let number = src.resolve(item).await.ok()?.as_f64()?;
        if !number.is_finite() {
            return None;
        }
        numbers.push(number as f32);
    }
    Some(numbers)
}

/// The `/Border` array: three numbers and an optional dash array.
///
/// Covers ISO 32000-1 §12.5.2.
async fn read_border<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<Border> {
    let array = src.resolve(value?).await.ok()?;
    let items = array.as_array()?;
    if items.len() < 3 {
        return None;
    }
    let mut numbers = [0.0f32; 3];
    for (slot, item) in numbers.iter_mut().zip(items) {
        let number = src.resolve(item).await.ok()?.as_f64()?;
        if !number.is_finite() {
            return None;
        }
        *slot = number as f32;
    }
    let dash = match items.get(3) {
        Some(dash) => Some(number_array(src, Some(dash)).await?),
        None => None,
    };
    Some(Border {
        horizontal_radius: numbers[0],
        vertical_radius: numbers[1],
        width: numbers[2],
        dash,
    })
}

/// A `/D` or `/Dest` value: an explicit destination array, or a name or
/// string. The name is kept either way; it is looked up in this document
/// unless `remote`, when it belongs to another file.
///
/// Covers ISO 32000-1 §12.6.4.2 and §12.6.4.3.
async fn destination_entry<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    value: &Object,
    remote: bool,
) -> (Option<Destination>, Option<Vec<u8>>) {
    let Ok(resolved) = src.resolve(value).await else {
        return (None, None);
    };
    let name = match &resolved {
        Object::Array(_) => return (destination_with(src, &resolved).await, None),
        Object::Name(name) => name.0.as_bytes().to_vec(),
        Object::String(bytes) => bytes.clone(),
        _ => return (None, None),
    };
    if remote {
        return (None, Some(name));
    }
    let destination = named_destination_with(src, trailer, &name).await;
    (destination, Some(name))
}

/// The actions an additional-actions dictionary (`/AA` of `dict`) names,
/// in the order of `triggers`.
///
/// Covers ISO 32000-1 §12.6.3.
async fn triggered_actions<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    dict: &Dict,
    triggers: &[(&str, Trigger)],
) -> Vec<TriggeredAction> {
    let Some(aa) = dict.get("AA") else {
        return Vec::new();
    };
    let Some(aa) = resolved_dict(src, aa).await else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for (key, trigger) in triggers {
        let Some(entry) = aa.get(key) else {
            continue;
        };
        if let Some(action) = action_with(src, trailer, entry).await {
            actions.push(TriggeredAction {
                trigger: *trigger,
                action,
            });
        }
    }
    actions
}

/// The actions a page's `/AA` dictionary fires when the page opens and
/// closes (Table 195), in that order.
///
/// Covers ISO 32000-1 §12.6.3.
pub async fn page_additional_actions_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    page: &Page,
) -> Vec<TriggeredAction> {
    triggered_actions(src, trailer, page.dict(), &PAGE_TRIGGERS).await
}

/// The actions the catalog's `/AA` dictionary fires around closing,
/// saving and printing the document (Table 197), in that order.
///
/// Covers ISO 32000-1 §12.6.3.
pub async fn document_additional_actions_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Vec<TriggeredAction> {
    let Some(root) = trailer.get("Root") else {
        return Vec::new();
    };
    let Some(catalog) = resolved_dict(src, root).await else {
        return Vec::new();
    };
    triggered_actions(src, trailer, &catalog, &DOCUMENT_TRIGGERS).await
}

/// One action dictionary being read, before its `/Next` children are
/// moved under it.
struct ActionNode {
    parent: Option<usize>,
    action: Action,
}

/// The action `value` holds, with its whole `/Next` tree. `None` when the
/// value is no dictionary or names no `/S`. The tree is read breadth by
/// depth-first order with a visited set over references and a budget of
/// [`MAX_ACTIONS`] dictionaries, so a `/Next` that points back at an
/// earlier action is not entered twice and a runaway tree stops.
///
/// Covers ISO 32000-1 §12.6.2 and §12.6.4.
pub async fn action_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    value: &Object,
) -> Option<Action> {
    let mut visited: FastSet<ObjRef> = FastSet::default();
    let mut nodes: Vec<ActionNode> = Vec::new();
    let mut pending: Vec<(Option<usize>, Object)> = vec![(None, value.clone())];
    while let Some((parent, object)) = pending.pop() {
        if nodes.len() >= MAX_ACTIONS {
            break;
        }
        let reference = object.as_ref();
        if let Some(r) = reference {
            if !visited.insert(r) {
                continue;
            }
        }
        let Some(dict) = resolved_dict(src, &object).await else {
            continue;
        };
        let Some(kind) = read_action_kind(src, trailer, &dict).await else {
            continue;
        };
        let index = nodes.len();
        nodes.push(ActionNode {
            parent,
            action: Action {
                object: reference,
                kind,
                next: Vec::new(),
            },
        });
        // Children are pushed last first so they pop in document order.
        let children = match dict.get("Next") {
            Some(next) => match src.resolve(next).await {
                Ok(Object::Array(items)) => items,
                Ok(Object::Dict(_)) => vec![next.clone()],
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        for child in children.into_iter().rev() {
            pending.push((Some(index), child));
        }
    }
    // Children were created after their parents, so folding from the back
    // moves every finished subtree into its parent; a node's children
    // arrive in reverse order and are turned round before it moves.
    while let Some(node) = nodes.pop() {
        let mut action = node.action;
        action.next.reverse();
        match node.parent {
            Some(parent) => nodes[parent].action.next.push(action),
            None => return Some(action),
        }
    }
    None
}

/// The entries of one action dictionary by its `/S`.
///
/// Covers ISO 32000-1 §12.6.4, §12.6.4.2, §12.6.4.3, §12.6.4.4, §12.6.4.5, §12.6.4.7, §12.6.4.11 and §12.6.4.16.
async fn read_action_kind<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    dict: &Dict,
) -> Option<ActionKind> {
    let entries = Entries { src, dict };
    let kind = entries.value("S").await?.as_name()?.0.clone();
    let file = |key: &'static str| async move {
        match dict.get(key) {
            Some(spec) => file_spec_with(src, spec).await,
            None => None,
        }
    };
    let destination = |remote: bool| async move {
        match dict.get("D") {
            Some(d) => destination_entry(src, trailer, d, remote).await,
            None => (None, None),
        }
    };
    Some(match kind.as_str() {
        "GoTo" => {
            let (destination, named_destination) = destination(false).await;
            ActionKind::GoTo {
                destination,
                named_destination,
            }
        }
        "GoToR" => {
            let (destination, named_destination) = destination(true).await;
            ActionKind::GoToR {
                file: file("F").await,
                destination,
                named_destination,
                new_window: entries.flag("NewWindow").await,
            }
        }
        "GoToE" => {
            let (destination, named_destination) = destination(true).await;
            ActionKind::GoToE {
                file: file("F").await,
                destination,
                named_destination,
                new_window: entries.flag("NewWindow").await,
                target: read_target(src, dict.get("T")).await,
            }
        }
        "Launch" => ActionKind::Launch {
            file: file("F").await,
            windows: read_windows_launch(src, dict.get("Win")).await,
            new_window: entries.flag("NewWindow").await,
        },
        "URI" => ActionKind::Uri {
            uri: entries.text("URI").await.unwrap_or_default(),
            is_map: entries.flag("IsMap").await.unwrap_or(false),
        },
        "Named" => ActionKind::Named {
            name: entries
                .named("N", |n| Some(n.to_string()))
                .await
                .unwrap_or_default(),
        },
        "JavaScript" => ActionKind::JavaScript {
            script: text_or_stream(src, dict.get("JS")).await,
        },
        _ => ActionKind::Other {
            kind,
            entries: dict.clone(),
        },
    })
}

/// The chain of target dictionaries from `value`, at most
/// [`MAX_TARGET_DEPTH`] deep.
///
/// Covers ISO 32000-1 §12.6.4.4.
async fn read_target<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<Target> {
    let mut steps: Vec<Target> = Vec::new();
    let mut current = value.cloned();
    while let Some(object) = current.take() {
        if steps.len() >= MAX_TARGET_DEPTH {
            break;
        }
        let Some(dict) = resolved_dict(src, &object).await else {
            break;
        };
        let entries = Entries { src, dict: &dict };
        let relationship = match entries.value("R").await?.as_name()?.0.as_str() {
            "P" => Relationship::Parent,
            "C" => Relationship::Child,
            _ => break,
        };
        let page = match entries.value("P").await {
            Some(Object::Int(n)) => u32::try_from(n).ok().map(TargetPage::Number),
            Some(Object::String(bytes)) => Some(TargetPage::Named(bytes)),
            _ => None,
        };
        let annotation = match entries.value("A").await {
            Some(Object::Int(n)) => u32::try_from(n).ok().map(TargetAnnotation::Index),
            Some(Object::String(bytes)) => Some(TargetAnnotation::Name(decode_text_string(&bytes))),
            _ => None,
        };
        steps.push(Target {
            relationship,
            name: entries
                .value("N")
                .await
                .and_then(|n| n.as_str_bytes().map(<[u8]>::to_vec)),
            page,
            annotation,
            next: None,
        });
        current = dict.get("T").cloned();
    }
    steps.into_iter().rev().fold(None, |next, mut step| {
        step.next = next.map(Box::new);
        Some(step)
    })
}

/// The Table 204 entries of a launch action's `/Win` dictionary.
///
/// Covers ISO 32000-1 §12.6.4.5.
async fn read_windows_launch<S: AsyncObjectSource>(
    src: &S,
    value: Option<&Object>,
) -> Option<WindowsLaunch> {
    let dict = resolved_dict(src, value?).await?;
    Some(WindowsLaunch {
        file: bytes_entry(src, &dict, "F").await?,
        directory: bytes_entry(src, &dict, "D").await,
        operation: match bytes_entry(src, &dict, "O").await {
            Some(operation) => String::from_utf8_lossy(&operation).into_owned(),
            None => "open".to_string(),
        },
        parameters: bytes_entry(src, &dict, "P").await,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::{DestinationPage, Fit};
    use crate::document::Document;
    use pdfboss_testkit::PdfBuilder;

    /// A one-page document whose page carries `page_extra`, with a PDF 1.1
    /// `/Dests` dictionary naming `Here`; `objects` supplies 10 and up and
    /// `streams` any streams.
    fn doc(page_extra: &str, objects: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Dests << /Here [3 0 R /FitH 500] >> \
             /AA << /WC << /S /JavaScript /JS (bye) >> /DP 30 0 R >> >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] {page_extra} >>"),
        );
        b.object(30, "<< /S /Named /N /NextPage >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        for (num, dict, data) in streams {
            b.stream(*num, dict, data);
        }
        Document::load(b.build(1)).expect("load")
    }

    fn annotations(page_extra: &str, objects: &[(u32, &str)]) -> Vec<Annotation> {
        let doc = doc(page_extra, objects, &[]);
        doc.annotations(&doc.page(0).unwrap())
    }

    fn one(page_extra: &str, objects: &[(u32, &str)]) -> Annotation {
        let mut all = annotations(page_extra, objects);
        assert_eq!(all.len(), 1, "{all:?}");
        all.remove(0)
    }

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    /// Every Table 164 entry reads: the subtype, the rectangle normalized,
    /// the text and name entries decoded, the date both as written and
    /// parsed, the flag word, the appearance dictionary and state, the
    /// border with its dash, the colour, the structure parent key and the
    /// optional content reference.
    // Covers ISO 32000-1 §12.5.2 and §12.5.3.
    #[test]
    fn reads_the_entries_common_to_every_annotation() {
        let annotation = one(
            "/Annots [10 0 R]",
            &[(
                10,
                "<< /Type /Annot /Subtype /Square /Rect [150 90 50 10] /Contents (Draft) \
                 /P 3 0 R /NM (sq-1) /M (D:20240102030405Z) /F 68 \
                 /AP << /N 11 0 R >> /AS /On /Border [2 3 1 [3 2]] /C [1 0 0] \
                 /StructParent 7 /OC 12 0 R >>",
            )],
        );
        assert_eq!(annotation.object, Some(r(10)));
        assert_eq!(annotation.subtype, "Square");
        assert_eq!(annotation.rect, Some(Rect::new(50.0, 10.0, 150.0, 90.0)));
        assert_eq!(annotation.contents.as_deref(), Some("Draft"));
        assert_eq!(annotation.page, Some(r(3)));
        assert_eq!(annotation.name.as_deref(), Some("sq-1"));
        assert_eq!(annotation.modified.as_deref(), Some("D:20240102030405Z"));
        assert_eq!(
            annotation.modified_date.map(|d| d.to_iso8601()).as_deref(),
            Some("2024-01-02T03:04:05Z")
        );
        assert_eq!(annotation.flags, AnnotationFlags(68));
        assert!(annotation.flags.print());
        assert!(annotation.flags.read_only());
        assert!(!annotation.flags.hidden());
        assert!(annotation.has_appearance);
        assert_eq!(annotation.appearance_state.as_deref(), Some("On"));
        assert_eq!(
            annotation.border,
            Some(Border {
                horizontal_radius: 2.0,
                vertical_radius: 3.0,
                width: 1.0,
                dash: Some(vec![3.0, 2.0]),
            })
        );
        assert_eq!(annotation.color, Some(vec![1.0, 0.0, 0.0]));
        assert_eq!(annotation.struct_parent, Some(7));
        assert_eq!(annotation.optional_content, Some(r(12)));
        assert!(
            annotation.markup.is_some(),
            "a square is a markup annotation"
        );
        assert_eq!(annotation.state, None);
        assert_eq!(annotation.action, None);
    }

    /// A direct dictionary in `/Annots` reads without a reference, an item
    /// that is no dictionary or has no `/Subtype` is skipped, a page
    /// without `/Annots` has none, and the optional entries read as absent
    /// with the flag word 0 and no appearance.
    // Covers ISO 32000-1 §12.5.2.
    #[test]
    fn direct_malformed_and_missing_annotations() {
        let all = annotations(
            "/Annots [<< /Subtype /Link /Rect [0 0 10 10] >> 5 << /Rect [0 0 1 1] >> 10 0 R]",
            &[(10, "<< /Subtype /Square /Rect [0 0 1] >>")],
        );
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(all[0].object, None);
        assert_eq!(all[0].subtype, "Link");
        assert_eq!(all[0].flags, AnnotationFlags(0));
        assert!(!all[0].has_appearance);
        assert_eq!(all[0].markup, None, "a link is not a markup annotation");
        assert_eq!(all[0].border, None);
        assert_eq!(all[1].rect, None, "a three-number rectangle is malformed");
        assert!(annotations("", &[]).is_empty());
        assert!(annotations("/Annots 4", &[]).is_empty());
    }

    /// The Table 170 entries of a markup annotation: the title, pop-up
    /// reference, opacity (1 by default), rich contents from a string or a
    /// stream, creation date, reply reference, subject, reply type (a reply
    /// by default) and intent.
    // Covers ISO 32000-1 §12.5.6.2.
    #[test]
    fn reads_the_markup_entries() {
        let doc = doc(
            "/Annots [10 0 R 11 0 R]",
            &[
                (
                    10,
                    "<< /Subtype /Square /Rect [0 0 10 10] /T (Ada) /Popup 12 0 R /CA 0.5 \
                     /RC (<p>rich</p>) /CreationDate (D:20240102030405Z) /IRT 11 0 R \
                     /Subj (Shape) /RT /Group /IT /SquareCloud >>",
                ),
                (11, "<< /Subtype /Circle /Rect [0 0 10 10] /RC 13 0 R >>"),
            ],
            &[(13, "", b"from a stream")],
        );
        let all = doc.annotations(&doc.page(0).unwrap());
        let square = all[0].markup.as_ref().unwrap();
        assert_eq!(square.title.as_deref(), Some("Ada"));
        assert_eq!(square.popup, Some(r(12)));
        assert_eq!(square.opacity, 0.5);
        assert_eq!(square.rich_contents.as_deref(), Some("<p>rich</p>"));
        assert_eq!(
            square.created.map(|d| d.to_iso8601()).as_deref(),
            Some("2024-01-02T03:04:05Z")
        );
        assert_eq!(square.in_reply_to, Some(r(11)));
        assert_eq!(square.subject.as_deref(), Some("Shape"));
        assert_eq!(square.reply_type, ReplyType::Group);
        assert_eq!(square.intent.as_deref(), Some("SquareCloud"));
        let circle = all[1].markup.as_ref().unwrap();
        assert_eq!(circle.opacity, 1.0);
        assert_eq!(circle.rich_contents.as_deref(), Some("from a stream"));
        assert_eq!(circle.reply_type, ReplyType::Reply);
        assert_eq!(circle.title, None);
    }

    /// A text annotation replying with a state model and state reads both;
    /// a model without a state takes the model's default; a state without
    /// a model keeps the state alone.
    // Covers ISO 32000-1 §12.5.6.3 and §12.5.6.4.
    #[test]
    fn reads_the_state_a_reply_sets() {
        let all = annotations(
            "/Annots [10 0 R 11 0 R 12 0 R 13 0 R]",
            &[
                (
                    10,
                    "<< /Subtype /Text /Rect [0 0 10 10] /IRT 14 0 R /T (Ada) \
                     /StateModel (Review) /State (Accepted) /Open true /Name /Comment >>",
                ),
                (
                    11,
                    "<< /Subtype /Text /Rect [0 0 10 10] /StateModel (Marked) >>",
                ),
                (
                    12,
                    "<< /Subtype /Text /Rect [0 0 10 10] /StateModel (Review) >>",
                ),
                (
                    13,
                    "<< /Subtype /Text /Rect [0 0 10 10] /State (Completed) >>",
                ),
            ],
        );
        assert_eq!(
            all[0].state,
            Some(AnnotationState {
                model: Some("Review".to_string()),
                state: "Accepted".to_string(),
            })
        );
        assert_eq!(all[0].open, Some(true));
        assert_eq!(all[0].icon.as_deref(), Some("Comment"));
        assert_eq!(all[0].markup.as_ref().unwrap().in_reply_to, Some(r(14)));
        assert_eq!(all[1].state.as_ref().unwrap().state, "Unmarked");
        assert_eq!(all[2].state.as_ref().unwrap().state, "None");
        assert_eq!(
            all[3].state,
            Some(AnnotationState {
                model: None,
                state: "Completed".to_string(),
            })
        );
        assert_eq!(all[3].open, None);
    }

    /// A link's `/Dest` reads explicit or by name through the catalog's
    /// destinations, and its `/A` reads as an action; a pop-up's parent
    /// and open flag read too.
    // Covers ISO 32000-1 §12.5.6.5, §12.5.6.14 and §12.6.4.2.
    #[test]
    fn reads_link_destinations_and_popups() {
        let all = annotations(
            "/Annots [10 0 R 11 0 R 12 0 R 13 0 R]",
            &[
                (
                    10,
                    "<< /Subtype /Link /Rect [0 0 10 10] /Dest [3 0 R /Fit] >>",
                ),
                (11, "<< /Subtype /Link /Rect [0 0 10 10] /Dest /Here >>"),
                (
                    12,
                    "<< /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D (Here) >> >>",
                ),
                (
                    13,
                    "<< /Subtype /Popup /Rect [0 0 10 10] /Parent 10 0 R /Open false >>",
                ),
            ],
        );
        let page = DestinationPage::Object(r(3));
        assert_eq!(
            all[0].destination,
            Some(Destination {
                page,
                fit: Fit::Fit
            })
        );
        assert_eq!(
            all[1].destination,
            Some(Destination {
                page,
                fit: Fit::FitH { top: Some(500.0) }
            })
        );
        assert_eq!(all[2].destination, None);
        assert_eq!(
            all[2].action.as_ref().unwrap().kind,
            ActionKind::GoTo {
                destination: Some(Destination {
                    page,
                    fit: Fit::FitH { top: Some(500.0) }
                }),
                named_destination: Some(b"Here".to_vec()),
            }
        );
        assert_eq!(all[3].parent, Some(r(10)));
        assert_eq!(all[3].open, Some(false));
        assert_eq!(all[3].markup, None, "a pop-up is not a markup annotation");
    }

    /// A file attachment annotation carries its file specification, whose
    /// embedded stream the document can decode, and its icon name.
    // Covers ISO 32000-1 §12.5.6.15 and §7.11.4.
    #[test]
    fn reads_a_file_attachment_and_its_data() {
        let doc = doc(
            "/Annots [10 0 R]",
            &[
                (
                    10,
                    "<< /Subtype /FileAttachment /Rect [0 0 10 10] /FS 11 0 R /Name /Paperclip \
                     /Contents (The data) >>",
                ),
                (
                    11,
                    "<< /Type /Filespec /F (data.csv) /EF << /F 12 0 R >> >>",
                ),
            ],
            &[(12, "/Type /EmbeddedFile", b"a,b\n1,2\n")],
        );
        let all = doc.annotations(&doc.page(0).unwrap());
        let file = all[0].file.as_ref().unwrap();
        assert_eq!(file.name(), "data.csv");
        assert_eq!(all[0].icon.as_deref(), Some("Paperclip"));
        assert_eq!(doc.file_spec_data(file).unwrap(), b"a,b\n1,2\n");
        let doc = self::doc(
            "/Annots [10 0 R]",
            &[(
                10,
                "<< /Subtype /Square /Rect [0 0 10 10] /FS (ignored.txt) >>",
            )],
            &[],
        );
        let all = doc.annotations(&doc.page(0).unwrap());
        assert_eq!(all[0].file, None, "only a file attachment carries a file");
    }

    /// `/Next` chains read in order whether a dictionary or an array, each
    /// with its own chain; an action that chains back to an earlier one is
    /// read once.
    // Covers ISO 32000-1 §12.6.2.
    #[test]
    fn follows_next_chains_once() {
        let annotation = one(
            "/Annots [10 0 R]",
            &[
                (
                    10,
                    "<< /Subtype /Link /Rect [0 0 10 10] /A 11 0 R >>",
                ),
                (
                    11,
                    "<< /S /URI /URI (http://a.example) /Next [12 0 R << /S /Named /N /LastPage >>] >>",
                ),
                (
                    12,
                    "<< /S /Named /N /NextPage /Next 13 0 R >>",
                ),
                (13, "<< /S /Named /N /PrevPage /Next 11 0 R >>"),
            ],
        );
        let action = annotation.action.unwrap();
        assert_eq!(action.object, Some(r(11)));
        assert_eq!(
            action.kind,
            ActionKind::Uri {
                uri: "http://a.example".to_string(),
                is_map: false
            }
        );
        assert_eq!(action.next.len(), 2, "{:?}", action.next);
        assert_eq!(action.next[0].object, Some(r(12)));
        assert_eq!(
            action.next[0].kind,
            ActionKind::Named {
                name: "NextPage".to_string()
            }
        );
        assert_eq!(action.next[0].next.len(), 1);
        assert_eq!(action.next[0].next[0].object, Some(r(13)));
        assert!(
            action.next[0].next[0].next.is_empty(),
            "the chain back to 11 is not entered again"
        );
        assert_eq!(action.next[1].object, None);
        assert_eq!(
            action.next[1].kind,
            ActionKind::Named {
                name: "LastPage".to_string()
            }
        );
    }

    /// A chain longer than the budget stops at it.
    // Covers ISO 32000-1 §12.6.2.
    #[test]
    fn a_runaway_next_chain_stops_at_the_budget() {
        let first = 100u32;
        let last = first + MAX_ACTIONS as u32 + 20;
        let bodies: Vec<(u32, String)> = (first..=last)
            .map(|num| {
                let next = if num < last {
                    format!(" /Next {} 0 R", num + 1)
                } else {
                    String::new()
                };
                (num, format!("<< /S /Named /N /NextPage{next} >>"))
            })
            .collect();
        let objects: Vec<(u32, &str)> = bodies
            .iter()
            .map(|(num, body)| (*num, body.as_str()))
            .collect();
        let annotation = one(
            "/Annots [<< /Subtype /Link /Rect [0 0 10 10] /A 100 0 R >>]",
            &objects,
        );
        let mut depth = 1;
        let mut action = annotation.action.as_ref().unwrap();
        while let Some(next) = action.next.first() {
            depth += 1;
            action = next;
        }
        assert_eq!(depth, MAX_ACTIONS);
    }

    /// The additional-actions dictionaries of an annotation (with the
    /// field triggers a merged widget carries), a page and the catalog
    /// read in table order, each trigger with its action.
    // Covers ISO 32000-1 §12.6.3.
    #[test]
    fn reads_trigger_events_on_annotations_pages_and_the_catalog() {
        let doc = doc(
            "/Annots [10 0 R] /AA << /C << /S /Named /N /FirstPage >> /O 30 0 R >>",
            &[(
                10,
                "<< /Subtype /Widget /Rect [0 0 10 10] /FT /Tx /T (f) \
                 /AA << /C << /S /JavaScript /JS (calc) >> /E 30 0 R /Fo << /S /Named /N /LastPage >> \
                 /Zz << /S /Named /N /Ignored >> >> >>",
            )],
            &[],
        );
        let page = doc.page(0).unwrap();
        let all = doc.annotations(&page);
        let triggers: Vec<Trigger> = all[0]
            .additional_actions
            .iter()
            .map(|t| t.trigger)
            .collect();
        assert_eq!(
            triggers,
            [Trigger::CursorEnter, Trigger::Focus, Trigger::Calculate]
        );
        assert_eq!(
            all[0].additional_actions[2].action.kind,
            ActionKind::JavaScript {
                script: Some("calc".to_string())
            }
        );
        let page_actions = doc.page_additional_actions(&page);
        let triggers: Vec<Trigger> = page_actions.iter().map(|t| t.trigger).collect();
        assert_eq!(triggers, [Trigger::Open, Trigger::Close]);
        assert_eq!(page_actions[0].action.object, Some(r(30)));
        let document_actions = doc.additional_actions();
        let triggers: Vec<Trigger> = document_actions.iter().map(|t| t.trigger).collect();
        assert_eq!(triggers, [Trigger::WillClose, Trigger::DidPrint]);
        assert_eq!(
            document_actions[0].action.kind,
            ActionKind::JavaScript {
                script: Some("bye".to_string())
            }
        );
        let bare = self::doc("", &[], &[]);
        assert!(bare
            .page_additional_actions(&bare.page(0).unwrap())
            .is_empty());
    }

    /// The action object `num` of a document whose objects 20 and up are
    /// `objects` and `streams`.
    fn action_of(
        objects: &[(u32, &str)],
        streams: &[(u32, &str, &[u8])],
        num: u32,
    ) -> Option<Action> {
        let doc = doc("", objects, streams);
        doc.action(&Object::Ref(r(num)))
    }

    /// Every Table 198 action type reads: the typed ones into their
    /// entries, the rest with their dictionary; a dictionary without `/S`
    /// or a value that is no dictionary is no action.
    // Covers ISO 32000-1 §12.6.4, §12.6.4.7, §12.6.4.11 and §12.6.4.16.
    #[test]
    fn reads_every_action_type() {
        let others = [
            "Thread",
            "Sound",
            "Movie",
            "Hide",
            "ResetForm",
            "ImportData",
            "SetOCGState",
            "Rendition",
            "Trans",
            "GoTo3DView",
        ];
        let other_bodies: Vec<String> = others.iter().map(|k| format!("<< /S /{k} >>")).collect();
        let mut objects = vec![
            (
                20,
                "<< /S /SubmitForm /F (http://x.example/post) /Flags 4 >>",
            ),
            (22, "<< /S /URI /URI (http://x.example/?q) /IsMap true >>"),
            (23, "<< /S /Named /N /FirstPage >>"),
            (24, "<< /S /JavaScript /JS 21 0 R >>"),
            (25, "<< /Type /Action >>"),
            (26, "(not a dictionary)"),
        ];
        objects.extend(
            other_bodies
                .iter()
                .enumerate()
                .map(|(i, body)| (30 + i as u32, body.as_str())),
        );
        let doc = doc("", &objects, &[(21, "", b"app.alert(1)")]);
        let action = |num: u32| doc.action(&Object::Ref(r(num)));
        assert_eq!(
            action(22).unwrap().kind,
            ActionKind::Uri {
                uri: "http://x.example/?q".to_string(),
                is_map: true
            }
        );
        assert_eq!(
            action(23).unwrap().kind,
            ActionKind::Named {
                name: "FirstPage".to_string()
            }
        );
        assert_eq!(
            action(24).unwrap().kind,
            ActionKind::JavaScript {
                script: Some("app.alert(1)".to_string())
            }
        );
        let other = action(20).unwrap();
        assert_eq!(other.object, Some(r(20)));
        match other.kind {
            ActionKind::Other { kind, entries } => {
                assert_eq!(kind, "SubmitForm");
                assert_eq!(entries.get("Flags").and_then(Object::as_int), Some(4));
            }
            other => panic!("{other:?}"),
        }
        for (i, kind) in others.iter().enumerate() {
            let read = action(30 + i as u32).unwrap();
            assert!(
                matches!(&read.kind, ActionKind::Other { kind: k, .. } if k == kind),
                "{kind}: {read:?}"
            );
        }
        assert_eq!(action(25), None);
        assert_eq!(action(26), None);
    }

    /// A remote go-to action carries its file, a page-numbered explicit
    /// destination or a name that stays a name, and the window flag.
    // Covers ISO 32000-1 §12.6.4.3.
    #[test]
    fn reads_remote_go_to_actions() {
        let objects = [
            (
                20,
                "<< /S /GoToR /F (other.pdf) /D [2 /XYZ 10 20 1.5] /NewWindow true >>",
            ),
            (
                21,
                "<< /S /GoToR /F << /F (o.pdf) /UF (o.pdf) >> /D (Here) >>",
            ),
        ];
        assert_eq!(
            action_of(&objects, &[], 20).unwrap().kind,
            ActionKind::GoToR {
                file: Some(FileSpec {
                    file: Some(b"other.pdf".to_vec()),
                    ..FileSpec::default()
                }),
                destination: Some(Destination {
                    page: DestinationPage::Number(2),
                    fit: Fit::Xyz {
                        left: Some(10.0),
                        top: Some(20.0),
                        zoom: Some(1.5)
                    }
                }),
                named_destination: None,
                new_window: Some(true),
            }
        );
        assert_eq!(
            action_of(&objects, &[], 21).unwrap().kind,
            ActionKind::GoToR {
                file: Some(FileSpec {
                    file: Some(b"o.pdf".to_vec()),
                    unicode_file: Some("o.pdf".to_string()),
                    ..FileSpec::default()
                }),
                destination: None,
                named_destination: Some(b"Here".to_vec()),
                new_window: None,
            }
        );
    }

    /// An embedded go-to action's target chain reads each step's
    /// relationship, name, page and annotation, nested through `/T`, stops
    /// at the depth cap, and ends at a step without a valid relationship.
    // Covers ISO 32000-1 §12.6.4.4.
    #[test]
    fn reads_embedded_go_to_targets() {
        let mut deep = String::from("<< /R /C >>");
        for _ in 0..(MAX_TARGET_DEPTH + 5) {
            deep = format!("<< /R /C /T {deep} >>");
        }
        let deep = format!("<< /S /GoToE /D (x) /T {deep} >>");
        let objects = [
            (
                20,
                "<< /S /GoToE /D [0 /Fit] /T << /R /P /T << /R /C /N (child.pdf) \
                 /T << /R /C /P 3 /A (annotName) >> >> >> >>",
            ),
            (21, "<< /S /GoToE /D (x) /T << /R /C /P (Here) /A 2 >> >>"),
            (22, deep.as_str()),
            (23, "<< /S /GoToE /D (x) /T << /R /Sideways >> >>"),
        ];
        let target_of = |num: u32| match action_of(&objects, &[], num).unwrap().kind {
            ActionKind::GoToE {
                target,
                destination,
                file,
                ..
            } => (target, destination, file),
            other => panic!("{other:?}"),
        };
        let (target, destination, file) = target_of(20);
        assert_eq!(file, None);
        assert_eq!(
            destination.map(|d| d.page),
            Some(DestinationPage::Number(0))
        );
        let first = target.unwrap();
        assert_eq!(first.relationship, Relationship::Parent);
        let second = first.next.as_deref().unwrap();
        assert_eq!(second.relationship, Relationship::Child);
        assert_eq!(second.name.as_deref(), Some(b"child.pdf".as_slice()));
        let third = second.next.as_deref().unwrap();
        assert_eq!(third.page, Some(TargetPage::Number(3)));
        assert_eq!(
            third.annotation,
            Some(TargetAnnotation::Name("annotName".to_string()))
        );
        assert_eq!(third.next, None);
        let (target, _, _) = target_of(21);
        let target = target.unwrap();
        assert_eq!(target.page, Some(TargetPage::Named(b"Here".to_vec())));
        assert_eq!(target.annotation, Some(TargetAnnotation::Index(2)));
        let (target, _, _) = target_of(22);
        let mut depth = 1;
        let mut step = target.unwrap();
        while let Some(next) = step.next {
            depth += 1;
            step = *next;
        }
        assert_eq!(depth, MAX_TARGET_DEPTH);
        let (target, _, _) = target_of(23);
        assert_eq!(target, None);
    }

    /// A launch action carries its file specification, its Windows
    /// parameters with `open` as the default operation, and the window
    /// flag.
    // Covers ISO 32000-1 §12.6.4.5.
    #[test]
    fn reads_launch_actions() {
        let objects = [
            (
                20,
                "<< /S /Launch /F (setup.exe) /NewWindow false \
                 /Win << /F (c:\\\\tools\\\\setup.exe) /D (c:\\\\tools) /O (print) /P (-q) >> >>",
            ),
            (21, "<< /S /Launch /Win << /F (notepad.exe) >> >>"),
        ];
        assert_eq!(
            action_of(&objects, &[], 20).unwrap().kind,
            ActionKind::Launch {
                file: Some(FileSpec {
                    file: Some(b"setup.exe".to_vec()),
                    ..FileSpec::default()
                }),
                windows: Some(WindowsLaunch {
                    file: b"c:\\tools\\setup.exe".to_vec(),
                    directory: Some(b"c:\\tools".to_vec()),
                    operation: "print".to_string(),
                    parameters: Some(b"-q".to_vec()),
                }),
                new_window: Some(false),
            }
        );
        assert_eq!(
            action_of(&objects, &[], 21).unwrap().kind,
            ActionKind::Launch {
                file: None,
                windows: Some(WindowsLaunch {
                    file: b"notepad.exe".to_vec(),
                    directory: None,
                    operation: "open".to_string(),
                    parameters: None,
                }),
                new_window: None,
            }
        );
    }
}
