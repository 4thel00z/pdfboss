//! Optional-content visibility (ISO 32000-1 §8.11): which optional content
//! groups the document's default configuration and its usage application
//! dictionaries turn off, whether content gated by an `/OC` entry or a
//! `BDC /OC` span is visible, and the groups themselves read as data.

use std::sync::Arc;

use crate::hash::FastSet;
use crate::object::{decode_text_string, Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::{resolved_dict, Entries};

/// Maximum `/VE` visibility-expression nesting depth. Real expressions are
/// one or two levels deep; past the cap the expression reads as malformed,
/// and malformed means visible.
const MAX_VE_DEPTH: u32 = 8;

/// The intent every group and configuration has when it declares none.
const DEFAULT_INTENT: &str = "View";

/// A usage application event (ISO 32000-1 §8.11.4.4, Table 103): the
/// situation a configuration's `/AS` usage application dictionaries apply
/// under. Each event is also the usage category it reads from a group's
/// `/Usage` dictionary: `/View /ViewState`, `/Print /PrintState` or
/// `/Export /ExportState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OcEvent {
    /// The document is opened on screen.
    View,
    /// The document is printed.
    Print,
    /// The document is exported to a format without optional content.
    Export,
}

impl OcEvent {
    /// The event an `/Event` or `/Category` name denotes; `None` for the
    /// Zoom, Language and User categories and for any other name.
    pub fn from_name(name: &str) -> Option<OcEvent> {
        match name {
            "View" => Some(OcEvent::View),
            "Print" => Some(OcEvent::Print),
            "Export" => Some(OcEvent::Export),
            _ => None,
        }
    }

    /// The name as written in the file, also the key of the matching
    /// `/Usage` entry.
    pub fn as_name(self) -> &'static str {
        match self {
            OcEvent::View => "View",
            OcEvent::Print => "Print",
            OcEvent::Export => "Export",
        }
    }

    /// The key of the ON or OFF name inside the matching `/Usage` entry.
    fn state_key(self) -> &'static str {
        match self {
            OcEvent::View => "ViewState",
            OcEvent::Print => "PrintState",
            OcEvent::Export => "ExportState",
        }
    }
}

/// The document's optional-content visibility under its default
/// configuration (`/OCProperties` `/D`, ISO 32000-1 §8.11.4.3) and one
/// usage application event: the set of groups that state turns off. A
/// group's identity is its indirect reference — groups are shared by
/// reference between the configuration, marked-content properties, and
/// `/OC` entries (§8.11.2.1).
///
/// Everything here is lenient: an entry that is missing, malformed, or will
/// not resolve leaves content visible, never hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcState {
    off: FastSet<ObjRef>,
}

impl OcState {
    /// The state a viewer shows on screen: [`OcState::load_for_with`] under
    /// the [`OcEvent::View`] event, which is what pdfium and pdf.js render
    /// by default.
    ///
    /// Covers ISO 32000-1 §8.11.4.5.
    pub async fn load_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<OcState> {
        OcState::load_for_with(src, trailer, Some(OcEvent::View)).await
    }

    /// Builds the state from the catalog's `/OCProperties`, or `None` when
    /// the document declares none — no optional content, everything
    /// visible. The default `/D` configuration is applied in specification
    /// order: `/BaseState` (default `ON`), then `/ON`, then `/OFF`, so a
    /// group named in both `/ON` and `/OFF` ends up off. Then the
    /// configuration's `/AS` usage application dictionaries whose `/Event`
    /// is `event` adjust the groups they name from the groups' `/Usage`
    /// dictionaries (§8.11.4.4). `None` applies no usage application: the
    /// default configuration alone, which §8.11.4.5 prescribes for printing
    /// and aggregating applications. Last, a group whose `/Intent` shares no
    /// name with the configuration's `/Intent` has no effect on visibility
    /// (§8.11.2.3) and leaves the off set.
    ///
    /// Covers ISO 32000-1 §7.7.2, §8.11.2.1, §8.11.2.3, §8.11.4.2,
    /// §8.11.4.3, §8.11.4.4 and §8.11.4.5.
    pub async fn load_for_with<S: AsyncObjectSource>(
        src: &S,
        trailer: &Dict,
        event: Option<OcEvent>,
    ) -> Option<OcState> {
        let props = properties(src, trailer).await?;
        Some(OcState::from_properties(src, &props, event).await)
    }

    /// [`OcState::load_for_with`] over an already resolved `/OCProperties`
    /// dictionary.
    async fn from_properties<S: AsyncObjectSource>(
        src: &S,
        props: &Dict,
        event: Option<OcEvent>,
    ) -> OcState {
        let config = match props.get("D") {
            Some(o) => resolved_dict(src, o).await,
            None => None,
        };
        let mut off = configured_off(src, props, config.as_ref()).await;
        if let (Some(config), Some(event)) = (config.as_ref(), event) {
            apply_usage(src, config, event, &mut off).await;
        }
        retain_matching_intents(src, config.as_ref(), &mut off).await;
        OcState { off }
    }

    /// Whether the configuration turns `group` off.
    ///
    /// Covers ISO 32000-1 §8.11.2.1 and §8.11.4.5.
    pub fn hidden(&self, group: ObjRef) -> bool {
        self.off.contains(&group)
    }

    /// Whether content gated by `value` — the operand of a stream or
    /// annotation dictionary's `/OC` entry — is visible: a group reference
    /// is visible unless the group is off; a membership dictionary follows
    /// its `/VE` visibility expression, else its `/P` policy over `/OCGs`.
    /// A direct group dictionary has no
    /// reference identity to be turned off by, and anything malformed is
    /// left visible.
    pub async fn visible_with<S: AsyncObjectSource>(&self, src: &S, value: &Object) -> bool {
        match value {
            Object::Ref(r) => {
                let Ok(resolved) = src.resolve(value).await else {
                    return true;
                };
                let Some(dict) = resolved.as_dict() else {
                    return true;
                };
                if is_ocmd(dict) {
                    return self.ocmd_visible(src, dict).await;
                }
                !self.hidden(*r)
            }
            Object::Dict(dict) => {
                if is_ocmd(dict) {
                    return self.ocmd_visible(src, dict).await;
                }
                true
            }
            _ => true,
        }
    }

