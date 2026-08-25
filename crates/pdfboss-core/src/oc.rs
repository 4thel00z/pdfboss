//! Optional-content visibility (ISO 32000-1 §8.11): which optional content
//! groups the document's default configuration turns off, and whether
//! content gated by an `/OC` entry or a `BDC /OC` span is visible.

use std::sync::Arc;

use crate::hash::FastSet;
use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;

/// Maximum `/VE` visibility-expression nesting depth. Real expressions are
/// one or two levels deep; past the cap the expression reads as malformed,
/// and malformed means visible.
const MAX_VE_DEPTH: u32 = 8;

/// The document's optional-content visibility under its default
/// configuration (`/OCProperties` `/D`, ISO 32000-1 §8.11.4.3): the set of
/// groups that configuration turns off. A group's identity is its indirect
/// reference — groups are shared by reference between the configuration,
/// marked-content properties, and `/OC` entries (§8.11.2.1).
///
/// Everything here is lenient: an entry that is missing, malformed, or will
/// not resolve leaves content visible, never hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OcState {
    off: FastSet<ObjRef>,
}

impl OcState {
    /// Builds the state from the catalog's `/OCProperties`, or `None` when
    /// the document declares none — no optional content, everything
    /// visible. The default `/D` configuration is applied in specification
    /// order: `/BaseState` (default `ON`), then `/ON`, then `/OFF`, so a
    /// group named in both `/ON` and `/OFF` ends up off.
    pub async fn load_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<OcState> {
        let root = trailer.get("Root")?;
        let catalog = src.resolve(root).await.ok()?;
        let props = src
            .resolve(catalog.as_dict()?.get("OCProperties")?)
            .await
            .ok()?;
        let props = props.as_dict()?;
        let config = match props.get("D") {
            Some(o) => src.resolve(o).await.ok(),
            None => None,
        };
        let config = config.as_ref().and_then(Object::as_dict);
        let base_off = match config.and_then(|d| d.get("BaseState")) {
            Some(o) => matches!(
                src.resolve(o).await.ok().as_ref().and_then(Object::as_name),
                Some(n) if n.0 == "OFF"
            ),
            None => false,
        };
        let mut off: FastSet<ObjRef> = FastSet::default();
        if base_off {
            off.extend(group_refs(src, props.get("OCGs")).await);
        }
        if let Some(config) = config {
            for group in group_refs(src, config.get("ON")).await {
                off.remove(&group);
            }
            off.extend(group_refs(src, config.get("OFF")).await);
        }
        Some(OcState { off })
    }

    /// Whether the configuration turns `group` off.
    pub fn hidden(&self, group: ObjRef) -> bool {
        self.off.contains(&group)
    }

    /// Whether content gated by `value` — the operand of a stream or
    /// annotation dictionary's `/OC` entry — is visible: a group reference
    /// is visible unless the group is off; a membership dictionary follows
    /// [`OcState::ocmd_visible`]. A direct group dictionary has no
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

    /// Evaluates a `/VE` array (§8.11.2.3): `[/And|/Or|/Not operands…]`,
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

    #[test]
    fn no_ocproperties_means_no_state() {
        let doc = doc_with_oc("", |_| {});
        assert_eq!(doc.oc_state(), None);
    }

    #[test]
    fn base_state_defaults_on_and_off_hides() {
        let doc = doc_with_oc("<< /OCGs [10 0 R 11 0 R] /D << /OFF [11 0 R] >> >>", |_| {});
        let state = doc.oc_state().expect("state");
        assert!(!state.hidden(gref(10)));
        assert!(state.hidden(gref(11)));
    }

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

    #[test]
    fn group_reference_visibility_follows_the_off_set() {
        let (doc, state) = split_state();
        assert!(visible(&doc, &state, &Object::Ref(gref(10))));
        assert!(!visible(&doc, &state, &Object::Ref(gref(11))));
    }

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
    #[test]
    fn expression_takes_precedence_over_policy() {
        assert!(!ve_visible("[/And 11 0 R]"));
    }

    #[test]
    fn malformed_expressions_are_visible() {
        assert!(ve_visible("[]"), "no operator");
        assert!(ve_visible("[/And]"), "no operands");
        assert!(ve_visible("[/Not 11 0 R 11 0 R]"), "Not takes one operand");
        assert!(ve_visible("[/Xor 11 0 R]"), "unknown operator");
        assert!(ve_visible("[/And 11 0 R (text)]"), "non-group operand");
    }

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
}
