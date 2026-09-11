//! The document's interactive form (ISO 32000-1 §12.7): the catalog's
//! `/AcroForm` dictionary, which names the root fields and carries the
//! defaults their widgets are drawn with.

use crate::hash::FastSet;
use crate::object::{decode_text_string, Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// Fields nested deeper than this below the root fields are not read.
const MAX_FIELD_DEPTH: usize = 64;

/// The document-level signature flags (`/SigFlags`, ISO 32000-1 §12.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignatureFlags {
    /// Bit 1, `SignaturesExist`: the document has at least one signature
    /// field.
    pub signatures_exist: bool,
    /// Bit 2, `AppendOnly`: the document's signatures would be invalidated
    /// by anything but an incremental update.
    pub append_only: bool,
}

impl SignatureFlags {
    /// The flags of a `/SigFlags` word; the reserved bits are ignored.
    ///
    /// Covers ISO 32000-1 §12.7.2.
    pub fn from_bits(bits: i64) -> SignatureFlags {
        SignatureFlags {
            signatures_exist: bits & 1 != 0,
            append_only: bits & 2 != 0,
        }
    }
}

/// The quadding of variable text (`/Q`, §12.7.3.3): how the text of a field
/// is justified within its widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quadding {
    /// 0: left-justified.
    Left,
    /// 1: centred.
    Centered,
    /// 2: right-justified.
    Right,
}

impl Quadding {
    /// The quadding a `/Q` value names, `None` for anything but 0, 1 and 2.
    pub fn from_int(value: i64) -> Option<Quadding> {
        match value {
            0 => Some(Quadding::Left),
            1 => Some(Quadding::Centered),
            2 => Some(Quadding::Right),
            _ => None,
        }
    }
}

/// The interactive form dictionary (ISO 32000-1 §12.7.2, Table 218).
#[derive(Debug, Clone, PartialEq)]
pub struct InteractiveForm {
    /// `/Fields`: the root fields, those with no parent, as the references
    /// the array holds.
    pub fields: Vec<ObjRef>,
    /// `/NeedAppearances`: whether a reader should build the widgets'
    /// appearance streams itself; false by default.
    pub need_appearances: bool,
    /// `/SigFlags`, no flag set by default.
    pub signature_flags: SignatureFlags,
    /// `/CO`: the fields with calculation actions, in the order their values
    /// are recalculated.
    pub calculation_order: Vec<ObjRef>,
    /// `/DR`: the default resources for the fields' appearance streams.
    pub default_resources: Option<Dict>,
    /// `/DA`: the document-wide default appearance string for variable
    /// text, a content fragment such as `/Helv 0 Tf 0 g`, as written.
    pub default_appearance: Option<String>,
    /// `/Q`: the document-wide default quadding for variable text; `None`
    /// when absent or not 0, 1 or 2.
    pub quadding: Option<Quadding>,
    /// `/XFA`: whether the form carries an XFA resource, as a stream or an
    /// array of packets (§12.7.8).
    pub xfa: bool,
}

/// The catalog's `/AcroForm` dictionary (ISO 32000-1 §12.7.2), `None` when
/// the catalog has none. Indirect values are followed; an entry of the
/// wrong type reads as absent.
///
/// Covers ISO 32000-1 §12.7.2.
pub async fn interactive_form_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
) -> Option<InteractiveForm> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    let dict = resolved_dict(src, catalog.get("AcroForm")?).await?;
    let entries = Entries { src, dict: &dict };
    let fields = references(entries.value("Fields").await.as_ref());
    let calculation_order = references(entries.value("CO").await.as_ref());
    let need_appearances = entries.flag("NeedAppearances").await.unwrap_or(false);
    let signature_flags = entries
        .value("SigFlags")
        .await
        .and_then(|bits| bits.as_int())
        .map_or_else(SignatureFlags::default, SignatureFlags::from_bits);
    let default_resources = match dict.get("DR") {
        Some(entry) => resolved_dict(src, entry).await,
        None => None,
    };
    let default_appearance = entries
        .value("DA")
        .await
        .and_then(|da| Some(String::from_utf8_lossy(da.as_str_bytes()?).into_owned()));
    let quadding = entries
        .value("Q")
        .await
        .and_then(|q| q.as_int())
        .and_then(Quadding::from_int);
    let xfa = matches!(
        entries.value("XFA").await,
        Some(Object::Stream(_)) | Some(Object::Array(_))
    );
    Some(InteractiveForm {
        fields,
        need_appearances,
        signature_flags,
        calculation_order,
        default_resources,
        default_appearance,
        quadding,
        xfa,
    })
}

/// The type of a terminal field (`/FT`, ISO 32000-1 §12.7.3, Table 220).
///
/// Covers ISO 32000-1 §12.7.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// `Btn`: a push button, check box or radio button (§12.7.4.2).
    Button,
    /// `Tx`: a text field (§12.7.4.3).
    Text,
    /// `Ch`: a list box or combo box (§12.7.4.4).
    Choice,
    /// `Sig`: a signature field (§12.7.4.5).
    Signature,
}

impl FieldType {
    /// The type a `/FT` name stands for, `None` for a name Table 220 does
    /// not list.
    ///
    /// Covers ISO 32000-1 §12.7.3.
    pub fn from_name(name: &str) -> Option<FieldType> {
        match name {
            "Btn" => Some(FieldType::Button),
            "Tx" => Some(FieldType::Text),
            "Ch" => Some(FieldType::Choice),
            "Sig" => Some(FieldType::Signature),
            _ => None,
        }
    }
}

/// A field's `/Ff` word (ISO 32000-1 §12.7.3, Table 221). The bits every
/// field type shares are read here; a type's own bits are read by that
/// type's methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FieldFlags(pub u32);

impl FieldFlags {
    /// Bit 1, `ReadOnly`: the user may not change the field's value.
    pub fn read_only(self) -> bool {
        self.0 & 1 != 0
    }

    /// Bit 2, `Required`: the field must have a value when the form is
    /// submitted.
    pub fn required(self) -> bool {
        self.0 & 2 != 0
    }

    /// Bit 3, `NoExport`: a submit-form action leaves the field out.
    pub fn no_export(self) -> bool {
        self.0 & 4 != 0
    }

    /// Bit 13, `Multiline` (text fields, Table 228): the text may span
    /// several lines.
    pub fn multiline(self) -> bool {
        self.bit(13)
    }

    /// Bit 14, `Password` (text fields): the text is echoed unreadably and
    /// should not be stored in the file.
    pub fn password(self) -> bool {
        self.bit(14)
    }

    /// Bit 21, `FileSelect` (text fields): the text is the path of a file
    /// whose contents are the field's value.
    pub fn file_select(self) -> bool {
        self.bit(21)
    }

    /// Bit 23, `DoNotSpellCheck` (text and choice fields): the text is not
    /// spell-checked.
    pub fn do_not_spell_check(self) -> bool {
        self.bit(23)
    }

    /// Bit 24, `DoNotScroll` (text fields): the field takes no more text
    /// than fits its rectangle.
    pub fn do_not_scroll(self) -> bool {
        self.bit(24)
    }

    /// Bit 25, `Comb` (text fields): the text is laid out in `MaxLen`
    /// equally spaced cells.
    pub fn comb(self) -> bool {
        self.bit(25)
    }

    /// Bit 26, `RichText` (text fields): the value is a rich text string
    /// held in `/RV`.
    pub fn rich_text(self) -> bool {
        self.bit(26)
    }

    /// Bit 18, `Combo` (choice fields, Table 230): a combo box rather than
    /// a list box.
    pub fn combo(self) -> bool {
        self.bit(18)
    }

    /// Bit 19, `Edit` (choice fields): the combo box has an editable text
    /// box next to its list.
    pub fn edit(self) -> bool {
        self.bit(19)
    }

    /// Bit 20, `Sort` (choice fields): a writer sorts the options; readers
    /// show them in `/Opt` order regardless.
    pub fn sort(self) -> bool {
        self.bit(20)
    }

    /// Bit 22, `MultiSelect` (choice fields): more than one option may be
    /// selected.
    pub fn multi_select(self) -> bool {
        self.bit(22)
    }

    /// Bit 27, `CommitOnSelChange` (choice fields): a new selection is
    /// committed at once instead of when the field is left.
    pub fn commit_on_sel_change(self) -> bool {
        self.bit(27)
    }