    /// Whether a `BDC /OC` span is visible: `props` is the operator's
    /// properties operand — an inline dictionary, or a name looked up in
    /// the resource chain's `/Properties` category. The lookup keeps the
    /// value unresolved, because a group's on/off identity is its indirect
    /// reference; resolving first would read every named group as visible.
    pub async fn props_visible_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        chain: &[Arc<Dict>],
        props: &Object,
    ) -> bool {
        let named;
        let value = match props {
            Object::Name(name) => {
                named = properties_value(src, chain, &name.0).await;
                match &named {
                    Some(value) => value,
                    None => return true,
                }
            }
            other => other,
        };
        self.visible_with(src, value).await
    }

    /// A membership dictionary's visibility (§8.11.2.2): the `/VE`
    /// visibility expression when present (taking precedence, malformed
    /// reading as visible), else the `/OCGs` groups under the `/P` policy —
    /// `AnyOn` (the default, and the reading of an unrecognized policy),
    /// `AllOn`, `AnyOff`, or `AllOff`. No usable groups means visible.
    ///
    /// Covers ISO 32000-1 §8.11.2.2.
    async fn ocmd_visible<S: AsyncObjectSource>(&self, src: &S, dict: &Dict) -> bool {
        if let Some(ve) = dict.get("VE") {
            let Ok(Object::Array(expr)) = src.resolve(ve).await else {
                return true;
            };
            return self.expression_visible(src, expr).await.unwrap_or(true);
        }
        let groups: Vec<ObjRef> = match dict.get("OCGs") {
            None => return true,
            Some(indirect @ Object::Ref(r)) => match src.resolve(indirect).await {
                Ok(Object::Array(items)) => items.iter().filter_map(Object::as_ref).collect(),
                Ok(Object::Dict(_)) => vec![*r],
                _ => Vec::new(),
            },
            Some(Object::Array(items)) => items.iter().filter_map(Object::as_ref).collect(),
            Some(_) => Vec::new(),
        };
        if groups.is_empty() {
            return true;
        }
        let policy = match dict.get("P") {
            Some(o) => src
                .resolve(o)
                .await
                .ok()
                .and_then(|o| o.as_name().map(|n| n.0.clone())),
            None => None,
        };
        match policy.as_deref() {
            Some("AllOn") => groups.iter().all(|g| !self.hidden(*g)),
            Some("AnyOff") => groups.iter().any(|g| self.hidden(*g)),
            Some("AllOff") => groups.iter().all(|g| self.hidden(*g)),
            _ => groups.iter().any(|g| !self.hidden(*g)),
        }
    }

    /// Evaluates a `/VE` array (§8.11.2.2): `[/And|/Or|/Not operands…]`,
    /// each operand a group reference or a nested expression (directly, or
    /// behind a reference). `None` is malformed — an unknown operator, no
    /// operands, `/Not` with more than one, an operand that is neither
    /// group nor expression, or nesting past [`MAX_VE_DEPTH`] — and reads
    /// as visible at the caller.
    ///
    /// An explicit work stack rather than recursion: a recursive `async fn`
    /// must box itself, and a `Send`-boxed future would demand `S: Sync`,
    /// which the synchronous `Immediate` source cannot supply — the same
    /// shape as the content executors' frame stacks.
    ///
    /// Covers ISO 32000-1 §8.11.2.2.
    async fn expression_visible<S: AsyncObjectSource>(
        &self,
        src: &S,
        expr: Vec<Object>,
    ) -> Option<bool> {
        let mut stack = vec![VeFrame::new(src, expr).await?];
        loop {
            let top = stack.len() - 1;
            let Some(operand) = stack[top].operands.get(stack[top].next).cloned() else {
                let done = stack.pop()?;
                let value = match done.operator.as_str() {
                    "And" => done.all,
                    "Or" => done.any,
                    _ => !done.any,
                };
                let Some(parent) = stack.last_mut() else {
                    return Some(value);
                };
                parent.fold(value);
                continue;
            };
            stack[top].next += 1;
            let value = match operand {
                Object::Array(items) => {
                    if stack.len() > MAX_VE_DEPTH as usize {
                        return None;
                    }
                    stack.push(VeFrame::new(src, items).await?);
                    continue;
                }
                Object::Ref(group) => match src.resolve(&operand).await.ok()? {
                    Object::Array(items) => {
                        if stack.len() > MAX_VE_DEPTH as usize {
                            return None;
                        }
                        stack.push(VeFrame::new(src, items).await?);
                        continue;
                    }
                    Object::Dict(_) => !self.hidden(group),
                    _ => return None,
                },
                _ => return None,
            };
            stack[top].fold(value);
        }
    }
}

/// One suspended `/VE` subexpression: its operator, its operands, how far
/// evaluation has got, and the conjunction/disjunction accumulated so far.
struct VeFrame {
    operator: String,
    operands: Vec<Object>,
    next: usize,
    all: bool,
    any: bool,
}

impl VeFrame {
    /// Validates and frames one expression array; `None` is malformed.
    async fn new<S: AsyncObjectSource>(src: &S, mut expr: Vec<Object>) -> Option<VeFrame> {
        let operator = match expr.first()? {
            Object::Name(n) => n.0.clone(),
            other => src.resolve(other).await.ok()?.as_name()?.0.clone(),
        };
        if !matches!(operator.as_str(), "And" | "Or" | "Not") {
            return None;
        }
        let operands = expr.split_off(1);
        if operands.is_empty() || (operator == "Not" && operands.len() != 1) {
            return None;
        }
        Some(VeFrame {
            operator,
            operands,
            next: 0,
            all: true,
            any: false,
        })
    }

