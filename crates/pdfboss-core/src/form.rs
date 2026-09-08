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
    pub widgets: Vec<ObjRef>,
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
    /// `/AA`: the field's additional-actions dictionary, as written.
    pub additional_actions: Option<Dict>,
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
    let value = entries.value("V").await.or(inherited.value);
    let default_value = entries.value("DV").await.or(inherited.default_value);
    let partial_name = text_string(&entries, "T").await;
    let name = qualified_name(&inherited.name, partial_name.as_deref());
    let additional_actions = match dict.get("AA") {
        Some(entry) => resolved_dict(src, entry).await,
        None => None,
    };
    let mut widgets = Vec::new();
    if is_widget(&dict) {
        widgets.push(object);
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
                widgets.push(kid);
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
        alternate_name: text_string(&entries, "TU").await,
        mapping_name: text_string(&entries, "TM").await,
        flags,
        value,
        default_value,
        additional_actions,
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

/// The dictionary a reference points at, `None` for anything else.
async fn field_dict<S: AsyncObjectSource>(src: &S, object: ObjRef) -> Option<Dict> {
    match src.get(object).await.ok()? {
        Object::Dict(dict) => Some(dict),
        _ => None,
    }
}

/// Whether a dictionary is a widget annotation (`/Subtype /Widget`).
fn is_widget(dict: &Dict) -> bool {
    dict.get_name("Subtype")
        .is_some_and(|subtype| subtype.0 == "Widget")
}

/// The text string under `key`, decoded (§7.9.2.2); `None` when absent or
/// not a string.
async fn text_string<S: AsyncObjectSource>(entries: &Entries<'_, S>, key: &str) -> Option<String> {
    Some(decode_text_string(
        entries.value(key).await?.as_str_bytes()?,
    ))
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
    use super::{FieldFlags, FieldType, InteractiveForm, Quadding, SignatureFlags};
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
        assert_eq!(field.widgets, vec![r(5)]);
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
        assert_eq!(fields[0].widgets, vec![r(6), r(7)]);
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
        assert_eq!(root.widgets, Vec::new());
        let first = &fields[1];
        assert_eq!(first.parent, Some(r(5)));
        assert_eq!(first.partial_name.as_deref(), Some("first"));
        assert_eq!(first.field_type, Some(FieldType::Text));
        assert_eq!(first.flags, FieldFlags(1));
        assert_eq!(first.value, Some(Object::String(b"from parent".to_vec())));
        assert_eq!(first.default_value, Some(Object::String(b"reset".to_vec())));
        assert_eq!(first.widgets, vec![r(6)]);
        let last = &fields[2];
        assert_eq!(last.flags, FieldFlags(2));
        assert_eq!(last.value, Some(Object::String(b"own".to_vec())));
        assert_eq!(last.default_value, Some(Object::String(b"reset".to_vec())));
        assert_eq!(last.widgets, vec![r(8)]);
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
}