    /// Bit 15, `NoToggleToOff` (radio buttons, Table 226): one button is
    /// always on; clicking the selected one does nothing.
    pub fn no_toggle_to_off(self) -> bool {
        self.bit(15)
    }

    /// Bit 16, `Radio` (button fields): a set of radio buttons rather than
    /// a check box.
    pub fn radio(self) -> bool {
        self.bit(16)
    }

    /// Bit 17, `Pushbutton` (button fields): a pushbutton, which keeps no
    /// value.
    pub fn pushbutton(self) -> bool {
        self.bit(17)
    }

    /// Bit 26, `RadiosInUnison` (radio buttons): buttons sharing an on
    /// state turn on and off together.
    pub fn radios_in_unison(self) -> bool {
        self.bit(26)
    }

    /// Whether the bit at `position`, numbered from 1 as the standard
    /// does, is set.
    fn bit(self, position: u32) -> bool {
        self.0 & (1 << (position - 1)) != 0
    }
}

/// A widget annotation that draws a field (ISO 32000-1 §12.5.6.19), as
/// far as the field's state needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Widget {
    /// The annotation dictionary's reference; the field's own for a merged
    /// field.
    pub object: ObjRef,
    /// `/AS`: the appearance state the widget shows.
    pub appearance_state: Option<String>,
    /// The widget's on state (§12.7.4.2.3): the one key of its `/AP /N`
    /// dictionary other than `Off`. `None` when the normal appearance is a
    /// single stream or names no or several other states.
    pub on_state: Option<String>,
    /// `/MK`: the captions and icons a button is drawn with (§12.7.4.2.2,
    /// Table 189); `None` without the dictionary.
    pub characteristics: Option<AppearanceCharacteristics>,
}

/// The entries of a widget's appearance characteristics dictionary (`/MK`,
/// ISO 32000-1 §12.5.6.19, Table 189) that give a button its captions and
/// icons. The normal caption may sit on any button; the rest are for
/// pushbuttons (§12.7.4.2.2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppearanceCharacteristics {
    /// `/CA`: the normal caption, shown when the button is at rest.
    pub caption: Option<String>,
    /// `/RC`: the rollover caption, shown while the cursor is over the
    /// button.
    pub rollover_caption: Option<String>,
    /// `/AC`: the alternate caption, shown while the mouse button is down.
    pub alternate_caption: Option<String>,
    /// `/I`: the normal icon, a form XObject by reference.
    pub icon: Option<ObjRef>,
    /// `/RI`: the rollover icon.
    pub rollover_icon: Option<ObjRef>,
    /// `/IX`: the alternate icon.
    pub alternate_icon: Option<ObjRef>,
    /// `/TP`: where the caption sits relative to the icon; caption only by
    /// default.
    pub caption_position: CaptionPosition,
}

/// Where a pushbutton's caption sits relative to its icon (`/TP`, ISO
/// 32000-1 §12.5.6.19, Table 189).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionPosition {
    /// 0: no icon, caption only.
    #[default]
    CaptionOnly,
    /// 1: no caption, icon only.
    IconOnly,
    /// 2: caption below the icon.
    Below,
    /// 3: caption above the icon.
    Above,
    /// 4: caption to the right of the icon.
    Right,
    /// 5: caption to the left of the icon.
    Left,
    /// 6: caption overlaid on the icon.
    Overlaid,
}

impl CaptionPosition {
    /// The position a `/TP` code names, `None` for a code outside 0 to 6.
    pub fn from_int(code: i64) -> Option<CaptionPosition> {
        Some(match code {
            0 => CaptionPosition::CaptionOnly,
            1 => CaptionPosition::IconOnly,
            2 => CaptionPosition::Below,
            3 => CaptionPosition::Above,
            4 => CaptionPosition::Right,
            5 => CaptionPosition::Left,
            6 => CaptionPosition::Overlaid,
            _ => return None,
        })
    }
}

/// The three kinds of button field (ISO 32000-1 §12.7.4.2), told apart by
/// the `Pushbutton` and `Radio` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// `Pushbutton` set: a control that keeps no value (§12.7.4.2.2).
    PushButton,
    /// Both flags clear: one or more check boxes toggling between on and
    /// off (§12.7.4.2.3).
    CheckBox,
    /// `Radio` set: a set of related buttons of which normally one is on
    /// (§12.7.4.2.4).
    RadioButtons,
}

/// A signature dictionary (ISO 32000-1 §12.8.1, Table 252) as the value of
/// a signature field, read as data: nothing here is verified.
///
/// Covers ISO 32000-1 §12.8.1.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Signature {
    /// `/Filter`: the preferred signature handler.
    pub filter: Option<String>,
    /// `/SubFilter`: the encoding of the signature value and key
    /// information, such as `adbe.pkcs7.detached`.
    pub sub_filter: Option<String>,
    /// `/ByteRange`: the (offset, length) pairs of the bytes the digest
    /// covers; an incomplete pair or a negative number ends the list.
    pub byte_range: Vec<(u64, u64)>,
    /// `/Contents`: the signature value as written; empty when absent.
    pub contents: Vec<u8>,
    /// `/Name`: the signer named in the dictionary.
    pub name: Option<String>,
    /// `/M`: the time of signing as written, a PDF date string.
    pub signing_time: Option<String>,
    /// `/Location`: where the signing took place.
    pub location: Option<String>,
    /// `/Reason`: why the document was signed.
    pub reason: Option<String>,
    /// `/ContactInfo`: how to reach the signer.
    pub contact_info: Option<String>,
}

impl Signature {
    /// The Table 252 entries of a signature dictionary as data: the filter
    /// and sub-filter names, the byte range pairs, the contents bytes, and
    /// the signer's name, time, location, reason and contact information as
    /// text strings. Nothing is verified. The same reader serves a signature
    /// field's `/V` (§12.7.4.5) and the catalog's permission handlers
    /// (§12.8.4).
    ///
    /// Covers ISO 32000-1 §12.8.1.
    pub fn from_dict(dict: &Dict) -> Signature {
        let text = |key: &str| Some(decode_text_string(dict.get(key)?.as_str_bytes()?));
        let name = |key: &str| Some(dict.get_name(key)?.0.clone());
        Signature {
            filter: name("Filter"),
            sub_filter: name("SubFilter"),
            byte_range: byte_range(dict.get("ByteRange")),
            contents: dict
                .get("Contents")
                .and_then(Object::as_str_bytes)
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            name: text("Name"),
            signing_time: text("M"),
            location: text("Location"),
            reason: text("Reason"),
            contact_info: text("ContactInfo"),
        }
    }
}

/// One entry of a choice field's `/Opt` array (ISO 32000-1 §12.7.4.4,
/// Table 231): the value exported for the option and the text shown for
/// it. A lone text string in the array is both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    /// The export value.
    pub export_value: String,
    /// The text shown to the user; also what `/V` names when the option is
    /// selected.
    pub name: String,
}

/// One field dictionary (ISO 32000-1 §12.7.3, Table 220) with the
/// inheritable entries taken from the nearest ancestor that has them.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// The field dictionary's own reference.
    pub object: ObjRef,
    /// `/Parent`: the field whose `/Kids` holds this one, as walked; `None`
    /// for a root field.
    pub parent: Option<ObjRef>,
    /// The `/Kids` that are fields themselves, in array order.
    pub kids: Vec<ObjRef>,
    /// The widget annotations that draw this field: the `/Kids` that are
    /// `/Subtype /Widget` dictionaries without a `/T` of their own, or the
    /// field itself when its single widget is merged into it.
    pub widgets: Vec<Widget>,
    /// `/FT`, inherited; `None` for a non-terminal field that names none
    /// and for a name Table 220 does not list.
    pub field_type: Option<FieldType>,
    /// `/T`: the partial field name (§12.7.3.2).
    pub partial_name: Option<String>,
    /// The fully qualified field name (§12.7.3.2): the partial names from
    /// the root field down, joined by a period; a field without a `/T`
    /// shares its parent's name, and a root without one has the empty name.
    pub name: String,
    /// `/TU`: the name shown to the user in place of the field name.
    pub alternate_name: Option<String>,
    /// `/TM`: the name used when the field's data is exported.
    pub mapping_name: Option<String>,
    /// `/Ff`, inherited; no bit set by default.
    pub flags: FieldFlags,
    /// `/V`, inherited, resolved one level, in the format of the field's
    /// type; a merged widget's own value beats its parent's.
    pub value: Option<Object>,
    /// `/DV`, inherited, resolved one level: the value a reset-form action
    /// restores.
    pub default_value: Option<Object>,
    /// `/MaxLen`, inherited (§12.7.4.3, Table 229): the most characters a
    /// text field's text may hold.
    pub max_len: Option<u32>,
    /// `/Opt`, inherited: the options of a choice field (§12.7.4.4, Table
    /// 231) or the export values of a check box or radio button
    /// (§12.7.4.2, Table 227); an entry that is neither a text string nor a
    /// pair of them is skipped.
    pub options: Vec<ChoiceOption>,
    /// `/TI` (§12.7.4.4): the index into `options` of the first option a
    /// scrollable list box shows; 0 by default.
    pub top_index: u32,
    /// `/I` (§12.7.4.4): the indices into `options` of the selected options
    /// of a multi-select choice field, as written.
    pub selected_indices: Vec<u32>,
    /// `/AA`: the field's additional-actions dictionary, as written.
    pub additional_actions: Option<Dict>,
    /// `/Lock` (§12.7.4.5, Table 232): the signature field lock dictionary
    /// naming the fields that lock once this field is signed, by reference.
    pub lock: Option<ObjRef>,
    /// `/SV` (§12.7.4.5, Table 232): the seed value dictionary constraining
    /// a signature applied to this field, by reference.
    pub seed_value: Option<ObjRef>,
}