    /// Accumulates one operand's value.
    fn fold(&mut self, value: bool) {
        self.all &= value;
        self.any |= value;
    }
}

/// An optional content group (ISO 32000-1 §8.11.2.1, Table 98) read as
/// data, with its state under the configuration and event the reader was
/// asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct OcGroup {
    /// The group's indirect reference: its identity in `/OC` entries,
    /// `/OCGs` arrays and the configuration.
    pub reference: ObjRef,
    /// `/Name`, the group's name for a user interface. Required by the
    /// table, `None` when missing.
    pub name: Option<String>,
    /// `/Intent`: the intent names, `View` and `Design` being the defined
    /// ones; the default `View` when absent.
    pub intent: Vec<String>,
    /// `/Usage` (Table 102), every field empty when the entry is absent.
    pub usage: OcUsage,
    /// Whether the group is on under the state the reader built: the
    /// default configuration, the usage application dictionaries for the
    /// requested event, and the intent rule.
    pub visible: bool,
}

/// An optional content usage dictionary (ISO 32000-1 §8.11.4.4, Table 102):
/// what the group's content is for. Every field is `None`, `false` or empty
/// when its entry is absent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OcUsage {
    /// `/View /ViewState`: whether the group should be on when the document
    /// is opened on screen.
    pub view: Option<bool>,
    /// `/Print /PrintState`: whether the group should be on when printed.
    pub print: Option<bool>,
    /// `/Print /Subtype`: the kind of print content, such as `Trapping`,
    /// `PrintersMarks` or `Watermark`.
    pub print_subtype: Option<String>,
    /// `/Export /ExportState`: whether the group should be on when exported
    /// to a format without optional content.
    pub export: Option<bool>,
    /// `/Zoom /min`: the magnification the group is on from; the table's
    /// default is 0.
    pub zoom_min: Option<f64>,
    /// `/Zoom /max`: the magnification below which the group is on; the
    /// table's default is infinity.
    pub zoom_max: Option<f64>,
    /// `/Language /Lang`: the content's language tag, such as `es-MX`.
    pub language: Option<String>,
    /// `/Language /Preferred`: whether the group is preferred on a partial
    /// language match; false by default.
    pub language_preferred: bool,
    /// `/PageElement /Subtype`: `HF` (header or footer), `FG` (foreground),
    /// `BG` (background) or `L` (logo).
    pub page_element: Option<String>,
    /// `/CreatorInfo /Creator`: the application that created the group.
    pub creator: Option<String>,
    /// `/CreatorInfo /Subtype`: the kind of content, such as `Artwork` or
    /// `Technical`.
    pub creator_subtype: Option<String>,
    /// `/User /Type`: `Ind` (individual), `Ttl` (title) or `Org`
    /// (organization).
    pub user_type: Option<String>,
    /// `/User /Name`: the names, one string or an array of strings.
    pub user_names: Vec<String>,
}

/// The document's optional content groups in `/OCProperties /OCGs` order,
/// each with its state under `event` (see [`OcState::load_for_with`]);
/// empty without `/OCProperties`. Entries that are not references to
/// dictionaries are skipped.
///
/// Covers ISO 32000-1 §8.11.2.1, §8.11.2.3, §8.11.4.2 and §8.11.4.4.
pub async fn optional_content_groups_with<S: AsyncObjectSource>(
    src: &S,
    trailer: &Dict,
    event: Option<OcEvent>,
) -> Vec<OcGroup> {
    let Some(props) = properties(src, trailer).await else {
        return Vec::new();
    };
    let state = OcState::from_properties(src, &props, event).await;
    let mut groups = Vec::new();
    for reference in group_refs(src, props.get("OCGs")).await {
        let Some(dict) = resolved_dict(src, &Object::Ref(reference)).await else {
            continue;
        };
        let usage = match sub_dict(src, &dict, "Usage").await {
            Some(usage) => read_usage(src, &usage).await,
            None => OcUsage::default(),
        };
        groups.push(OcGroup {
            reference,
            name: Entries { src, dict: &dict }.text("Name").await,
            intent: intent_names(src, &dict).await,
            usage,
            visible: !state.hidden(reference),
        });
    }
    groups
}

/// The catalog's `/OCProperties` dictionary (§8.11.4.2).
async fn properties<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<Dict> {
    let catalog = resolved_dict(src, trailer.get("Root")?).await?;
    resolved_dict(src, catalog.get("OCProperties")?).await
}

/// The off set the default configuration alone prescribes (§8.11.4.3;
/// §8.11.4.5 steps a and b): `/BaseState`, then `/ON`, then `/OFF`.
async fn configured_off<S: AsyncObjectSource>(
    src: &S,
    props: &Dict,
    config: Option<&Dict>,
) -> FastSet<ObjRef> {
    let mut off: FastSet<ObjRef> = FastSet::default();
    let Some(config) = config else {
        return off;
    };
    let base = Entries { src, dict: config }.value("BaseState").await;
    if on_off(base.as_ref()) == Some(false) {
        off.extend(group_refs(src, props.get("OCGs")).await);
    }
    for group in group_refs(src, config.get("ON")).await {
        off.remove(&group);
    }
    off.extend(group_refs(src, config.get("OFF")).await);
    off
}

/// Applies the usage application dictionaries of the configuration's `/AS`
/// array whose `/Event` is `event`, in array order, later ones winning
/// (§8.11.4.4, Table 103; §8.11.4.5). For each group in a dictionary's
/// `/OCGs` and each name in its `/Category` that is View, Print or Export,
/// the group's `/Usage` entry of that category sets the state: ON turns the
/// group on, OFF off, and a missing or malformed entry leaves it unchanged.
/// The Zoom, Language and User categories depend on a viewer's
/// magnification, locale and user, which this library has none of, and
/// leave the state unchanged.
async fn apply_usage<S: AsyncObjectSource>(
    src: &S,
    config: &Dict,
    event: OcEvent,
    off: &mut FastSet<ObjRef>,
) {
    let Some(apps) = config.get("AS") else {
        return;
    };
    let Ok(Object::Array(apps)) = src.resolve(apps).await else {
        return;
    };
    for app in &apps {
        let Some(app) = resolved_dict(src, app).await else {
            continue;
        };
        let entries = Entries { src, dict: &app };
        if entries.named("Event", OcEvent::from_name).await != Some(event) {
            continue;
        }
        let categories: Vec<OcEvent> = names(src, app.get("Category"))
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|name| OcEvent::from_name(name))
            .collect();
        if categories.is_empty() {
            continue;
        }
        for group in group_refs(src, app.get("OCGs")).await {
            let Some(dict) = resolved_dict(src, &Object::Ref(group)).await else {
                continue;
            };
            let Some(usage) = sub_dict(src, &dict, "Usage").await else {
                continue;
            };
            for category in &categories {
                match usage_state(src, &usage, *category).await {
                    Some(true) => {
                        off.remove(&group);
                    }
                    Some(false) => {
                        off.insert(group);
                    }
                    None => {}
                }
            }
        }
    }
}

/// Keeps in `off` only the groups whose `/Intent` shares a name with the
/// configuration's `/Intent` (§8.11.2.3): a group whose intent does not
/// match has no effect on visibility, so it can never hide content. Both
/// default to View; a configuration naming `All` matches every group; an
/// empty configuration array matches none, so nothing is hidden.
async fn retain_matching_intents<S: AsyncObjectSource>(
    src: &S,
    config: Option<&Dict>,
    off: &mut FastSet<ObjRef>,
) {
    if off.is_empty() {
        return;
    }
    let config_intents = match config {
        Some(config) => intent_names(src, config).await,
        None => vec![DEFAULT_INTENT.to_string()],
    };
    if config_intents.iter().any(|name| name == "All") {
        return;
    }
    let groups: Vec<ObjRef> = off.iter().copied().collect();
    for group in groups {
        let group_intents = match resolved_dict(src, &Object::Ref(group)).await {
            Some(dict) => intent_names(src, &dict).await,
            None => vec![DEFAULT_INTENT.to_string()],
        };
        if group_intents
            .iter()
            .any(|name| config_intents.contains(name))
        {
            continue;
        }
        off.remove(&group);
    }
}

/// The names a dictionary's `/Intent` entry carries, a single name or an
/// array of names; absent or malformed reads as the default View.
async fn intent_names<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Vec<String> {
    names(src, dict.get("Intent"))
        .await
        .unwrap_or_else(|| vec![DEFAULT_INTENT.to_string()])
}