impl FormField {
    /// The text of a text field (ISO 32000-1 §12.7.4.3): `/V` decoded as a
    /// text string. `None` for a field of another type or without a value.
    pub fn text(&self) -> Option<String> {
        if self.field_type != Some(FieldType::Text) {
            return None;
        }
        Some(decode_text_string(self.value.as_ref()?.as_str_bytes()?))
    }

    /// Which kind of button a button field is (ISO 32000-1 §12.7.4.2,
    /// Table 226): a pushbutton when the `Pushbutton` flag is set, radio
    /// buttons when `Radio` is set, a check box otherwise. `None` for a
    /// field of another type.
    pub fn button_kind(&self) -> Option<ButtonKind> {
        if self.field_type != Some(FieldType::Button) {
            return None;
        }
        Some(if self.flags.pushbutton() {
            ButtonKind::PushButton
        } else if self.flags.radio() {
            ButtonKind::RadioButtons
        } else {
            ButtonKind::CheckBox
        })
    }

    /// The appearance state a check box or radio button field is in (ISO
    /// 32000-1 §12.7.4.2): `/V` as a name, the one the widgets' `/AP /N`
    /// dictionaries key their on and off appearances by, `Off` when there
    /// is no value (§12.7.4.2.4's default). `None` for a pushbutton, which
    /// keeps no value, for another field type, and for a value that is no
    /// name.
    pub fn state(&self) -> Option<&str> {
        match self.button_kind()? {
            ButtonKind::PushButton => None,
            ButtonKind::CheckBox | ButtonKind::RadioButtons => match &self.value {
                None => Some("Off"),
                Some(value) => Some(value.as_name()?.0.as_str()),
            },
        }
    }

    /// Whether a check box is checked (ISO 32000-1 §12.7.4.2.3): its state
    /// is a name other than `Off`; an absent value is the off state. `None`
    /// for a field that is no check box.
    pub fn checked(&self) -> Option<bool> {
        (self.button_kind()? == ButtonKind::CheckBox)
            .then(|| self.state().is_some_and(|state| state != "Off"))
    }

    /// The widgets of a check box or radio button field that are in the on
    /// state (ISO 32000-1 §12.7.4.2.3, §12.7.4.2.4): those whose on state
    /// is the field's state, as indices into `widgets`. The same indices
    /// pick the Table 227 export values out of `options`. Empty for a field
    /// in the off state, a pushbutton, or another field type.
    pub fn on_widgets(&self) -> Vec<usize> {
        let Some(state) = self.state().filter(|state| *state != "Off") else {
            return Vec::new();
        };
        self.widgets
            .iter()
            .enumerate()
            .filter(|(_, widget)| widget.on_state.as_deref() == Some(state))
            .map(|(index, _)| index)
            .collect()
    }

    /// The signature a signature field holds (ISO 32000-1 §12.7.4.5): its
    /// `/V` read as a signature dictionary. `None` for another field type,
    /// an unsigned field, or a value that is no dictionary.
    pub fn signature(&self) -> Option<Signature> {
        if self.field_type != Some(FieldType::Signature) {
            return None;
        }
        Some(Signature::from_dict(self.value.as_ref()?.as_dict()?))
    }