/// The names in a value that is one name or an array of names, resolved;
/// `None` when absent or neither.
async fn names<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Option<Vec<String>> {
    match src.resolve(value?).await.ok()? {
        Object::Name(name) => Some(vec![name.0]),
        Object::Array(items) => Some(
            items
                .iter()
                .filter_map(|item| item.as_name().map(|name| name.0.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// The dictionary under `key`, resolved; `None` when absent or not one.
async fn sub_dict<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<Dict> {
    resolved_dict(src, dict.get(key)?).await
}

/// The state a usage dictionary's `category` entry names: `Some(true)` for
/// ON, `Some(false)` for OFF, `None` when absent or neither.
async fn usage_state<S: AsyncObjectSource>(
    src: &S,
    usage: &Dict,
    category: OcEvent,
) -> Option<bool> {
    let entry = sub_dict(src, usage, category.as_name()).await?;
    on_off(
        Entries { src, dict: &entry }
            .value(category.state_key())
            .await
            .as_ref(),
    )
}

/// `Some(true)` for the name ON, `Some(false)` for OFF, `None` otherwise.
fn on_off(value: Option<&Object>) -> Option<bool> {
    match value?.as_name()?.0.as_str() {
        "ON" => Some(true),
        "OFF" => Some(false),
        _ => None,
    }
}

/// A usage dictionary's entries (Table 102) as data.
async fn read_usage<S: AsyncObjectSource>(src: &S, usage: &Dict) -> OcUsage {
    let mut out = OcUsage {
        view: usage_state(src, usage, OcEvent::View).await,
        print: usage_state(src, usage, OcEvent::Print).await,
        export: usage_state(src, usage, OcEvent::Export).await,
        ..OcUsage::default()
    };
    if let Some(print) = sub_dict(src, usage, "Print").await {
        out.print_subtype = name_entry(src, &print, "Subtype").await;
    }
    if let Some(zoom) = sub_dict(src, usage, "Zoom").await {
        let entries = Entries { src, dict: &zoom };
        out.zoom_min = entries.value("min").await.and_then(|o| o.as_f64());
        out.zoom_max = entries.value("max").await.and_then(|o| o.as_f64());
    }
    if let Some(language) = sub_dict(src, usage, "Language").await {
        let entries = Entries {
            src,
            dict: &language,
        };
        out.language = entries.text("Lang").await;
        out.language_preferred = on_off(entries.value("Preferred").await.as_ref()) == Some(true);
    }
    if let Some(element) = sub_dict(src, usage, "PageElement").await {
        out.page_element = name_entry(src, &element, "Subtype").await;
    }
    if let Some(creator) = sub_dict(src, usage, "CreatorInfo").await {
        let entries = Entries {
            src,
            dict: &creator,
        };
        out.creator = entries.text("Creator").await;
        out.creator_subtype = name_entry(src, &creator, "Subtype").await;
    }
    if let Some(user) = sub_dict(src, usage, "User").await {
        let entries = Entries { src, dict: &user };
        out.user_type = name_entry(src, &user, "Type").await;
        out.user_names = match entries.value("Name").await {
            Some(Object::Array(items)) => items
                .iter()
                .filter_map(|item| item.as_str_bytes().map(decode_text_string))
                .collect(),
            Some(single) => single
                .as_str_bytes()
                .map(decode_text_string)
                .into_iter()
                .collect(),
            None => Vec::new(),
        };
    }
    out
}

/// The name under `key` as a string, resolved; `None` when absent or not a
/// name.
async fn name_entry<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<String> {
    Entries { src, dict }
        .value(key)
        .await
        .and_then(|o| o.as_name().map(|name| name.0.clone()))
}

/// Whether a dictionary reached through an `/OC`-shaped value is a
/// membership dictionary rather than a group: `/Type /OCMD` says so, and a
/// dictionary with no `/Type` carrying `/OCGs` or `/VE` is read as one too —
/// a group never carries those keys, and files omit `/Type`.
fn is_ocmd(dict: &Dict) -> bool {
    match dict.get_name("Type") {
        Some(n) => n.0 == "OCMD",
        None => dict.get("OCGs").is_some() || dict.get("VE").is_some(),
    }
}

/// The group references in a (possibly indirect) array. Null entries and
/// non-reference values are ignored (§8.11.2.2); a value that is not an
/// array yields nothing.
async fn group_refs<S: AsyncObjectSource>(src: &S, value: Option<&Object>) -> Vec<ObjRef> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Ok(Object::Array(items)) = src.resolve(value).await else {
        return Vec::new();
    };
    items.iter().filter_map(Object::as_ref).collect()
}

/// The raw `/Properties` resource value for `name`, innermost dictionary
/// first — deliberately unresolved, so a reference keeps the identity the
/// off set is keyed by.
///
/// Covers ISO 32000-1 §14.6.2.
async fn properties_value<S: AsyncObjectSource>(
    src: &S,
    chain: &[Arc<Dict>],
    name: &str,
) -> Option<Object> {
    for res in chain {
        let Some(cat) = res.get("Properties") else {
            continue;
        };
        let Ok(Object::Dict(dict)) = src.resolve(cat).await else {
            continue;
        };
        let Some(value) = dict.get(name) else {
            continue;
        };
        return Some(value.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block_on, Document, Immediate, Name};
    use pdfboss_testkit::PdfBuilder;

    fn gref(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    /// A document whose catalog carries `oc_props` as `/OCProperties`
    /// (skipped entirely when empty) and whose objects 10 and 11 are two
    /// groups; `extra` adds more objects (membership dictionaries etc.).
    fn doc_with_oc(oc_props: &str, extra: impl FnOnce(&mut PdfBuilder)) -> Document {
        let mut b = PdfBuilder::new();
        let oc = if oc_props.is_empty() {
            String::new()
        } else {
            format!(" /OCProperties {oc_props}")
        };
        b.object(1, &format!("<< /Type /Catalog /Pages 2 0 R{oc} >>"));
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        b.object(10, "<< /Type /OCG /Name (one) >>");
        b.object(11, "<< /Type /OCG /Name (two) >>");
        extra(&mut b);
        Document::load(b.build(1)).expect("load")
    }

    fn visible(doc: &Document, state: &OcState, value: &Object) -> bool {
        block_on(state.visible_with(&Immediate(doc), value))
    }

    fn state_for(doc: &Document, event: Option<OcEvent>) -> OcState {
        doc.oc_state_for(event).expect("state")
    }

    // Covers ISO 32000-1 §8.11.4.2.
    #[test]
    fn no_ocproperties_means_no_state() {
        let doc = doc_with_oc("", |_| {});
        assert_eq!(doc.oc_state(), None);
    }

    // Covers ISO 32000-1 §8.11.2.1, §8.11.4.3 and §8.11.4.5.
    #[test]
    fn base_state_defaults_on_and_off_hides() {
        let doc = doc_with_oc("<< /OCGs [10 0 R 11 0 R] /D << /OFF [11 0 R] >> >>", |_| {});
        let state = doc.oc_state().expect("state");
        assert!(!state.hidden(gref(10)));
        assert!(state.hidden(gref(11)));
    }

    // Covers ISO 32000-1 §8.11.4.3.
    #[test]
    fn base_state_off_hides_all_but_on() {
        let doc = doc_with_oc(
            "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /OFF /ON [10 0 R] >> >>",
            |_| {},
        );
        let state = doc.oc_state().expect("state");
        assert!(!state.hidden(gref(10)));
        assert!(state.hidden(gref(11)));
    }

    /// §8.11.4.3 applies `/BaseState`, then `/ON`, then `/OFF`: a group
    /// named in both lists ends up off.
    // Covers ISO 32000-1 §8.11.4.3 and §8.11.4.5.
    #[test]
    fn off_is_applied_after_on() {
        let doc = doc_with_oc(
            "<< /OCGs [10 0 R] /D << /ON [10 0 R] /OFF [10 0 R] >> >>",
            |_| {},
        );
        let state = doc.oc_state().expect("state");
        assert!(state.hidden(gref(10)));
    }

    /// Indirection at every level: the configuration, its arrays, and the
    /// base state name all resolve through references.
    // Covers ISO 32000-1 §8.11.4.2.
    #[test]
    fn configuration_resolves_indirection() {
        let doc = doc_with_oc("<< /OCGs 20 0 R /D 21 0 R >>", |b| {
            b.object(20, "[10 0 R 11 0 R]");
            b.object(21, "<< /BaseState 22 0 R /ON 23 0 R >>");
            b.object(22, "/OFF");
            b.object(23, "[10 0 R]");
        });
        let state = doc.oc_state().expect("state");
        assert!(!state.hidden(gref(10)));
        assert!(state.hidden(gref(11)));
    }

    /// Group 10 is on, group 11 off, for every membership test below.
    fn split_state() -> (Document, OcState) {
        let doc = doc_with_oc("<< /OCGs [10 0 R 11 0 R] /D << /OFF [11 0 R] >> >>", |b| {
            b.object(30, "<< /Type /OCMD /OCGs [10 0 R 11 0 R] >>");
            b.object(31, "<< /Type /OCMD /OCGs [10 0 R 11 0 R] /P /AllOn >>");
            b.object(32, "<< /Type /OCMD /OCGs [10 0 R 11 0 R] /P /AnyOff >>");
            b.object(33, "<< /Type /OCMD /OCGs [10 0 R 11 0 R] /P /AllOff >>");
            b.object(34, "<< /Type /OCMD /OCGs [11 0 R] >>");
            b.object(35, "<< /Type /OCMD /OCGs [] /P /AllOff >>");
            b.object(36, "<< /Type /OCMD /OCGs [null null] /P /AllOff >>");
            b.object(37, "<< /Type /OCMD /OCGs 11 0 R /P /AnyOn >>");
            b.object(38, "<< /Type /OCMD /OCGs [11 0 R] /P /Bogus >>");
        });
        let state = doc.oc_state().expect("state");
        (doc, state)
    }

    // Covers ISO 32000-1 §8.11.2.1.
    #[test]
    fn group_reference_visibility_follows_the_off_set() {
        let (doc, state) = split_state();
        assert!(visible(&doc, &state, &Object::Ref(gref(10))));
        assert!(!visible(&doc, &state, &Object::Ref(gref(11))));
    }

    // Covers ISO 32000-1 §8.11.2.2.
    #[test]
    fn membership_policies_follow_the_specification() {
        let (doc, state) = split_state();
        let ocmd = |num| Object::Ref(gref(num));
        assert!(visible(&doc, &state, &ocmd(30)), "AnyOn default: 10 is on");
        assert!(!visible(&doc, &state, &ocmd(31)), "AllOn: 11 is off");
        assert!(visible(&doc, &state, &ocmd(32)), "AnyOff: 11 is off");
        assert!(!visible(&doc, &state, &ocmd(33)), "AllOff: 10 is on");
        assert!(
            !visible(&doc, &state, &ocmd(34)),
            "AnyOn over one off group"
        );
        assert!(visible(&doc, &state, &ocmd(35)), "empty /OCGs is visible");
        assert!(visible(&doc, &state, &ocmd(36)), "nulls are ignored");
        assert!(
            !visible(&doc, &state, &ocmd(37)),
            "single group by reference"
        );
        assert!(
            !visible(&doc, &state, &ocmd(38)),
            "unknown policy reads AnyOn"
        );
    }

    #[test]
    fn malformed_values_stay_visible() {
        let (doc, state) = split_state();
        assert!(visible(&doc, &state, &Object::Null));
        assert!(visible(&doc, &state, &Object::Int(3)));
        assert!(visible(&doc, &state, &Object::Ref(gref(999))), "dangling");
        let direct_group = Object::Dict({
            let mut d = Dict::new();
            d.insert(Name("Type".into()), Object::Name(Name("OCG".into())));
            d
        });
        assert!(
            visible(&doc, &state, &direct_group),
            "a direct group dictionary has no identity to be off"
        );
    }

    fn ve_doc(ve: &str) -> (Document, OcState) {
        let doc = doc_with_oc("<< /OCGs [10 0 R 11 0 R] /D << /OFF [11 0 R] >> >>", |b| {
            b.object(40, &format!("<< /Type /OCMD /OCGs [10 0 R] /VE {ve} >>"));
        });
        let state = doc.oc_state().expect("state");
        (doc, state)
    }

    fn ve_visible(ve: &str) -> bool {
        let (doc, state) = ve_doc(ve);
        visible(&doc, &state, &Object::Ref(gref(40)))
    }

    // Covers ISO 32000-1 §8.11.2.2.
    #[test]
    fn visibility_expressions_evaluate() {
        assert!(ve_visible("[/Not 11 0 R]"), "Not of an off group");
        assert!(!ve_visible("[/Not 10 0 R]"), "Not of an on group");
        assert!(!ve_visible("[/And 10 0 R 11 0 R]"));
        assert!(ve_visible("[/Or 10 0 R 11 0 R]"));
        assert!(!ve_visible("[/Or 11 0 R 11 0 R]"));
        assert!(
            ve_visible("[/Or 11 0 R [/Not 11 0 R]]"),
            "nested expression"
        );
        assert!(
            !ve_visible("[/And 10 0 R [/Not [/Not 11 0 R]]]"),
            "double negation"
        );
    }

    /// `/VE` takes precedence over `/OCGs` and `/P`: object 40 carries
    /// `/OCGs [10 0 R]` (on, so AnyOn would show it), yet an expression
    /// naming only the off group hides it.
    // Covers ISO 32000-1 §8.11.2.2.
    #[test]
    fn expression_takes_precedence_over_policy() {
        assert!(!ve_visible("[/And 11 0 R]"));
    }

    // Covers ISO 32000-1 §8.11.2.2.
    #[test]
    fn malformed_expressions_are_visible() {
        assert!(ve_visible("[]"), "no operator");
        assert!(ve_visible("[/And]"), "no operands");
        assert!(ve_visible("[/Not 11 0 R 11 0 R]"), "Not takes one operand");
        assert!(ve_visible("[/Xor 11 0 R]"), "unknown operator");
        assert!(ve_visible("[/And 11 0 R (text)]"), "non-group operand");
    }

    // Covers ISO 32000-1 §8.11.2.2.
    #[test]
    fn expression_depth_is_capped() {
        let mut ve = "11 0 R".to_string();
        for _ in 0..(MAX_VE_DEPTH + 2) {
            ve = format!("[/Not {ve}]");
        }
        assert!(ve_visible(&ve), "past the cap reads as visible");
    }

    /// The properties operand of `BDC /OC` may be a resource name; the
    /// lookup keeps the reference, so the named group's off state applies.
    // Covers ISO 32000-1 §14.6.2 and §8.11.3.2.
    #[test]
    fn named_properties_keep_group_identity() {
        let (doc, state) = split_state();
        let mut properties = Dict::new();
        properties.insert(Name("On".into()), Object::Ref(gref(10)));
        properties.insert(Name("Off".into()), Object::Ref(gref(11)));
        let mut res = Dict::new();
        res.insert(Name("Properties".into()), Object::Dict(properties));
        let chain = vec![Arc::new(res)];
        let src = Immediate(&doc);
        let named = |name: &str| Object::Name(Name(name.into()));
        assert!(block_on(state.props_visible_with(
            &src,
            &chain,
            &named("On")
        )));
        assert!(!block_on(state.props_visible_with(
            &src,
            &chain,
            &named("Off")
        )));
        assert!(
            block_on(state.props_visible_with(&src, &chain, &named("Nope"))),
            "an unresolvable name stays visible"
        );
    }

    /// Objects 12 and 13 are two more groups: 12 carries a `/Usage` with a
    /// View state of OFF and a Print state of ON; 13 an Export state of OFF
    /// and a Zoom range. `config` is the `/D` configuration.
    fn usage_doc(config: &str) -> Document {
        doc_with_oc(
            &format!("<< /OCGs [10 0 R 11 0 R 12 0 R 13 0 R] /D {config} >>"),
            |b| {
                b.object(
                    12,
                    "<< /Type /OCG /Name (print only) /Usage << /View << /ViewState /OFF >> \
                     /Print << /PrintState /ON /Subtype /Watermark >> >> >>",
                );
                b.object(
                    13,
                    "<< /Type /OCG /Name (screen only) /Usage << /Export << /ExportState /OFF >> \
                     /Zoom << /min 2 >> >> >>",
                );
            },
        )
    }

    /// The default state is the viewer's: the View event's usage
    /// application dictionaries apply, so a group whose `/ViewState` is OFF
    /// is hidden although the configuration's `/OFF` array does not name it,
    /// and the Export event's dictionaries do not apply.
    // Covers ISO 32000-1 §8.11.4.4 and §8.11.4.5.
    #[test]
    fn view_usage_application_hides_a_view_state_off_group() {
        let doc = usage_doc(
            "<< /AS [ << /Event /View /OCGs [12 0 R 13 0 R] /Category [/View] >> \
             << /Event /Export /OCGs [13 0 R] /Category [/Export] >> ] >>",
        );
        let state = doc.oc_state().expect("state");
        assert!(state.hidden(gref(12)), "ViewState OFF under the View event");
        assert!(
            !state.hidden(gref(13)),
            "the Export dictionary is not applied"
        );
        assert!(
            !state.hidden(gref(10)),
            "a group without /Usage is unchanged"
        );
    }

    /// Each event applies only the dictionaries carrying its `/Event`, and
    /// `None` applies none: the state printing and aggregating applications
    /// use, the default configuration alone.
    // Covers ISO 32000-1 §8.11.4.4 and §8.11.4.5.
    #[test]
    fn each_event_applies_its_own_dictionaries() {
        let doc = usage_doc(
            "<< /OFF [12 0 R] /AS [ << /Event /View /OCGs [12 0 R 13 0 R] /Category [/View] >> \
             << /Event /Print /OCGs [12 0 R] /Category [/Print] >> \
             << /Event /Export /OCGs [13 0 R] /Category [/Export] >> ] >>",
        );
        let print = state_for(&doc, Some(OcEvent::Print));
        assert!(
            !print.hidden(gref(12)),
            "PrintState ON turns the /OFF group on"
        );
        assert!(!print.hidden(gref(13)));
        let export = state_for(&doc, Some(OcEvent::Export));
        assert!(export.hidden(gref(12)), "the /OFF array still applies");
        assert!(
            export.hidden(gref(13)),
            "ExportState OFF under the Export event"
        );
        let none = state_for(&doc, None);
        assert!(none.hidden(gref(12)));
        assert!(!none.hidden(gref(13)), "no usage application at all");
    }

    /// Without `/AS` the usage dictionaries are information only: a
    /// `/ViewState` of OFF hides nothing. A dictionary naming a category
    /// the group's `/Usage` lacks, or only the Zoom, Language or User
    /// categories, leaves the state unchanged; later dictionaries win over
    /// earlier ones.
    // Covers ISO 32000-1 §8.11.4.4.
    #[test]
    fn usage_without_application_or_category_changes_nothing() {
        let doc = usage_doc("<< >>");
        assert!(!doc.oc_state().expect("state").hidden(gref(12)), "no /AS");
        let doc =
            usage_doc("<< /AS [ << /Event /View /OCGs [12 0 R] /Category [/Export /Zoom] >> ] >>");
        assert!(
            !doc.oc_state().expect("state").hidden(gref(12)),
            "categories the group has no usage entry for"
        );
        let doc = usage_doc(
            "<< /AS [ << /Event /View /OCGs [12 0 R] /Category [/View] >> \
             << /Event /View /OCGs [12 0 R] /Category [/Print] >> ] >>",
        );
        assert!(
            !doc.oc_state().expect("state").hidden(gref(12)),
            "the later dictionary's Print category (ON) wins"
        );
        let doc = usage_doc("<< /AS [ << /Event /View /Category [/View] >> ] >>");
        assert!(
            !doc.oc_state().expect("state").hidden(gref(12)),
            "no /OCGs means no groups are affected"
        );
    }

    /// Objects 14 and 15 are groups with `/Intent /Design` and
    /// `/Intent [/Design /View]`; `config` is the `/D` configuration.
    fn intent_doc(config: &str) -> Document {
        doc_with_oc(
            &format!("<< /OCGs [10 0 R 11 0 R 14 0 R 15 0 R] /D {config} >>"),
            |b| {
                b.object(14, "<< /Type /OCG /Name (guides) /Intent /Design >>");
                b.object(15, "<< /Type /OCG /Name (both) /Intent [/Design /View] >>");
            },
        )
    }

    /// A group whose intent shares no name with the configuration's (both
    /// default to View) has no effect on visibility: turned off, it still
    /// hides nothing. An intent array matches on any of its names.
    // Covers ISO 32000-1 §8.11.2.3.
    #[test]
    fn a_group_of_another_intent_never_hides_content() {
        let doc = intent_doc("<< /OFF [11 0 R 14 0 R 15 0 R] >>");
        let state = doc.oc_state().expect("state");
        assert!(state.hidden(gref(11)), "default intents match");
        assert!(
            !state.hidden(gref(14)),
            "Design against a View configuration"
        );
        assert!(state.hidden(gref(15)), "[/Design /View] shares View");
        assert!(
            visible(&doc, &state, &Object::Ref(gref(14))),
            "content in the Design group paints"
        );
    }

    /// The configuration's own `/Intent` selects the groups it controls:
    /// `/Design` controls only Design groups, `/All` every group, and an
    /// empty array none, so everything is visible.
    // Covers ISO 32000-1 §8.11.2.3 and §8.11.4.3.
    #[test]
    fn the_configuration_intent_selects_the_groups_it_controls() {
        let design = intent_doc("<< /Intent /Design /OFF [11 0 R 14 0 R 15 0 R] >>");
        let state = design.oc_state().expect("state");
        assert!(
            !state.hidden(gref(11)),
            "a View group under a Design configuration"
        );
        assert!(state.hidden(gref(14)));
        assert!(state.hidden(gref(15)));
        let all = intent_doc("<< /Intent [/All] /OFF [11 0 R 14 0 R] >>");
        let state = all.oc_state().expect("state");
        assert!(state.hidden(gref(11)));
        assert!(state.hidden(gref(14)));
        let none = intent_doc("<< /Intent [] /OFF [11 0 R 14 0 R] >>");
        let state = none.oc_state().expect("state");
        assert!(!state.hidden(gref(11)), "an empty intent controls nothing");
        assert!(!state.hidden(gref(14)));
    }

    /// The intent rule is applied after the usage application: a Design
    /// group the View usage turns off still hides nothing.
    // Covers ISO 32000-1 §8.11.2.3 and §8.11.4.5.
    #[test]
    fn the_intent_rule_applies_after_usage() {
        let doc = doc_with_oc(
            "<< /OCGs [10 0 R 16 0 R] /D << /AS [ << /Event /View /OCGs [16 0 R] /Category [/View] >> ] >> >>",
            |b| {
                b.object(
                    16,
                    "<< /Type /OCG /Name (design, view off) /Intent /Design \
                     /Usage << /View << /ViewState /OFF >> >> >>",
                );
            },
        );
        assert!(!doc.oc_state().expect("state").hidden(gref(16)));
    }

    /// Every group in `/OCGs` order with its name, intents, usage entries
    /// and state; the state follows the event asked for.
    // Covers ISO 32000-1 §8.11.2.1, §8.11.2.3 and §8.11.4.4.
    #[test]
    fn groups_read_as_data() {
        let doc = doc_with_oc(
            "<< /OCGs [10 0 R 17 0 R 999 0 R] /D << /OFF [17 0 R] \
             /AS [ << /Event /Print /OCGs [17 0 R] /Category [/Print] >> ] >> >>",
            |b| {
                b.object(
                    17,
                    "<< /Type /OCG /Name <FEFF00450073> /Intent [/Design /View] /Usage << \
                     /CreatorInfo << /Creator (CAD) /Subtype /Technical >> \
                     /Language << /Lang (es-MX) /Preferred /ON >> \
                     /Export << /ExportState /OFF >> \
                     /Zoom << /min 0.5 /max 4 >> \
                     /Print << /Subtype /Watermark /PrintState /ON >> \
                     /View << /ViewState /OFF >> \
                     /User << /Type /Org /Name [(Acme) (Globex)] >> \
                     /PageElement << /Subtype /HF >> >> >>",
                );
            },
        );
        let groups = doc.optional_content_groups(Some(OcEvent::View));
        assert_eq!(groups.len(), 2, "a dangling reference is skipped");
        assert_eq!(
            groups[0],
            OcGroup {
                reference: gref(10),
                name: Some("one".to_string()),
                intent: vec!["View".to_string()],
                usage: OcUsage::default(),
                visible: true,
            }
        );
        assert_eq!(
            groups[1],
            OcGroup {
                reference: gref(17),
                name: Some("Es".to_string()),
                intent: vec!["Design".to_string(), "View".to_string()],
                usage: OcUsage {
                    view: Some(false),
                    print: Some(true),
                    print_subtype: Some("Watermark".to_string()),
                    export: Some(false),
                    zoom_min: Some(0.5),
                    zoom_max: Some(4.0),
                    language: Some("es-MX".to_string()),
                    language_preferred: true,
                    page_element: Some("HF".to_string()),
                    creator: Some("CAD".to_string()),
                    creator_subtype: Some("Technical".to_string()),
                    user_type: Some("Org".to_string()),
                    user_names: vec!["Acme".to_string(), "Globex".to_string()],
                },
                visible: false,
            }
        );
        let printed = doc.optional_content_groups(Some(OcEvent::Print));
        assert!(printed[1].visible, "PrintState ON under the Print event");
        assert!(
            doc_with_oc("", |_| {})
                .optional_content_groups(None)
                .is_empty(),
            "no /OCProperties, no groups"
        );
    }
}