    /// The names of a choice field's selected options (ISO 32000-1
    /// §12.7.4.4): `/V` as one text string or, for a multi-select field,
    /// an array of them. Empty for a field of another type, without a
    /// value, or whose value is neither.
    pub fn selected(&self) -> Vec<String> {
        if self.field_type != Some(FieldType::Choice) {
            return Vec::new();
        }
        match &self.value {
            Some(Object::String(name)) => vec![decode_text_string(name)],
            Some(Object::Array(names)) => names
                .iter()
                .filter_map(|name| Some(decode_text_string(name.as_str_bytes()?)))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Every field dictionary reachable from the form's `/Fields`, depth first
/// in array order, each with `/FT`, `/Ff`, `/V` and `/DV` filled from the
/// nearest ancestor when its own dictionary lacks them. A `/Kids` entry
/// that is a `/Subtype /Widget` dictionary without a `/T` is one of its
/// field's widgets, not a field; a reference seen before, one that is not
/// a dictionary, and anything deeper than the field depth cap is skipped.
///
/// Covers ISO 32000-1 §12.7.3.
pub async fn form_fields_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Vec<FormField> {
    let mut fields = Vec::new();
    let Some(form) = interactive_form_with(src, trailer).await else {
        return fields;
    };
    let mut visited: FastSet<ObjRef> = FastSet::default();
    let mut stack = Vec::new();
    for object in form.fields.into_iter().rev() {
        if let Some(dict) = field_dict(src, object).await {
            stack.push(Pending {
                object,
                dict,
                parent: None,
                inherited: Inherited::default(),
                depth: 0,
            });
        }
    }
    while let Some(pending) = stack.pop() {
        if !visited.insert(pending.object) {
            continue;
        }
        let (field, children) = read_field(src, pending, &visited).await;
        fields.push(field);
        stack.extend(children.into_iter().rev());
    }
    fields
}

/// The entries a field takes from its parent when its own dictionary lacks
/// them, the rows of Table 220 marked inheritable, and the parent's fully
/// qualified name the field's own name is built on.
#[derive(Clone, Default)]
struct Inherited {
    field_type: Option<FieldType>,
    flags: Option<FieldFlags>,
    value: Option<Object>,
    default_value: Option<Object>,
    name: String,
    max_len: Option<u32>,
    options: Vec<ChoiceOption>,
}

/// A field dictionary waiting to be read, with what it inherits.
struct Pending {
    object: ObjRef,
    dict: Dict,
    parent: Option<ObjRef>,
    inherited: Inherited,
    depth: usize,
}

/// Reads one field and classifies its `/Kids` into widgets and child
/// fields; the children come back ready to be read with what they inherit.
/// A kid already visited, one that is not a dictionary, and every kid of a
/// field at the depth cap is skipped.
async fn read_field<S: AsyncObjectSource>(
    src: &S,
    pending: Pending,
    visited: &FastSet<ObjRef>,
) -> (FormField, Vec<Pending>) {
    let Pending {
        object,
        dict,
        parent,
        inherited,
        depth,
    } = pending;
    let entries = Entries { src, dict: &dict };
    let field_type = entries
        .value("FT")
        .await
        .and_then(|ft| FieldType::from_name(&ft.as_name()?.0))
        .or(inherited.field_type);
    let flags = entries
        .value("Ff")
        .await
        .and_then(|ff| u32::try_from(ff.as_int()?).ok())
        .map(FieldFlags)
        .or(inherited.flags)
        .unwrap_or_default();
    let value = match entries.value("V").await.or(inherited.value) {
        // A text field's value may be a stream since PDF 1.5 (§12.7.4.3);
        // its data is the text.
        Some(Object::Stream(stream)) if field_type == Some(FieldType::Text) => {
            src.stream_data(&stream).await.ok().map(Object::String)
        }
        value => value,
    };
    let default_value = entries.value("DV").await.or(inherited.default_value);
    let max_len = entries
        .value("MaxLen")
        .await
        .and_then(|len| u32::try_from(len.as_int()?).ok())
        .or(inherited.max_len);
    let options = match entries.value("Opt").await {
        Some(opt) => choice_options(src, &opt).await,
        None => inherited.options,
    };
    let top_index = entries
        .value("TI")
        .await
        .and_then(|ti| u32::try_from(ti.as_int()?).ok())
        .unwrap_or(0);
    let selected_indices = entries
        .value("I")
        .await
        .and_then(|indices| {
            Some(
                indices
                    .as_array()?
                    .iter()
                    .filter_map(|index| u32::try_from(index.as_int()?).ok())
                    .collect(),
            )
        })
        .unwrap_or_default();
    let partial_name = entries.text("T").await;
    let name = qualified_name(&inherited.name, partial_name.as_deref());
    let additional_actions = match dict.get("AA") {
        Some(entry) => resolved_dict(src, entry).await,
        None => None,
    };
    let mut widgets = Vec::new();
    if is_widget(&dict) {
        widgets.push(widget(src, object, &dict).await);
    }
    let mut kids = Vec::new();
    let mut children = Vec::new();
    if depth < MAX_FIELD_DEPTH {
        for kid in references(entries.value("Kids").await.as_ref()) {
            if visited.contains(&kid) {
                continue;
            }
            let Some(kid_dict) = field_dict(src, kid).await else {
                continue;
            };
            if is_widget(&kid_dict) && kid_dict.get("T").is_none() {
                widgets.push(widget(src, kid, &kid_dict).await);
                continue;
            }
            kids.push(kid);
            children.push(Pending {
                object: kid,
                dict: kid_dict,
                parent: Some(object),
                inherited: Inherited {
                    field_type,
                    flags: Some(flags),
                    value: value.clone(),
                    default_value: default_value.clone(),
                    name: name.clone(),
                    max_len,
                    options: options.clone(),
                },
                depth: depth + 1,
            });
        }
    }
    let field = FormField {
        object,
        parent,
        kids,
        widgets,
        field_type,
        partial_name,
        name,
        alternate_name: entries.text("TU").await,
        mapping_name: entries.text("TM").await,
        flags,
        value,
        default_value,
        max_len,
        options,
        top_index,
        selected_indices,
        additional_actions,
        lock: dict.get("Lock").and_then(Object::as_ref),
        seed_value: dict.get("SV").and_then(Object::as_ref),
    };
    (field, children)
}

/// The fully qualified name of a field with partial name `partial` under
/// a parent named `parent`: the two joined by a period, the partial name
/// alone under an unnamed root, and the parent's name for a field without
/// a `/T`.
///
/// Covers ISO 32000-1 §12.7.3.2.
fn qualified_name(parent: &str, partial: Option<&str>) -> String {
    match partial {
        Some(partial) if parent.is_empty() => partial.to_string(),
        Some(partial) => format!("{parent}.{partial}"),
        None => parent.to_string(),
    }
}

/// The options an `/Opt` array holds (ISO 32000-1 §12.7.4.4, Table 231):
/// a text string is an option whose export value is its name, a pair of
/// text strings is an export value and a name; indirect entries are
/// followed and anything else is skipped.
async fn choice_options<S: AsyncObjectSource>(src: &S, opt: &Object) -> Vec<ChoiceOption> {
    let Some(items) = opt.as_array() else {
        return Vec::new();
    };
    let mut options = Vec::new();
    for item in items {
        let Ok(item) = src.resolve(item).await else {
            continue;
        };
        match &item {
            Object::String(name) => {
                let name = decode_text_string(name);
                options.push(ChoiceOption {
                    export_value: name.clone(),
                    name,
                });
            }
            Object::Array(pair) => {
                let [export_value, name] = pair.as_slice() else {
                    continue;
                };
                let (Some(export_value), Some(name)) =
                    (export_value.as_str_bytes(), name.as_str_bytes())
                else {
                    continue;
                };
                options.push(ChoiceOption {
                    export_value: decode_text_string(export_value),
                    name: decode_text_string(name),
                });
            }
            _ => {}
        }
    }
    options
}

/// The (offset, length) pairs of a `/ByteRange` array (ISO 32000-1
/// §12.8.1): complete pairs of non-negative integers up to the first
/// element that is not one.
fn byte_range(value: Option<&Object>) -> Vec<(u64, u64)> {
    let Some(items) = value.and_then(Object::as_array) else {
        return Vec::new();
    };
    items
        .as_chunks::<2>()
        .0
        .iter()
        .map_while(|[offset, length]| {
            Some((
                u64::try_from(offset.as_int()?).ok()?,
                u64::try_from(length.as_int()?).ok()?,
            ))
        })
        .collect()
}

/// The dictionary a reference points at, `None` for anything else.
async fn field_dict<S: AsyncObjectSource>(src: &S, object: ObjRef) -> Option<Dict> {
    match src.get(object).await.ok()? {
        Object::Dict(dict) => Some(dict),
        _ => None,
    }
}

/// The widget record of an annotation dictionary: its reference, its
/// `/AS` and its on state.
async fn widget<S: AsyncObjectSource>(src: &S, object: ObjRef, dict: &Dict) -> Widget {
    Widget {
        object,
        appearance_state: dict.get_name("AS").map(|state| state.0.clone()),
        on_state: on_state(src, dict).await,
        characteristics: characteristics(src, dict).await,
    }
}

/// The caption and icon entries of a widget's `/MK` dictionary (Table
/// 189); `None` without the dictionary. A caption that is no string, an
/// icon that is no reference and a `/TP` outside 0 to 6 read as absent.
///
/// Covers ISO 32000-1 §12.7.4.2.2.
async fn characteristics<S: AsyncObjectSource>(
    src: &S,
    dict: &Dict,
) -> Option<AppearanceCharacteristics> {
    let mk = resolved_dict(src, dict.get("MK")?).await?;
    let entries = Entries { src, dict: &mk };
    let reference = |key: &str| mk.get(key).and_then(Object::as_ref);
    Some(AppearanceCharacteristics {
        caption: entries.text("CA").await,
        rollover_caption: entries.text("RC").await,
        alternate_caption: entries.text("AC").await,
        icon: reference("I"),
        rollover_icon: reference("RI"),
        alternate_icon: reference("IX"),
        caption_position: entries
            .value("TP")
            .await
            .and_then(|code| CaptionPosition::from_int(code.as_int()?))
            .unwrap_or_default(),
    })
}

/// The on state of a widget (ISO 32000-1 §12.7.4.2.3): the one key of its
/// `/AP /N` dictionary other than `Off`; `None` when `/N` is a single
/// stream or names no or several other states.
async fn on_state<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<String> {
    let appearance = resolved_dict(src, dict.get("AP")?).await?;
    let Object::Dict(states) = src.resolve(appearance.get("N")?).await.ok()? else {
        return None;
    };
    let mut on = states
        .iter()
        .map(|(state, _)| state)
        .filter(|state| state.0 != "Off");
    let state = on.next()?;
    on.next().is_none().then(|| state.0.clone())
}

/// Whether a dictionary is a widget annotation (`/Subtype /Widget`).
fn is_widget(dict: &Dict) -> bool {
    dict.get_name("Subtype")
        .is_some_and(|subtype| subtype.0 == "Widget")
}

/// The references an array holds, anything else in it skipped; empty for a
/// value that is no array.
fn references(value: Option<&Object>) -> Vec<ObjRef> {
    value
        .and_then(Object::as_array)
        .map(|items| items.iter().filter_map(Object::as_ref).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        AppearanceCharacteristics, ButtonKind, CaptionPosition, ChoiceOption, FieldFlags,
        FieldType, FormField, InteractiveForm, Quadding, Signature, SignatureFlags, Widget,
    };
    use crate::object::{Name, ObjRef, Object};
    use crate::Document;
    use pdfboss_testkit::PdfBuilder;

    fn doc(catalog_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).unwrap()
    }

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    fn widget_refs(field: &FormField) -> Vec<ObjRef> {
        field.widgets.iter().map(|widget| widget.object).collect()
    }

    /// `doc` with one stream object added.
    fn doc_with_stream(
        catalog_extra: &str,
        objects: &[(u32, &str)],
        stream: (u32, &str, &[u8]),
    ) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        for (num, body) in objects {
            b.object(*num, body);
        }
        b.stream(stream.0, stream.1, stream.2);
        Document::load(b.build(1)).unwrap()
    }

    /// Every entry of Table 218 set, the dictionary itself and its flags
    /// indirect; a direct dictionary in `/Fields` is not a reference and is
    /// skipped.
    // Covers ISO 32000-1 §12.7.2.
    #[test]
    fn the_interactive_form_dictionary_is_read() {
        let found = doc(
            "/AcroForm 4 0 R",
            &[
                (
                    4,
                    "<< /Fields [5 0 R << /T (inline) >> 6 0 R] /NeedAppearances true \
                     /SigFlags 7 0 R /CO [6 0 R] /DR << /Font << /Helv 8 0 R >> >> \
                     /DA (/Helv 0 Tf 0 g) /Q 1 /XFA [(preamble) 9 0 R] >>",
                ),
                (5, "<< /T (a) /FT /Tx >>"),
                (6, "<< /T (b) /FT /Tx >>"),
                (7, "3"),
                (8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            ],
        )
        .interactive_form()
        .unwrap();
        assert_eq!(found.fields, vec![r(5), r(6)]);
        assert!(found.need_appearances);
        assert_eq!(
            found.signature_flags,
            SignatureFlags {
                signatures_exist: true,
                append_only: true,
            }
        );
        assert_eq!(found.calculation_order, vec![r(6)]);
        assert!(found
            .default_resources
            .as_ref()
            .and_then(|dr| dr.get_dict("Font"))
            .is_some());
        assert_eq!(found.default_appearance.as_deref(), Some("/Helv 0 Tf 0 g"));
        assert_eq!(found.quadding, Some(Quadding::Centered));
        assert!(found.xfa);
    }

    /// A catalog without the dictionary has no form; an empty dictionary
    /// holds the table's defaults; a quadding the table does not define and
    /// a `/Fields` that is no array read as absent.
    // Covers ISO 32000-1 §12.7.2.
    #[test]
    fn missing_and_malformed_form_entries_take_the_defaults() {
        assert_eq!(doc("", &[]).interactive_form(), None);
        assert_eq!(doc("/AcroForm 5", &[]).interactive_form(), None);
        assert_eq!(
            doc("/AcroForm << >>", &[]).interactive_form(),
            Some(InteractiveForm {
                fields: Vec::new(),
                need_appearances: false,
                signature_flags: SignatureFlags::default(),
                calculation_order: Vec::new(),
                default_resources: None,
                default_appearance: None,
                quadding: None,
                xfa: false,
            })
        );
        let odd = doc(
            "/AcroForm << /Fields 5 0 R /Q 7 /SigFlags 4 >>",
            &[(5, "(x)")],
        )
        .interactive_form()
        .unwrap();
        assert_eq!(odd.fields, Vec::new());
        assert_eq!(odd.quadding, None);
        assert_eq!(odd.signature_flags, SignatureFlags::default());
    }

    /// A root field with its single widget merged into it (Table 220's
    /// `Kids` omitted): every non-inheritable entry read as written, the
    /// text strings decoded, the field its own widget.
    // Covers ISO 32000-1 §12.7.3.
    #[test]
    fn a_merged_field_is_read_with_itself_as_its_widget() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (
                    5,
                    "<< /FT /Tx /T <FEFF004E0061006D0065> /TU (Your name) /TM (name_export) \
                 /Ff 7 /V (Ada) /DV 6 0 R /AA << /F 7 0 R >> /Subtype /Widget \
                 /Rect [0 0 10 10] >>",
                ),
                (6, "(nobody)"),
            ],
        )
        .form_fields();
        assert_eq!(fields.len(), 1);
        let field = &fields[0];
        assert_eq!(field.object, r(5));
        assert_eq!(field.parent, None);
        assert_eq!(field.kids, Vec::new());
        assert_eq!(widget_refs(field), vec![r(5)]);
        assert_eq!(field.field_type, Some(FieldType::Text));
        assert_eq!(field.partial_name.as_deref(), Some("Name"));
        assert_eq!(field.alternate_name.as_deref(), Some("Your name"));
        assert_eq!(field.mapping_name.as_deref(), Some("name_export"));
        assert!(field.flags.read_only() && field.flags.required() && field.flags.no_export());
        assert_eq!(field.value, Some(Object::String(b"Ada".to_vec())));
        assert_eq!(
            field.default_value,
            Some(Object::String(b"nobody".to_vec()))
        );
        assert!(field
            .additional_actions
            .as_ref()
            .is_some_and(|aa| aa.get("F").is_some()));
    }

    /// A radio button field whose `/Kids` are two widget annotations: one
    /// field, two widgets, no child fields.
    // Covers ISO 32000-1 §12.7.3.
    #[test]
    fn widget_kids_belong_to_their_field_and_are_not_fields() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (
                    5,
                    "<< /FT /Btn /Ff 32768 /T (card) /V /visa /Kids [6 0 R 7 0 R] >>",
                ),
                (
                    6,
                    "<< /Subtype /Widget /Parent 5 0 R /AS /visa /Rect [0 0 1 1] >>",
                ),
                (
                    7,
                    "<< /Subtype /Widget /Parent 5 0 R /AS /Off /Rect [0 0 1 1] >>",
                ),
            ],
        )
        .form_fields();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].kids, Vec::new());
        assert_eq!(widget_refs(&fields[0]), vec![r(6), r(7)]);
        assert_eq!(fields[0].value, Some(Object::Name(Name("visa".into()))));
    }

    /// Fields are listed depth first in array order; a child takes `/FT`,
    /// `/Ff`, `/V` and `/DV` from its parent unless it has its own, keeps
    /// its own `/T`, and a kid with a `/T` is a field even when it is also
    /// a widget.
    // Covers ISO 32000-1 §12.7.3.
    #[test]
    fn children_inherit_the_inheritable_entries_from_their_parent() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 9 0 R] >>",
            &[
                (5, "<< /T (personal) /FT /Tx /Ff 1 /V (from parent) /DV (reset) /Kids [6 0 R 7 0 R] >>"),
                (6, "<< /T (first) /Parent 5 0 R /Subtype /Widget /Rect [0 0 1 1] >>"),
                (7, "<< /T (last) /Parent 5 0 R /Ff 2 /V (own) /Kids [8 0 R] >>"),
                (8, "<< /Subtype /Widget /Parent 7 0 R /Rect [0 0 1 1] >>"),
                (9, "<< /T (other) /FT /Ch /Subtype /Widget /Rect [0 0 1 1] >>"),
            ],
        )
        .form_fields();
        let objects: Vec<ObjRef> = fields.iter().map(|f| f.object).collect();
        assert_eq!(objects, vec![r(5), r(6), r(7), r(9)]);
        let root = &fields[0];
        assert_eq!(root.kids, vec![r(6), r(7)]);
        assert_eq!(widget_refs(root), Vec::new());
        let first = &fields[1];
        assert_eq!(first.parent, Some(r(5)));
        assert_eq!(first.partial_name.as_deref(), Some("first"));
        assert_eq!(first.field_type, Some(FieldType::Text));
        assert_eq!(first.flags, FieldFlags(1));
        assert_eq!(first.value, Some(Object::String(b"from parent".to_vec())));
        assert_eq!(first.default_value, Some(Object::String(b"reset".to_vec())));
        assert_eq!(widget_refs(first), vec![r(6)]);
        let last = &fields[2];
        assert_eq!(last.flags, FieldFlags(2));
        assert_eq!(last.value, Some(Object::String(b"own".to_vec())));
        assert_eq!(last.default_value, Some(Object::String(b"reset".to_vec())));
        assert_eq!(widget_refs(last), vec![r(8)]);
        assert_eq!(fields[3].field_type, Some(FieldType::Choice));
        assert_eq!(fields[3].flags, FieldFlags::default());
    }

    /// No form or no fields gives an empty list; a `/Fields` or `/Kids`
    /// entry that is not a dictionary, a kid that points back up the tree
    /// and an `/FT` name Table 220 does not list are skipped or read as
    /// absent.
    // Covers ISO 32000-1 §12.7.3.
    #[test]
    fn malformed_field_trees_are_walked_without_repeats() {
        assert_eq!(doc("", &[]).form_fields(), Vec::new());
        assert_eq!(doc("/AcroForm << >>", &[]).form_fields(), Vec::new());
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >>",
            &[
                (5, "(not a field)"),
                (6, "<< /T (loop) /FT /Bogus /Kids [6 0 R 8 0 R 7 0 R] >>"),
                (7, "<< /T (leaf) /FT /Sig >>"),
                (8, "42"),
            ],
        )
        .form_fields();
        let objects: Vec<ObjRef> = fields.iter().map(|f| f.object).collect();
        assert_eq!(objects, vec![r(6), r(7)]);
        assert_eq!(fields[0].field_type, None);
        assert_eq!(fields[0].kids, vec![r(7)]);
        assert_eq!(fields[1].parent, Some(r(6)));
        assert_eq!(fields[1].field_type, Some(FieldType::Signature));
    }

    /// The standard's own example: PersonalData, Address and ZipCode nested
    /// three deep give the leaf the name PersonalData.Address.ZipCode, and
    /// each level's name is a prefix of its children's.
    // Covers ISO 32000-1 §12.7.3.2.
    #[test]
    fn fully_qualified_names_join_the_partial_names_with_periods() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (5, "<< /T (PersonalData) /Kids [6 0 R] >>"),
                (6, "<< /T (Address) /Parent 5 0 R /Kids [7 0 R] >>"),
                (7, "<< /T (ZipCode) /FT /Tx /Parent 6 0 R >>"),
            ],
        )
        .form_fields();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "PersonalData",
                "PersonalData.Address",
                "PersonalData.Address.ZipCode"
            ]
        );
    }

    /// A kid without a `/T` is another representation of its parent's
    /// field and shares the fully qualified name; a root without a `/T`
    /// has the empty name and its named child carries no leading period.
    // Covers ISO 32000-1 §12.7.3.2.
    #[test]
    fn fields_without_a_partial_name_share_their_parent_s_name() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 8 0 R] >>",
            &[
                (5, "<< /T (choice) /FT /Btn /Kids [6 0 R 7 0 R] >>"),
                (6, "<< /Parent 5 0 R /Kids [9 0 R] >>"),
                (7, "<< /Parent 5 0 R /Subtype /Widget /Rect [0 0 1 1] >>"),
                (8, "<< /FT /Tx /Kids [10 0 R] >>"),
                (9, "<< /Parent 6 0 R /Subtype /Widget /Rect [0 0 1 1] >>"),
                (10, "<< /T (lonely) /Parent 8 0 R >>"),
            ],
        )
        .form_fields();
        let names: Vec<(ObjRef, &str)> =
            fields.iter().map(|f| (f.object, f.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (r(5), "choice"),
                (r(6), "choice"),
                (r(8), ""),
                (r(10), "lonely")
            ]
        );
        assert_eq!(fields[1].partial_name, None);
    }

    /// A text field's value is a text string; `/MaxLen` is inheritable; the
    /// Table 228 bits are read by position, numbered from 1.
    // Covers ISO 32000-1 §12.7.4.3.
    #[test]
    fn text_fields_decode_their_value_and_read_max_len_and_their_flags() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (
                    5,
                    "<< /T (box) /FT /Tx /MaxLen 8 /Ff 16781312 /Kids [6 0 R] >>",
                ),
                (6, "<< /T (line) /Parent 5 0 R /V <FEFF00480069> >>"),
            ],
        )
        .form_fields();
        let line = &fields[1];
        assert_eq!(line.text().as_deref(), Some("Hi"));
        assert_eq!(line.max_len, Some(8));
        assert!(line.flags.multiline() && line.flags.comb());
        assert!(!line.flags.password() && !line.flags.file_select());
        assert!(!line.flags.do_not_spell_check() && !line.flags.do_not_scroll());
        assert!(!line.flags.rich_text());
        assert_eq!(fields[0].text(), None);
        let all = FieldFlags((1 << 25) | (1 << 24) | (1 << 23) | (1 << 22) | (1 << 20) | (1 << 13));
        assert!(all.password() && all.file_select() && all.do_not_spell_check());
        assert!(all.do_not_scroll() && all.comb() && all.rich_text());
        assert!(!all.multiline());
    }

    /// Since PDF 1.5 the value may be a stream; its data is the text.
    // Covers ISO 32000-1 §12.7.4.3.
    #[test]
    fn a_stream_value_of_a_text_field_is_read_as_its_text() {
        let fields = doc_with_stream(
            "/AcroForm << /Fields [5 0 R] >>",
            &[(5, "<< /T (essay) /FT /Tx /V 6 0 R >>")],
            (6, "", b"From a stream"),
        )
        .form_fields();
        assert_eq!(fields[0].text().as_deref(), Some("From a stream"));
        assert_eq!(
            fields[0].value,
            Some(Object::String(b"From a stream".to_vec()))
        );
    }

    /// Only text fields have text: a button's name value and a text field
    /// without a value give none, and a value that is no string or stream
    /// gives none.
    // Covers ISO 32000-1 §12.7.4.3.
    #[test]
    fn other_field_types_and_odd_values_have_no_text() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >>",
            &[
                (5, "<< /T (check) /FT /Btn /V /Yes >>"),
                (6, "<< /T (empty) /FT /Tx >>"),
                (7, "<< /T (odd) /FT /Tx /V 42 /MaxLen -1 >>"),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].text(), None);
        assert_eq!(fields[1].text(), None);
        assert_eq!(fields[2].text(), None);
        assert_eq!(fields[2].max_len, None);
    }

    /// A combo box: `/Opt` mixes lone names with export/name pairs, `/TI`
    /// and `/I` are read, `/V` names the selected option, and the Table
    /// 230 bits are read by position.
    // Covers ISO 32000-1 §12.7.4.4.
    #[test]
    fn choice_fields_list_their_options_top_index_and_selection() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[(
                5,
                "<< /T (colour) /FT /Ch /Ff 393216 /Opt [(Red) [(b) (Blue)] <FEFF0047>] \
                 /TI 1 /I [1] /V (Blue) >>",
            )],
        )
        .form_fields();
        let field = &fields[0];
        assert_eq!(
            field.options,
            vec![
                ChoiceOption {
                    export_value: "Red".into(),
                    name: "Red".into(),
                },
                ChoiceOption {
                    export_value: "b".into(),
                    name: "Blue".into(),
                },
                ChoiceOption {
                    export_value: "G".into(),
                    name: "G".into(),
                },
            ]
        );
        assert_eq!(field.top_index, 1);
        assert_eq!(field.selected_indices, vec![1]);
        assert_eq!(field.selected(), vec!["Blue".to_string()]);
        assert!(field.flags.combo() && field.flags.edit());
        assert!(!field.flags.sort() && !field.flags.multi_select());
        assert!(!field.flags.commit_on_sel_change());
    }

    /// A multi-select list box holds an array of names in `/V`; an indirect
    /// `/Opt` array with an indirect element is followed; `/Opt` is taken
    /// from the parent when the field has none.
    // Covers ISO 32000-1 §12.7.4.4.
    #[test]
    fn multi_select_choice_fields_hold_several_selected_names() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (
                    5,
                    "<< /T (toppings) /FT /Ch /Ff 69730304 /Opt 6 0 R /Kids [8 0 R] >>",
                ),
                (6, "[(a) 7 0 R (c)]"),
                (7, "[(bee) (b)]"),
                (8, "<< /Parent 5 0 R /V [(a) (c)] /I [0 2] >>"),
            ],
        )
        .form_fields();
        let kid = &fields[1];
        assert_eq!(kid.options.len(), 3);
        assert_eq!(kid.options[1].export_value, "bee");
        assert_eq!(kid.selected(), vec!["a".to_string(), "c".to_string()]);
        assert_eq!(kid.selected_indices, vec![0, 2]);
        assert!(kid.flags.sort() && kid.flags.multi_select() && kid.flags.commit_on_sel_change());
        assert!(!kid.flags.combo());
        assert_eq!(fields[0].selected(), Vec::<String>::new());
    }

    /// Other field types have no selection even with a string value; a
    /// choice field without `/Opt` has no options and the default top
    /// index; an `/Opt` entry that is a number or a three-element array, a
    /// negative `/TI` and a negative index in `/I` are skipped.
    // Covers ISO 32000-1 §12.7.4.4.
    #[test]
    fn choice_entries_default_for_other_types_and_skip_odd_values() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >>",
            &[
                (5, "<< /T (text) /FT /Tx /V (x) /Opt [(y)] >>"),
                (6, "<< /T (bare) /FT /Ch >>"),
                (
                    7,
                    "<< /T (odd) /FT /Ch /Opt [7 [(a) (b) (c)] (ok)] /TI -3 /I [-1 0] /V 5 >>",
                ),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].selected(), Vec::<String>::new());
        assert_eq!(fields[0].options.len(), 1);
        assert_eq!(fields[1].options, Vec::new());
        assert_eq!(fields[1].top_index, 0);
        assert_eq!(fields[1].selected(), Vec::<String>::new());
        assert_eq!(fields[2].options.len(), 1);
        assert_eq!(fields[2].options[0].name, "ok");
        assert_eq!(fields[2].top_index, 0);
        assert_eq!(fields[2].selected_indices, vec![0]);
        assert_eq!(fields[2].selected(), Vec::<String>::new());
    }

    /// The `Pushbutton` and `Radio` bits pick the kind; `NoToggleToOff` and
    /// `RadiosInUnison` are read alongside; a field of another type has no
    /// kind.
    // Covers ISO 32000-1 §12.7.4.2.
    #[test]
    fn button_fields_are_told_apart_by_their_flags() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R 8 0 R] >>",
            &[
                (5, "<< /T (go) /FT /Btn /Ff 65536 >>"),
                (6, "<< /T (card) /FT /Btn /Ff 33603584 /V /visa >>"),
                (7, "<< /T (urgent) /FT /Btn /V /Yes >>"),
                (8, "<< /T (name) /FT /Tx /V (x) >>"),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].button_kind(), Some(ButtonKind::PushButton));
        assert!(fields[0].flags.pushbutton() && !fields[0].flags.radio());
        assert_eq!(fields[1].button_kind(), Some(ButtonKind::RadioButtons));
        assert!(fields[1].flags.radio() && fields[1].flags.no_toggle_to_off());
        assert!(fields[1].flags.radios_in_unison() && !fields[1].flags.pushbutton());
        assert_eq!(fields[2].button_kind(), Some(ButtonKind::CheckBox));
        assert!(!fields[2].flags.no_toggle_to_off() && !fields[2].flags.radios_in_unison());
        assert_eq!(fields[3].button_kind(), None);
    }

    /// A check box's or radio button field's `/V` names its appearance
    /// state; a pushbutton keeps no value even when one is written; a
    /// missing value is the `Off` state and a value that is no name gives
    /// none.
    // Covers ISO 32000-1 §12.7.4.2, §12.7.4.2.4.
    #[test]
    fn a_button_field_s_state_is_its_value_name() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] >>",
            &[
                (5, "<< /T (urgent) /FT /Btn /V /Yes >>"),
                (6, "<< /T (card) /FT /Btn /Ff 32768 /V /cardbrand1 >>"),
                (7, "<< /T (blank) /FT /Btn >>"),
                (8, "<< /T (go) /FT /Btn /Ff 65536 /V /Yes >>"),
                (9, "<< /T (odd) /FT /Btn /V (Yes) >>"),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].state(), Some("Yes"));
        assert_eq!(fields[1].state(), Some("cardbrand1"));
        assert_eq!(fields[2].state(), Some("Off"));
        assert_eq!(fields[3].state(), None);
        assert_eq!(fields[4].state(), None);
    }

    /// A merged check box: `/V /Yes` is checked, `/V /Off` and no value are
    /// not; its widget carries `/AS` and the on state read from `/AP /N`;
    /// a radio field and a text field have no checked state.
    // Covers ISO 32000-1 §12.7.4.2.3.
    #[test]
    fn a_check_box_is_checked_when_its_state_is_not_off() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R] >>",
            &[
                (
                    5,
                    "<< /T (urgent) /FT /Btn /V /Yes /AS /Yes /Subtype /Widget /Rect [0 0 1 1] \
                     /AP << /N << /Yes 10 0 R /Off 11 0 R >> >> >>",
                ),
                (
                    6,
                    "<< /T (done) /FT /Btn /V /Off /AS /Off /Subtype /Widget /Rect [0 0 1 1] >>",
                ),
                (7, "<< /T (blank) /FT /Btn >>"),
                (8, "<< /T (card) /FT /Btn /Ff 32768 /V /a >>"),
                (9, "<< /T (name) /FT /Tx /V (Yes) >>"),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].checked(), Some(true));
        assert_eq!(
            fields[0].widgets,
            vec![Widget {
                object: r(5),
                appearance_state: Some("Yes".into()),
                on_state: Some("Yes".into()),
                characteristics: None,
            }]
        );
        assert_eq!(fields[0].on_widgets(), vec![0]);
        assert_eq!(fields[1].checked(), Some(false));
        assert_eq!(fields[1].widgets[0].on_state, None);
        assert_eq!(fields[1].on_widgets(), Vec::<usize>::new());
        assert_eq!(fields[2].checked(), Some(false));
        assert_eq!(fields[3].checked(), None);
        assert_eq!(fields[4].checked(), None);
    }

    /// A check box field with two widgets and `/Opt` (Table 227): each
    /// widget's on state is its position in `/Kids` as a name, the field's
    /// state picks the widget that is on, and that index picks the export
    /// value; a widget whose `/N` is one stream or names two on states has
    /// no on state.
    // Covers ISO 32000-1 §12.7.4.2.3.
    #[test]
    fn check_box_widgets_are_matched_to_the_state_and_their_export_values() {
        let fields = doc_with_stream(
            "/AcroForm << /Fields [5 0 R 9 0 R] >>",
            &[
                (
                    5,
                    "<< /T (size) /FT /Btn /Opt [(small) (large)] /V /1 /Kids [6 0 R 7 0 R] >>",
                ),
                (
                    6,
                    "<< /Subtype /Widget /Parent 5 0 R /AS /Off /Rect [0 0 1 1] \
                     /AP << /N << /0 10 0 R /Off 10 0 R >> >> >>",
                ),
                (
                    7,
                    "<< /Subtype /Widget /Parent 5 0 R /AS /1 /Rect [0 0 1 1] \
                     /AP 8 0 R >>",
                ),
                (8, "<< /N << /1 10 0 R /Off 10 0 R >> >>"),
                (9, "<< /T (odd) /FT /Btn /V /Yes /Kids [11 0 R 12 0 R] >>"),
                (
                    11,
                    "<< /Subtype /Widget /Parent 9 0 R /AP << /N 10 0 R >> /Rect [0 0 1 1] >>",
                ),
                (
                    12,
                    "<< /Subtype /Widget /Parent 9 0 R /Rect [0 0 1 1] \
                     /AP << /N << /Yes 10 0 R /Also 10 0 R /Off 10 0 R >> >> >>",
                ),
            ],
            (10, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b""),
        )
        .form_fields();
        let size = &fields[0];
        assert_eq!(size.checked(), Some(true));
        assert_eq!(size.on_widgets(), vec![1]);
        assert_eq!(size.widgets[0].on_state.as_deref(), Some("0"));
        assert_eq!(size.widgets[1].on_state.as_deref(), Some("1"));
        assert_eq!(size.widgets[1].appearance_state.as_deref(), Some("1"));
        assert_eq!(size.options[1].export_value, "large");
        let odd = &fields[1];
        assert_eq!(odd.widgets[0].on_state, None);
        assert_eq!(odd.widgets[1].on_state, None);
        assert_eq!(odd.on_widgets(), Vec::<usize>::new());
        assert_eq!(odd.checked(), Some(true));
    }

    /// The standard's radio button example: the parent's `/V` names the
    /// on state of the button that is on, each kid widget's on state comes
    /// from its own `/AP /N`, and the on widget's index picks its Table
    /// 227 export value; a radio field is no check box.
    // Covers ISO 32000-1 §12.7.4.2.4.
    #[test]
    fn a_radio_button_field_names_the_widget_that_is_on() {
        let fields = doc_with_stream(
            "/AcroForm << /Fields [10 0 R] >>",
            &[
                (
                    10,
                    "<< /FT /Btn /Ff 32768 /T (Credit card) /V /cardbrand1 /Opt [(Visa) (Master)] \
                     /Kids [11 0 R 12 0 R] >>",
                ),
                (
                    11,
                    "<< /Parent 10 0 R /Subtype /Widget /AS /cardbrand1 /Rect [0 0 1 1] \
                     /AP << /N << /cardbrand1 8 0 R /Off 8 0 R >> >> >>",
                ),
                (
                    12,
                    "<< /Parent 10 0 R /Subtype /Widget /AS /Off /Rect [0 0 1 1] \
                     /AP << /N << /cardbrand2 8 0 R /Off 8 0 R >> >> >>",
                ),
            ],
            (8, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b""),
        )
        .form_fields();
        let card = &fields[0];
        assert_eq!(card.button_kind(), Some(ButtonKind::RadioButtons));
        assert_eq!(card.state(), Some("cardbrand1"));
        assert_eq!(card.on_widgets(), vec![0]);
        assert_eq!(card.widgets[0].on_state.as_deref(), Some("cardbrand1"));
        assert_eq!(card.widgets[1].on_state.as_deref(), Some("cardbrand2"));
        assert_eq!(card.widgets[1].appearance_state.as_deref(), Some("Off"));
        assert_eq!(card.options[0].export_value, "Visa");
        assert_eq!(card.checked(), None);
    }

    /// With `RadiosInUnison` two buttons sharing an on state are on
    /// together; a radio field without `/V` is in the `Off` state with no
    /// button on; `NoToggleToOff` is read.
    // Covers ISO 32000-1 §12.7.4.2.4.
    #[test]
    fn radios_in_unison_are_on_together_and_a_valueless_radio_field_is_off() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 9 0 R] >>",
            &[
                (5, "<< /T (size) /FT /Btn /Ff 33587200 /V /a /Kids [6 0 R 7 0 R 8 0 R] >>"),
                (6, "<< /Parent 5 0 R /Subtype /Widget /AS /a /AP << /N << /a 6 0 R /Off 6 0 R >> >> >>"),
                (7, "<< /Parent 5 0 R /Subtype /Widget /AS /a /AP << /N << /a 6 0 R /Off 6 0 R >> >> >>"),
                (8, "<< /Parent 5 0 R /Subtype /Widget /AS /Off /AP << /N << /b 6 0 R /Off 6 0 R >> >> >>"),
                (9, "<< /T (none) /FT /Btn /Ff 49152 /Kids [10 0 R] >>"),
                (10, "<< /Parent 9 0 R /Subtype /Widget /AS /Off /AP << /N << /x 6 0 R /Off 6 0 R >> >> >>"),
            ],
        )
        .form_fields();
        let size = &fields[0];
        assert!(size.flags.radio() && size.flags.radios_in_unison());
        assert_eq!(size.on_widgets(), vec![0, 1]);
        let none = &fields[1];
        assert!(none.flags.radio() && none.flags.no_toggle_to_off());
        assert_eq!(none.state(), Some("Off"));
        assert_eq!(none.on_widgets(), Vec::<usize>::new());
    }

    /// A merged pushbutton with every caption and icon entry of Table 189:
    /// the captions are text strings, the icons references, the caption
    /// position a code.
    // Covers ISO 32000-1 §12.7.4.2.2.
    #[test]
    fn a_pushbutton_s_widget_carries_its_captions_and_icons() {
        let fields = doc_with_stream(
            "/AcroForm << /Fields [5 0 R] >>",
            &[
                (
                    5,
                    "<< /T (go) /FT /Btn /Ff 65536 /V /Yes /Subtype /Widget /Rect [0 0 10 10] \
                     /MK 7 0 R >>",
                ),
                (
                    7,
                    "<< /CA <FEFF00530065006E0064> /RC (Go!) /AC (Sending) /I 6 0 R /RI 6 0 R \
                     /IX 6 0 R /TP 2 /BC [0] /BG [1 1 1] >>",
                ),
            ],
            (6, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b""),
        )
        .form_fields();
        let go = &fields[0];
        assert_eq!(go.button_kind(), Some(ButtonKind::PushButton));
        assert_eq!(go.state(), None);
        assert_eq!(
            go.widgets[0].characteristics,
            Some(AppearanceCharacteristics {
                caption: Some("Send".into()),
                rollover_caption: Some("Go!".into()),
                alternate_caption: Some("Sending".into()),
                icon: Some(r(6)),
                rollover_icon: Some(r(6)),
                alternate_icon: Some(r(6)),
                caption_position: CaptionPosition::Below,
            })
        );
    }

    /// A check box may carry the normal caption alone, with the caption
    /// position at its default; a widget without `/MK` has no
    /// characteristics; a caption that is a number, an icon written
    /// directly and a `/TP` of 9 read as absent.
    // Covers ISO 32000-1 §12.7.4.2.2.
    #[test]
    fn captions_default_and_odd_characteristics_read_as_absent() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >>",
            &[
                (5, "<< /T (urgent) /FT /Btn /V /Yes /Subtype /Widget /Rect [0 0 1 1] /MK << /CA (8) >> >>"),
                (6, "<< /T (plain) /FT /Btn /Ff 65536 /Subtype /Widget /Rect [0 0 1 1] >>"),
                (
                    7,
                    "<< /T (odd) /FT /Btn /Ff 65536 /Subtype /Widget /Rect [0 0 1 1] \
                     /MK << /CA 5 /I << /Type /XObject >> /TP 9 >> >>",
                ),
            ],
        )
        .form_fields();
        assert_eq!(
            fields[0].widgets[0].characteristics,
            Some(AppearanceCharacteristics {
                caption: Some("8".into()),
                ..AppearanceCharacteristics::default()
            })
        );
        assert_eq!(fields[1].widgets[0].characteristics, None);
        assert_eq!(
            fields[2].widgets[0].characteristics,
            Some(AppearanceCharacteristics::default())
        );
        assert_eq!(
            CaptionPosition::from_int(6),
            Some(CaptionPosition::Overlaid)
        );
        assert_eq!(CaptionPosition::from_int(7), None);
    }

    /// A signed signature field: `/V` is a signature dictionary whose
    /// Table 252 entries are read as data, and the field's own `/Lock` and
    /// `/SV` are kept by reference.
    // Covers ISO 32000-1 §12.7.4.5.
    #[test]
    fn a_signed_signature_field_exposes_its_signature_dictionary() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R] /SigFlags 3 >>",
            &[
                (5, "<< /T (sig) /FT /Sig /V 6 0 R /Lock 7 0 R /SV 8 0 R /Subtype /Widget /Rect [0 0 0 0] >>"),
                (
                    6,
                    "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
                     /ByteRange [0 100 200 50] /Contents <3031> /Name <FEFF00410064> \
                     /M (D:20260909100000Z) /Location (Berlin) /Reason (I agree) \
                     /ContactInfo (ada@example.org) >>",
                ),
                (7, "<< /Type /SigFieldLock /Action /All >>"),
                (8, "<< /Type /SV /Ff 1 >>"),
            ],
        )
        .form_fields();
        let sig = &fields[0];
        assert_eq!(sig.field_type, Some(FieldType::Signature));
        assert_eq!(
            sig.signature(),
            Some(Signature {
                filter: Some("Adobe.PPKLite".into()),
                sub_filter: Some("adbe.pkcs7.detached".into()),
                byte_range: vec![(0, 100), (200, 50)],
                contents: b"01".to_vec(),
                name: Some("Ad".into()),
                signing_time: Some("D:20260909100000Z".into()),
                location: Some("Berlin".into()),
                reason: Some("I agree".into()),
                contact_info: Some("ada@example.org".into()),
            })
        );
        assert_eq!(sig.lock, Some(r(7)));
        assert_eq!(sig.seed_value, Some(r(8)));
        assert_eq!(sig.text(), None);
        assert_eq!(sig.selected(), Vec::<String>::new());
    }

    /// An unsigned signature field has no signature; a text field's
    /// dictionary value is none either; a signature dictionary with only
    /// `/Type`, an odd `/ByteRange` and a direct `/Lock` read as defaults or
    /// absent.
    // Covers ISO 32000-1 §12.7.4.5.
    #[test]
    fn unsigned_fields_and_odd_signature_entries_read_as_absent() {
        let fields = doc(
            "/AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >>",
            &[
                (5, "<< /T (unsigned) /FT /Sig /Lock << /Action /All >> >>"),
                (6, "<< /T (name) /FT /Tx /V << /Filter /X >> >>"),
                (
                    7,
                    "<< /T (odd) /FT /Sig /V << /Type /Sig /ByteRange [0 10 20] /Contents 5 >> >>",
                ),
            ],
        )
        .form_fields();
        assert_eq!(fields[0].signature(), None);
        assert_eq!(fields[0].lock, None);
        assert_eq!(fields[0].seed_value, None);
        assert_eq!(fields[1].signature(), None);
        assert_eq!(
            fields[2].signature(),
            Some(Signature {
                byte_range: vec![(0, 10)],
                ..Signature::default()
            })
        );
    }
}
