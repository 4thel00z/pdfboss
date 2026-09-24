//! Structure-tree reading order (ISO 32000-1 §14.7): where a page's
//! marked-content sequences sit in the document's logical structure, so a
//! tagged page can be read in the order its author declared, and which
//! structure type the element holding each sequence declares (§14.7.3).

use std::sync::Arc;

use crate::document::Page;
use crate::hash::{FastMap, FastSet};
use crate::object::{Dict, ObjRef, Object};
use crate::source::AsyncObjectSource;
use crate::tree::resolved_dict;

/// Maximum number of ancestors walked from an element up to the root.
/// Deeper ancestry reads as malformed and leaves the element unranked.
const MAX_ELEMENT_DEPTH: usize = 64;

/// One marked-content sequence: its `/MCID`, and the `/StructParents` key of
/// the content stream it appeared in: the page's, or a form XObject's own
/// when the form declares one (§14.7.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarkedContentId {
    pub parents: u32,
    pub mcid: u32,
}

/// One content item of a structure element (§14.7.4): a marked-content
/// sequence of a content stream, or a whole object, an annotation or an
/// XObject, that the element holds through an object reference dictionary
/// (`/Type /OBJR`, §14.7.4.3).
///
/// Covers ISO 32000-1 §14.7.4 and §14.7.4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentItem {
    Sequence(MarkedContentId),
    Object(ObjRef),
}

/// A content item with its place in the structure tree.
///
/// Covers ISO 32000-1 §14.7.4.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedItem {
    pub item: ContentItem,
    pub placement: Placement,
}

/// The four groups §14.8.4 sorts the standard structure types into, one per
/// clause that defines them; the block-level and inline-level ones are the
/// two the basic layout model of §14.8.3 lays out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardKind {
    /// §14.8.4.2: elements that group other elements and hold no content
    /// of their own.
    Grouping,
    /// §14.8.4.3: paragraphs, headings, lists and tables, laid out as
    /// blocks.
    BlockLevel,
    /// §14.8.4.4: elements within a block's text.
    InlineLevel,
    /// §14.8.4.5: figures, formulas and forms.
    Illustration,
}

/// The standard structure types of ISO 32000-1 §14.8.4, each variant spelled
/// as the standard spells the `/S` name it stands for.
///
/// Covers ISO 32000-1 §14.8.4.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardType {
    // Grouping elements (§14.8.4.2).
    Document,
    Part,
    Art,
    Sect,
    Div,
    BlockQuote,
    Caption,
    TOC,
    TOCI,
    Index,
    NonStruct,
    Private,
    // Block-level structure elements (§14.8.4.3).
    P,
    H,
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
    L,
    LI,
    Lbl,
    LBody,
    Table,
    TR,
    TH,
    TD,
    THead,
    TBody,
    TFoot,
    // Inline-level structure elements (§14.8.4.4).
    Span,
    Quote,
    Note,
    Reference,
    BibEntry,
    Code,
    Link,
    Annot,
    Ruby,
    RB,
    RT,
    RP,
    Warichu,
    WT,
    WP,
    // Illustration elements (§14.8.4.5).
    Figure,
    Formula,
    Form,
}

impl StandardType {
    /// Every standard type, grouping elements first, in the standard's order.
    pub const ALL: [StandardType; 49] = [
        StandardType::Document,
        StandardType::Part,
        StandardType::Art,
        StandardType::Sect,
        StandardType::Div,
        StandardType::BlockQuote,
        StandardType::Caption,
        StandardType::TOC,
        StandardType::TOCI,
        StandardType::Index,
        StandardType::NonStruct,
        StandardType::Private,
        StandardType::P,
        StandardType::H,
        StandardType::H1,
        StandardType::H2,
        StandardType::H3,
        StandardType::H4,
        StandardType::H5,
        StandardType::H6,
        StandardType::L,
        StandardType::LI,
        StandardType::Lbl,
        StandardType::LBody,
        StandardType::Table,
        StandardType::TR,
        StandardType::TH,
        StandardType::TD,
        StandardType::THead,
        StandardType::TBody,
        StandardType::TFoot,
        StandardType::Span,
        StandardType::Quote,
        StandardType::Note,
        StandardType::Reference,
        StandardType::BibEntry,
        StandardType::Code,
        StandardType::Link,
        StandardType::Annot,
        StandardType::Ruby,
        StandardType::RB,
        StandardType::RT,
        StandardType::RP,
        StandardType::Warichu,
        StandardType::WT,
        StandardType::WP,
        StandardType::Figure,
        StandardType::Formula,
        StandardType::Form,
    ];

    /// The standard type a structure type name stands for, `None` for a name
    /// outside the standard set. Names are case-sensitive; a document's own
    /// types reach the standard set through the role map (§14.7.3), not here.
    pub fn from_name(name: &str) -> Option<StandardType> {
        StandardType::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The name as the standard spells it.
    pub fn name(self) -> &'static str {
        match self {
            StandardType::Document => "Document",
            StandardType::Part => "Part",
            StandardType::Art => "Art",
            StandardType::Sect => "Sect",
            StandardType::Div => "Div",
            StandardType::BlockQuote => "BlockQuote",
            StandardType::Caption => "Caption",
            StandardType::TOC => "TOC",
            StandardType::TOCI => "TOCI",
            StandardType::Index => "Index",
            StandardType::NonStruct => "NonStruct",
            StandardType::Private => "Private",
            StandardType::P => "P",
            StandardType::H => "H",
            StandardType::H1 => "H1",
            StandardType::H2 => "H2",
            StandardType::H3 => "H3",
            StandardType::H4 => "H4",
            StandardType::H5 => "H5",
            StandardType::H6 => "H6",
            StandardType::L => "L",
            StandardType::LI => "LI",
            StandardType::Lbl => "Lbl",
            StandardType::LBody => "LBody",
            StandardType::Table => "Table",
            StandardType::TR => "TR",
            StandardType::TH => "TH",
            StandardType::TD => "TD",
            StandardType::THead => "THead",
            StandardType::TBody => "TBody",
            StandardType::TFoot => "TFoot",
            StandardType::Span => "Span",
            StandardType::Quote => "Quote",
            StandardType::Note => "Note",
            StandardType::Reference => "Reference",
            StandardType::BibEntry => "BibEntry",
            StandardType::Code => "Code",
            StandardType::Link => "Link",
            StandardType::Annot => "Annot",
            StandardType::Ruby => "Ruby",
            StandardType::RB => "RB",
            StandardType::RT => "RT",
            StandardType::RP => "RP",
            StandardType::Warichu => "Warichu",
            StandardType::WT => "WT",
            StandardType::WP => "WP",
            StandardType::Figure => "Figure",
            StandardType::Formula => "Formula",
            StandardType::Form => "Form",
        }
    }

    /// The clause of §14.8.4 that defines the type.
    pub fn kind(self) -> StandardKind {
        match self {
            StandardType::Document
            | StandardType::Part
            | StandardType::Art
            | StandardType::Sect
            | StandardType::Div
            | StandardType::BlockQuote
            | StandardType::Caption
            | StandardType::TOC
            | StandardType::TOCI
            | StandardType::Index
            | StandardType::NonStruct
            | StandardType::Private => StandardKind::Grouping,
            StandardType::P
            | StandardType::H
            | StandardType::H1
            | StandardType::H2
            | StandardType::H3
            | StandardType::H4
            | StandardType::H5
            | StandardType::H6
            | StandardType::L
            | StandardType::LI
            | StandardType::Lbl
            | StandardType::LBody
            | StandardType::Table
            | StandardType::TR
            | StandardType::TH
            | StandardType::TD
            | StandardType::THead
            | StandardType::TBody
            | StandardType::TFoot => StandardKind::BlockLevel,
            StandardType::Span
            | StandardType::Quote
            | StandardType::Note
            | StandardType::Reference
            | StandardType::BibEntry
            | StandardType::Code
            | StandardType::Link
            | StandardType::Annot
            | StandardType::Ruby
            | StandardType::RB
            | StandardType::RT
            | StandardType::RP
            | StandardType::Warichu
            | StandardType::WT
            | StandardType::WP => StandardKind::InlineLevel,
            StandardType::Figure | StandardType::Formula | StandardType::Form => {
                StandardKind::Illustration
            }
        }
    }
}

/// One structure element on a placement's path: its standard type and the
/// object holding it, so two neighbouring elements of one type are told
/// apart, and its revision number.
///
/// Covers ISO 32000-1 §14.8.4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructureElement {
    pub standard_type: StandardType,
    pub object: ObjRef,
    /// The element's `/R` (§14.7.5.3), 0 when it declares none.
    pub revision: i64,
}

/// The standard attribute owners of ISO 32000-1 Table 331: the four owners
/// the standard defines attributes for, and the seven document formats whose
/// own attributes an attribute object may carry.
///
/// Covers ISO 32000-1 §14.8.5.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardOwner {
    /// `Layout`: layout attributes (§14.8.5.4).
    Layout,
    /// `List`: the list attribute (§14.8.5.5).
    List,
    /// `PrintField`: print-field attributes (§14.8.5.6).
    PrintField,
    /// `Table`: table attributes (§14.8.5.7).
    Table,
    /// `XML-1.00`: attributes of XML 1.00.
    Xml100,
    /// `HTML-3.2`: attributes of HTML 3.2.
    Html32,
    /// `HTML-4.01`: attributes of HTML 4.01.
    Html401,
    /// `OEB-1.0`: attributes of the Open eBook 1.0 format.
    Oeb10,
    /// `RTF-1.05`: attributes of RTF 1.05.
    Rtf105,
    /// `CSS-1.00`: attributes of CSS 1.00.
    Css100,
    /// `CSS-2.00`: attributes of CSS 2.00.
    Css200,
}

impl StandardOwner {
    /// Every standard owner, in the order of Table 331.
    pub const ALL: [StandardOwner; 11] = [
        StandardOwner::Layout,
        StandardOwner::List,
        StandardOwner::PrintField,
        StandardOwner::Table,
        StandardOwner::Xml100,
        StandardOwner::Html32,
        StandardOwner::Html401,
        StandardOwner::Oeb10,
        StandardOwner::Rtf105,
        StandardOwner::Css100,
        StandardOwner::Css200,
    ];

    /// The standard owner an `/O` name stands for, `None` for a producer's
    /// own owner (or `UserProperties`, §14.7.5.4, which is not one of them).
    /// Names are case-sensitive.
    pub fn from_name(name: &str) -> Option<StandardOwner> {
        StandardOwner::ALL
            .into_iter()
            .find(|owner| owner.name() == name)
    }

    /// The name as the standard spells it.
    pub fn name(self) -> &'static str {
        match self {
            StandardOwner::Layout => "Layout",
            StandardOwner::List => "List",
            StandardOwner::PrintField => "PrintField",
            StandardOwner::Table => "Table",
            StandardOwner::Xml100 => "XML-1.00",
            StandardOwner::Html32 => "HTML-3.2",
            StandardOwner::Html401 => "HTML-4.01",
            StandardOwner::Oeb10 => "OEB-1.0",
            StandardOwner::Rtf105 => "RTF-1.05",
            StandardOwner::Css100 => "CSS-1.00",
            StandardOwner::Css200 => "CSS-2.00",
        }
    }
}

/// One attribute object of a structure element (§14.7.5): the element it
/// belongs to, its owner (`/O`, empty when it names none), the revision
/// number that follows it in an `/A` or `/C` array (0 when none does,
/// §14.7.5.3), and its entries as written, `/O` included.
///
/// Covers ISO 32000-1 §14.7.5.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeObject {
    pub element: ObjRef,
    pub owner: String,
    pub revision: i64,
    pub entries: Dict,
}

impl AttributeObject {
    /// The standard owner the object's `/O` names (§14.8.5.2), `None` for a
    /// producer's own.
    pub fn standard_owner(&self) -> Option<StandardOwner> {
        StandardOwner::from_name(&self.owner)
    }
}

/// Where one marked-content sequence sits in the tree: its rank in the
/// tree's depth-first order, and the structure type of the element holding
/// it, as written, after the root's `/RoleMap`, and as a standard type,
/// with the standard-typed elements above it.
///
/// Covers ISO 32000-1 §14.7.3 and §14.8.4.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub rank: u32,
    /// The element's `/S`, as the file writes it.
    pub structure_type: Option<String>,
    /// `structure_type` followed through the role map until a name the map
    /// has no entry for; the same name when the map never names it.
    pub mapped_type: Option<String>,
    /// The standard type `mapped_type` names, `None` when it names none.
    pub standard_type: Option<StandardType>,
    /// The element and its ancestors that have a standard type, the root's
    /// child first and the element itself last; an ancestor of no standard
    /// type is skipped, its children keeping their place.
    pub path: Vec<StructureElement>,
    /// The alternate description (`/Alt`, §14.9.3) of the element, or of
    /// the nearest ancestor that has one, decoded as a text string.
    pub alt: Option<String>,
    /// The language (`/Lang`, §14.9.2) of the element, or of the nearest
    /// ancestor that declares one; `None` leaves the document's own.
    pub lang: Option<String>,
    /// The expansion (`/E`, §14.9.5) of the abbreviation or acronym the
    /// element's text is, or the nearest ancestor's, decoded as a text
    /// string.
    pub expansion: Option<String>,
    /// Every attribute object of the element and its ancestors (§14.7.5),
    /// the root's child's first; for one element, the objects its `/C`
    /// classes name come before its direct `/A` objects, so a later object
    /// overrides an earlier one (see [`Placement::attribute`]).
    pub attributes: Vec<AttributeObject>,
}

impl Placement {
    /// The value `key` takes for `element` among its attribute objects of
    /// `owner`: the last object that has the key wins, so a direct `/A`
    /// object overrides a class's and a later class an earlier one, the
    /// order §14.8.5.3 gives the standard attributes. Inheritance from an
    /// ancestor is the caller's, since only some attributes inherit.
    ///
    /// Covers ISO 32000-1 §14.7.5, §14.7.5.2, §14.8.5 and §14.8.5.3.
    pub fn attribute(&self, element: ObjRef, owner: &str, key: &str) -> Option<&Object> {
        self.attributes
            .iter()
            .rev()
            .filter(|a| a.element == element && a.owner == owner)
            .find_map(|a| a.entries.get(key))
    }
}

/// The document's structure tree root (`/StructTreeRoot`), loaded once per
/// document and asked per page where that page's marked content sits in
/// the tree. `/MarkInfo` is never consulted: a tree with leaves counts,
/// whatever the file says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureTree {
    root: Dict,
    root_ref: Option<ObjRef>,
    /// The root's `/RoleMap`: structure type names to the names they stand
    /// for (§14.7.3), entries whose value is not a name dropped.
    role_map: FastMap<String, String>,
    /// The root's `/ClassMap`: attribute class names to their attribute
    /// objects, as written (§14.7.5.2).
    class_map: FastMap<String, Object>,
}

impl StructureTree {
    /// Loads the catalog's `/StructTreeRoot`, or `None` when the document
    /// declares none, or when the entry is missing or unreadable, which
    /// leaves every page in content order.
    ///
    /// Covers ISO 32000-1 §14.7.2.
    pub async fn load_with<S: AsyncObjectSource>(src: &S, trailer: &Dict) -> Option<StructureTree> {
        let root = trailer.get("Root")?;
        let catalog = src.resolve(root).await.ok()?;
        let entry = catalog.as_dict()?.get("StructTreeRoot")?;
        let root_ref = entry.as_ref();
        let resolved = src.resolve(entry).await.ok()?;
        let root = resolved.as_dict()?.clone();
        let role_map = match root.get("RoleMap") {
            Some(entry) => role_map_of(resolved_dict(src, entry).await),
            None => FastMap::default(),
        };
        let class_map = match root.get("ClassMap") {
            Some(entry) => class_map_of(resolved_dict(src, entry).await),
            None => FastMap::default(),
        };
        Some(StructureTree {
            root,
            root_ref,
            role_map,
            class_map,
        })
    }

    /// `name` followed through the root's `/RoleMap` until a name the map
    /// has no entry for. The map is meant to reach a standard structure type
    /// (§14.8.4) in one step; a chain is followed in case a mapped name is
    /// itself mapped, and a cycle leaves the name as written.
    ///
    /// Covers ISO 32000-1 §14.7.3.
    pub fn mapped_type(&self, name: &str) -> String {
        let mut seen: FastSet<&str> = FastSet::default();
        let mut current = name;
        while let Some(next) = self.role_map.get(current) {
            if !seen.insert(current) {
                return name.to_string();
            }
            current = next;
        }
        current.to_string()
    }

    /// The ranks of [`StructureTree::place_with`] alone: one page's
    /// marked-content sequences by their position in the tree's depth-first
    /// order, 0 for the first the tree reaches.
    ///
    /// Covers ISO 32000-1 §14.8.2 and §14.8.2.3.
    pub async fn ranks_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        page: &Page,
        ids: &[MarkedContentId],
    ) -> FastMap<MarkedContentId, u32> {
        self.place_with(src, page, ids)
            .await
            .into_iter()
            .map(|(id, placement)| (id, placement.rank))
            .collect()
    }

    /// Places `ids`, one page's marked-content sequences, in the tree: each
    /// gets its rank in the tree's depth-first order (0 for the first the
    /// tree reaches, and so on) and the structure type of the element holding
    /// it. An id the tree never reaches (untagged content, a key the parent
    /// tree lacks, an element whose ancestry is broken) is absent, so an
    /// empty map means the page has no leaves in the tree.
    ///
    /// The lookup goes through the parent tree (`/ParentTree`, keyed by
    /// `/StructParents`) and each element's `/P` chain, so it costs the page's
    /// own elements, never a walk of the whole tree.
    ///
    /// Covers ISO 32000-1 §14.7.3, §14.8.2 and §14.8.2.3.
    pub async fn place_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        page: &Page,
        ids: &[MarkedContentId],
    ) -> FastMap<MarkedContentId, Placement> {
        let items: Vec<ContentItem> = ids.iter().copied().map(ContentItem::Sequence).collect();
        self.place_items_with(src, page, &items)
            .await
            .into_iter()
            .filter_map(|(item, placement)| match item {
                ContentItem::Sequence(id) => Some((id, placement)),
                ContentItem::Object(_) => None,
            })
            .collect()
    }

    /// Every content item of `page` the tree reaches, in the tree's
    /// depth-first order: the marked-content sequences of the page's own
    /// content stream (every entry of the parent tree's array under the
    /// page's `/StructParents`) and the annotations of its `/Annots` that
    /// carry a `/StructParent`. Ranks are shared, so an annotation's place
    /// among the sequences is the place §14.8.2.3.2 gives it: right before
    /// the sequence that follows it in the tree. The sequences of a form
    /// XObject with its own `/StructParents` are not enumerated here; pass
    /// them to [`StructureTree::place_items_with`] when a walk of the page
    /// content has found them.
    ///
    /// Covers ISO 32000-1 §14.7.4, §14.7.4.3, §14.7.4.4 and §14.8.2.3.2.
    pub async fn content_items_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        page: &Page,
    ) -> Vec<PlacedItem> {
        let Some((parent_tree, mut walk)) = self.walk(src, page).await else {
            return Vec::new();
        };
        let mut items: Vec<ContentItem> = Vec::new();
        let page_dict = page.dict();
        let parents = page_dict
            .get_int("StructParents")
            .and_then(|n| u32::try_from(n).ok());
        if let Some(parents) = parents {
            if let Some(entries) = walk.parent_array(&parent_tree, parents).await {
                items.extend(
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.as_ref().is_some())
                        .map(|(mcid, _)| {
                            ContentItem::Sequence(MarkedContentId {
                                parents,
                                mcid: mcid as u32,
                            })
                        }),
                );
            }
        }
        for r in annotation_refs(src, page_dict).await {
            let Some(dict) = walk.dict(r).await else {
                continue;
            };
            if dict.get("StructParent").is_some() {
                items.push(ContentItem::Object(r));
            }
        }
        let mut placed: Vec<PlacedItem> = walk
            .place(&parent_tree, self, &items)
            .await
            .into_iter()
            .map(|(item, placement)| PlacedItem { item, placement })
            .collect();
        placed.sort_by_key(|placed| placed.placement.rank);
        placed
    }

    /// Places `items`, content items of `page`, in the tree: each gets its
    /// rank in the tree's depth-first order among the items given (0 for
    /// the first the tree reaches, and so on) and the structure type of the
    /// element holding it. A sequence is found through the parent tree's
    /// array under its `/StructParents` key, an object through the parent
    /// tree's entry under the object's own `/StructParent` (§14.7.4.4) and
    /// then the object reference among the element's kids (§14.7.4.3). An
    /// item the tree never reaches is absent.
    ///
    /// Covers ISO 32000-1 §14.7.4, §14.7.4.3 and §14.7.4.4.
    pub async fn place_items_with<S: AsyncObjectSource>(
        &self,
        src: &S,
        page: &Page,
        items: &[ContentItem],
    ) -> FastMap<ContentItem, Placement> {
        let Some((parent_tree, mut walk)) = self.walk(src, page).await else {
            return FastMap::default();
        };
        walk.place(&parent_tree, self, items).await
    }

    /// The resolved parent tree and a fresh walk of `page`, or `None` when
    /// the root names no readable `/ParentTree`, which leaves every content
    /// item unplaced.
    async fn walk<'a, S: AsyncObjectSource>(
        &'a self,
        src: &'a S,
        page: &Page,
    ) -> Option<(Dict, Walk<'a, S>)> {
        let parent_tree = self.root.get("ParentTree")?;
        let parent_tree = resolved_dict(src, parent_tree).await?;
        let walk = Walk {
            src,
            page_ref: page.object_ref(),
            root_ref: self.root_ref,
            class_map: &self.class_map,
            dicts: FastMap::default(),
            paths: FastMap::default(),
            parents: FastMap::default(),
            parent_elements: FastMap::default(),
        };
        Some((parent_tree, walk))
    }
}

impl<S: AsyncObjectSource> Walk<'_, S> {
    /// [`StructureTree::place_items_with`] over this walk: the sort keys of
    /// every item the tree reaches, then ranks by key order.
    ///
    /// Covers ISO 32000-1 §14.7.3, §14.8.2 and §14.8.2.3.
    async fn place(
        &mut self,
        parent_tree: &Dict,
        tree: &StructureTree,
        items: &[ContentItem],
    ) -> FastMap<ContentItem, Placement> {
        let mut placed: FastMap<ContentItem, Placement> = FastMap::default();
        let mut keyed: Vec<(ContentItem, Vec<u32>, Ancestry)> = Vec::new();
        let mut seen: FastSet<ContentItem> = FastSet::default();
        for item in items {
            if !seen.insert(*item) {
                continue;
            }
            let found = match item {
                ContentItem::Sequence(id) => self.key_of(parent_tree, *id).await,
                ContentItem::Object(r) => self.object_key_of(parent_tree, *r).await,
            };
            let Some((key, ancestry)) = found else {
                continue;
            };
            keyed.push((*item, key, ancestry));
        }
        keyed.sort_by(|a, b| a.1.cmp(&b.1));
        for (rank, (id, _, ancestry)) in keyed.into_iter().enumerate() {
            let structure_type = ancestry.last().and_then(|a| a.structure_type.clone());
            let mapped_type = structure_type.as_deref().map(|s| tree.mapped_type(s));
            let standard_type = mapped_type.as_deref().and_then(StandardType::from_name);
            let path = ancestry
                .iter()
                .filter_map(|ancestor| {
                    let name = ancestor.structure_type.as_deref()?;
                    let standard_type = StandardType::from_name(&tree.mapped_type(name))?;
                    Some(StructureElement {
                        standard_type,
                        object: ancestor.object,
                        revision: ancestor.revision,
                    })
                })
                .collect();
            let alt = ancestry.iter().rev().find_map(|a| a.alt.clone());
            let lang = ancestry.iter().rev().find_map(|a| a.lang.clone());
            let expansion = ancestry.iter().rev().find_map(|a| a.expansion.clone());
            let attributes = ancestry
                .into_iter()
                .flat_map(|ancestor| ancestor.attributes)
                .collect();
            placed.insert(
                id,
                Placement {
                    rank: rank as u32,
                    structure_type,
                    mapped_type,
                    standard_type,
                    path,
                    alt,
                    lang,
                    expansion,
                    attributes,
                },
            );
        }
        placed
    }
}

/// One element on the way from a marked-content sequence up to the root:
/// its object, its `/S` as written, its `/Alt` and `/Lang` decoded, its
/// `/R` and its attribute objects.
struct Ancestor {
    object: ObjRef,
    structure_type: Option<String>,
    alt: Option<String>,
    lang: Option<String>,
    expansion: Option<String>,
    revision: i64,
    attributes: Vec<AttributeObject>,
}

/// An element and its ancestors up to the root, the root's child first and
/// the element last.
type Ancestry = Vec<Ancestor>;

/// The `/ClassMap` dictionary as class name to attribute objects, the
/// objects kept as written and read when an element names the class.
///
/// Covers ISO 32000-1 §14.7.5.2.
fn class_map_of(dict: Option<Dict>) -> FastMap<String, Object> {
    let Some(dict) = dict else {
        return FastMap::default();
    };
    dict.iter()
        .map(|(key, value)| (key.0.clone(), value.clone()))
        .collect()
}

/// One attribute object as read: its `/O` owner (empty when it names none)
/// and its entries, at revision 0 until an array says otherwise. A
/// `/UserProperties` object arrives like any other, its `/P` array
/// uninterpreted.
///
/// Covers ISO 32000-1 §14.7.5 and §14.7.5.4.
fn attribute_object(element: ObjRef, entries: Dict) -> AttributeObject {
    AttributeObject {
        element,
        owner: entries
            .get_name("O")
            .map(|n| n.0.clone())
            .unwrap_or_default(),
        revision: 0,
        entries,
    }
}

/// The `/RoleMap` dictionary as name-to-name pairs.
///
/// Covers ISO 32000-1 §14.7.3.
fn role_map_of(dict: Option<Dict>) -> FastMap<String, String> {
    let Some(dict) = dict else {
        return FastMap::default();
    };
    dict.iter()
        .filter_map(|(key, value)| Some((key.0.clone(), value.as_name()?.0.clone())))
        .collect()
}

/// One page's walk through the tree: the dictionaries it has already read
/// and the paths it has already computed, so a paragraph's ancestry is
/// walked once for every marked-content sequence it contains.
struct Walk<'a, S> {
    src: &'a S,
    page_ref: Option<ObjRef>,
    root_ref: Option<ObjRef>,
    /// The root's `/ClassMap`, for elements that name attribute classes.
    class_map: &'a FastMap<String, Object>,
    dicts: FastMap<ObjRef, Option<Arc<Dict>>>,
    /// Each element's kid-index path from the root, or `None` once its
    /// ancestry proved unwalkable.
    paths: FastMap<ObjRef, Option<Arc<Vec<u32>>>>,
    /// The parent tree's array for each `/StructParents` key seen.
    parents: FastMap<u32, Option<Arc<Vec<Object>>>>,
    /// The parent tree's element for each `/StructParent` key seen.
    parent_elements: FastMap<u32, Option<ObjRef>>,
}

impl<S: AsyncObjectSource> Walk<'_, S> {
    /// The sort key of one marked-content sequence, its element's path from
    /// the root then its own index among the element's kids, with the
    /// element's ancestry and its structure types (`/S`, §14.7.3).
    async fn key_of(
        &mut self,
        parent_tree: &Dict,
        id: MarkedContentId,
    ) -> Option<(Vec<u32>, Ancestry)> {
        let elements = self.parent_array(parent_tree, id.parents).await?;
        let element = elements.get(id.mcid as usize)?.as_ref()?;
        let path = self.path_of(element).await?;
        let dict = self.dict(element).await?;
        let index = self.mcid_index(&dict, id).await?;
        let mut key = Vec::with_capacity(path.len() + 1);
        key.extend_from_slice(&path);
        key.push(index);
        Some((key, self.ancestry(element).await))
    }

    /// The sort key of one whole object, an annotation or an XObject: the
    /// parent tree's entry under the object's own `/StructParent` is its
    /// element (§14.7.4.4), and the object reference dictionary naming it
    /// among the element's kids gives its index (§14.7.4.3).
    ///
    /// Covers ISO 32000-1 §14.7.4.3 and §14.7.4.4.
    async fn object_key_of(
        &mut self,
        parent_tree: &Dict,
        object: ObjRef,
    ) -> Option<(Vec<u32>, Ancestry)> {
        let dict = self.dict(object).await?;
        let key = u32::try_from(dict.get_int("StructParent")?).ok()?;
        let element = self.parent_element(parent_tree, key).await?;
        let path = self.path_of(element).await?;
        let dict = self.dict(element).await?;
        let index = self.object_index(&dict, object).await?;
        let mut key = Vec::with_capacity(path.len() + 1);
        key.extend_from_slice(&path);
        key.push(index);
        Some((key, self.ancestry(element).await))
    }

    /// The parent tree's entry for a `/StructParent` key: a reference to
    /// the element holding the object as a content item. An array there
    /// (a `/StructParents` entry named by mistake) is no element.
    ///
    /// Covers ISO 32000-1 §14.7.4.4.
    async fn parent_element(&mut self, parent_tree: &Dict, key: u32) -> Option<ObjRef> {
        if let Some(cached) = self.parent_elements.get(&key) {
            return *cached;
        }
        let found = crate::tree::lookup(self.src, parent_tree, &i64::from(key))
            .await
            .and_then(|entry| entry.as_ref());
        self.parent_elements.insert(key, found);
        found
    }

    /// The index among `element`'s kids of the object reference dictionary
    /// (§14.7.4.3, Table 325) whose `/Obj` is `object` and whose page is
    /// this one: the dictionary's own `/Pg`, or the element's when it has
    /// none. A direct dictionary is taken first; only when there is none
    /// are the indirect kids read.
    ///
    /// Covers ISO 32000-1 §14.7.4.3.
    async fn object_index(&mut self, element: &Dict, object: ObjRef) -> Option<u32> {
        let kids = kids_of(element);
        let page_ref = self.page_ref;
        let element_page = element.get_ref("Pg");
        let refers = |d: &Dict| {
            d.get_ref("Obj") == Some(object) && on_page(d.get_ref("Pg").or(element_page), page_ref)
        };
        let direct = kids.iter().position(|kid| match kid {
            Object::Dict(d) => refers(d),
            _ => false,
        });
        if let Some(index) = direct {
            return u32::try_from(index).ok();
        }
        for (index, kid) in kids.iter().enumerate() {
            let Some(r) = kid.as_ref() else {
                continue;
            };
            let Some(dict) = self.dict(r).await else {
                continue;
            };
            if refers(&dict) {
                return u32::try_from(index).ok();
            }
        }
        None
    }

    /// Whether a marked-content reference dictionary (§14.7.4.2, Table 324)
    /// is the one for `id`: its `/MCID` matches, its page (its own `/Pg`,
    /// or the element's when it has none) is this one, and when it names
    /// the content stream the sequence sits in (`/Stm`), that stream's
    /// `/StructParents` is the key the sequence was filed under, so a
    /// form's sequence and the page's cannot stand in for each other. A
    /// stream without the key is not held against the match.
    ///
    /// Covers ISO 32000-1 §14.7.4.2.
    async fn is_reference_for(
        &mut self,
        dict: &Dict,
        id: MarkedContentId,
        element_page: Option<ObjRef>,
    ) -> bool {
        if !is_mcr(dict, id.mcid) || !on_page(dict.get_ref("Pg").or(element_page), self.page_ref) {
            return false;
        }
        let Some(stream) = dict.get_ref("Stm") else {
            return true;
        };
        let Some(stream) = self.dict(stream).await else {
            return true;
        };
        match stream.get_int("StructParents") {
            Some(parents) => u32::try_from(parents).is_ok_and(|p| p == id.parents),
            None => true,
        }
    }

    /// The element and its ancestors up to the root, the root's child first
    /// and the element itself last, each with its `/S`, `/Alt`, `/Lang`,
    /// `/R` and attribute objects: what a placement's structure type, path,
    /// description, language and attributes are read from. The climb stops
    /// at the root, at a missing `/P`, or after [`MAX_ELEMENT_DEPTH`]
    /// elements; the dictionaries are the ones [`Walk::path_of`] already
    /// read.
    ///
    /// Covers ISO 32000-1 §14.7.3, §14.7.5, §14.8.4.3, §14.9.2, §14.9.3 and
    /// §14.9.5.
    async fn ancestry(&mut self, element: ObjRef) -> Ancestry {
        let mut chain: Ancestry = Vec::new();
        let mut current = element;
        for _ in 0..MAX_ELEMENT_DEPTH {
            let Some(dict) = self.dict(current).await else {
                break;
            };
            let alt = match dict.get("Alt") {
                Some(alt) => self.text_string(alt).await,
                None => None,
            };
            let lang = match dict.get("Lang") {
                Some(lang) => self.text_string(lang).await,
                None => None,
            };
            let expansion = match dict.get("E") {
                Some(expansion) => self.text_string(expansion).await,
                None => None,
            };
            let attributes = self.attribute_objects(current, &dict).await;
            chain.push(Ancestor {
                object: current,
                structure_type: dict.get_name("S").map(|n| n.0.clone()),
                alt,
                lang,
                expansion,
                revision: dict.get_int("R").unwrap_or(0),
                attributes,
            });
            let Some(parent) = dict.get("P").and_then(Object::as_ref) else {
                break;
            };
            if self.is_root(parent).await {
                break;
            }
            current = parent;
        }
        chain.reverse();
        chain
    }

    /// The attribute objects of one element (§14.7.5): those its `/C`
    /// classes name in the root's `/ClassMap` (§14.7.5.2) first, then its
    /// direct `/A` objects, so a later object overrides an earlier one.
    ///
    /// Covers ISO 32000-1 §14.7.5 and §14.7.5.2.
    async fn attribute_objects(&mut self, element: ObjRef, dict: &Dict) -> Vec<AttributeObject> {
        let mut out = Vec::new();
        if let Some(classes) = dict.get("C") {
            self.class_attributes(element, classes, &mut out).await;
        }
        if let Some(direct) = dict.get("A") {
            self.direct_attributes(element, direct, &mut out).await;
        }
        out
    }

    /// `value` as attribute objects: one dictionary, or an array of
    /// dictionaries, indirect ones resolved, each optionally followed by an
    /// integer, its revision number; anything else in the array is skipped.
    ///
    /// Covers ISO 32000-1 §14.7.5 and §14.7.5.3.
    async fn direct_attributes(
        &mut self,
        element: ObjRef,
        value: &Object,
        out: &mut Vec<AttributeObject>,
    ) {
        let Ok(resolved) = self.src.resolve(value).await else {
            return;
        };
        let items = match resolved {
            Object::Dict(dict) => {
                out.push(attribute_object(element, dict));
                return;
            }
            Object::Array(items) => items,
            _ => return,
        };
        let start = out.len();
        for item in items {
            if let Some(revision) = item.as_int() {
                if out.len() > start {
                    if let Some(last) = out.last_mut() {
                        last.revision = revision;
                    }
                }
                continue;
            }
            let Ok(resolved) = self.src.resolve(&item).await else {
                continue;
            };
            if let Object::Dict(dict) = resolved {
                out.push(attribute_object(element, dict));
            }
        }
    }

    /// `value` as attribute class names, one or an array each optionally
    /// followed by a revision number that then applies to every object the
    /// class brought; a name the root's `/ClassMap` lacks brings nothing.
    ///
    /// Covers ISO 32000-1 §14.7.5.2 and §14.7.5.3.
    async fn class_attributes(
        &mut self,
        element: ObjRef,
        value: &Object,
        out: &mut Vec<AttributeObject>,
    ) {
        let Ok(resolved) = self.src.resolve(value).await else {
            return;
        };
        let names = match resolved {
            Object::Name(_) => vec![resolved],
            Object::Array(items) => items,
            _ => return,
        };
        let mut added = 0..0;
        for item in names {
            if let Some(revision) = item.as_int() {
                for attribute in &mut out[added.clone()] {
                    attribute.revision = revision;
                }
                continue;
            }
            let Some(name) = item.as_name() else {
                continue;
            };
            let Some(class) = self.class_map.get(&name.0).cloned() else {
                continue;
            };
            let start = out.len();
            self.direct_attributes(element, &class, out).await;
            added = start..out.len();
        }
    }

    /// A text string entry (§7.9.2.2), resolved and decoded; `None` for
    /// anything but a string.
    async fn text_string(&mut self, value: &Object) -> Option<String> {
        let resolved = self.src.resolve(value).await.ok()?;
        Some(crate::object::decode_text_string(resolved.as_str_bytes()?))
    }

    /// The parent tree's entry for a `/StructParents` key: the array whose
    /// index is a marked-content id and whose value is that id's element.
    ///
    /// Covers ISO 32000-1 §14.7.4.4.
    async fn parent_array(&mut self, parent_tree: &Dict, key: u32) -> Option<Arc<Vec<Object>>> {
        if let Some(cached) = self.parents.get(&key) {
            return cached.clone();
        }
        let found = match crate::tree::lookup(self.src, parent_tree, &i64::from(key)).await {
            Some(entry) => match self.src.resolve(&entry).await.ok()? {
                Object::Array(items) => Some(Arc::new(items)),
                _ => None,
            },
            None => None,
        };
        self.parents.insert(key, found.clone());
        found
    }

    async fn dict(&mut self, r: ObjRef) -> Option<Arc<Dict>> {
        if let Some(cached) = self.dicts.get(&r) {
            return cached.clone();
        }
        let loaded = match self.src.get(r).await.ok()? {
            Object::Dict(dict) => Some(Arc::new(dict)),
            Object::Stream(stream) => Some(Arc::new(stream.dict)),
            _ => None,
        };
        self.dicts.insert(r, loaded.clone());
        loaded
    }

    /// An element's kid-index path from the root: the ancestry is followed
    /// up through `/P` until it reaches the structure tree root, then each
    /// ancestor's index among its parent's kids is read on the way back down.
    /// Every ancestor's own path is remembered as a by-product.
    ///
    /// Covers ISO 32000-1 §14.7.2.
    async fn path_of(&mut self, element: ObjRef) -> Option<Arc<Vec<u32>>> {
        if let Some(cached) = self.paths.get(&element) {
            return cached.clone();
        }
        let mut chain: Vec<ObjRef> = vec![element];
        // The ancestor the climb stopped at, with its path: the root
        // itself, or an ancestor whose path an earlier climb computed.
        let mut stop: Option<(ObjRef, bool, Arc<Vec<u32>>)> = None;
        let mut current = element;
        for _ in 0..MAX_ELEMENT_DEPTH {
            let Some(dict) = self.dict(current).await else {
                break;
            };
            let Some(parent) = dict.get("P").and_then(Object::as_ref) else {
                break;
            };
            if self.is_root(parent).await {
                stop = Some((parent, true, Arc::new(Vec::new())));
                break;
            }
            if let Some(cached) = self.paths.get(&parent) {
                stop = cached.clone().map(|path| (parent, false, path));
                break;
            }
            chain.push(parent);
            current = parent;
        }
        let Some((mut parent_ref, mut parent_is_root, known)) = stop else {
            for r in chain {
                self.paths.insert(r, None);
            }
            return None;
        };
        // From the topmost unresolved ancestor down to the element itself.
        let mut path: Vec<u32> = (*known).clone();
        let mut resolved: Option<Arc<Vec<u32>>> = None;
        for child in chain.into_iter().rev() {
            let index = if parent_is_root {
                self.root_kid_index(child).await
            } else {
                match self.dict(parent_ref).await {
                    Some(parent) => kid_index(&parent, child),
                    None => None,
                }
            };
            let Some(index) = index else {
                self.paths.insert(child, None);
                return None;
            };
            path.push(index);
            let shared = Arc::new(path.clone());
            self.paths.insert(child, Some(shared.clone()));
            resolved = Some(shared);
            parent_ref = child;
            parent_is_root = false;
        }
        resolved
    }

    /// Whether `r` is the structure tree root: the reference the catalog
    /// named, or failing that a dictionary typed `/StructTreeRoot`.
    async fn is_root(&mut self, r: ObjRef) -> bool {
        if self.root_ref == Some(r) {
            return true;
        }
        let Some(dict) = self.dict(r).await else {
            return false;
        };
        dict.get_name("Type")
            .is_some_and(|n| n.0 == "StructTreeRoot")
    }

    /// The index of a top-level element among the root's `/K` kids.
    async fn root_kid_index(&mut self, child: ObjRef) -> Option<u32> {
        let root_ref = self.root_ref?;
        let root = self.dict(root_ref).await?;
        kid_index(&root, child)
    }

    /// The index among `element`'s kids of the marked-content sequence
    /// numbered `mcid` on this page: a bare integer (the element's `/Pg`
    /// page) or a marked-content reference dictionary naming the page. A
    /// direct match is taken first; only when there is none are the
    /// indirect kids read, in case the reference dictionary is one of them.
    ///
    /// Covers ISO 32000-1 §14.7.4 and §14.7.4.2.
    async fn mcid_index(&mut self, element: &Dict, id: MarkedContentId) -> Option<u32> {
        let kids = kids_of(element);
        let page_ref = self.page_ref;
        let element_page = element.get_ref("Pg");
        for (index, kid) in kids.iter().enumerate() {
            let found = match kid {
                Object::Int(n) => {
                    u32::try_from(*n).is_ok_and(|n| n == id.mcid) && on_page(element_page, page_ref)
                }
                Object::Dict(d) => self.is_reference_for(d, id, element_page).await,
                _ => false,
            };
            if found {
                return u32::try_from(index).ok();
            }
        }
        for (index, kid) in kids.iter().enumerate() {
            let Some(r) = kid.as_ref() else {
                continue;
            };
            let Some(dict) = self.dict(r).await else {
                continue;
            };
            if self.is_reference_for(&dict, id, element_page).await {
                return u32::try_from(index).ok();
            }
        }
        None
    }
}

/// The indirect references in a page's `/Annots`, in order. A direct
/// annotation dictionary has no reference an object reference dictionary
/// could name, so it is skipped.
async fn annotation_refs<S: AsyncObjectSource>(src: &S, page: &Dict) -> Vec<ObjRef> {
    let Some(annots) = page.get("Annots") else {
        return Vec::new();
    };
    let Ok(Object::Array(items)) = src.resolve(annots).await else {
        return Vec::new();
    };
    items.iter().filter_map(Object::as_ref).collect()
}

/// Whether a kid's `/Pg` names this page. Either side unknown reads as a
/// match: a page inlined into `/Kids` has no reference to compare, and a
/// bare integer kid under an element without `/Pg` has nothing to compare
/// against.
fn on_page(pg: Option<ObjRef>, page_ref: Option<ObjRef>) -> bool {
    match (pg, page_ref) {
        (Some(pg), Some(page)) => pg == page,
        _ => true,
    }
}

/// Whether `dict` is the marked-content reference for `mcid`.
///
/// Covers ISO 32000-1 §14.7.4.2.
fn is_mcr(dict: &Dict, mcid: u32) -> bool {
    dict.get_int("MCID")
        .and_then(|n| u32::try_from(n).ok())
        .is_some_and(|n| n == mcid)
}

/// An element's `/K` as a list: a single kid stands alone, an array is its
/// items, nothing is empty.
fn kids_of(element: &Dict) -> Vec<Object> {
    match element.get("K") {
        Some(Object::Array(items)) => items.clone(),
        Some(single) => vec![single.clone()],
        None => Vec::new(),
    }
}

/// The index of `child` among `parent`'s kids, by reference identity.
fn kid_index(parent: &Dict, child: ObjRef) -> Option<u32> {
    let index = kids_of(parent)
        .iter()
        .position(|kid| kid.as_ref() == Some(child))?;
    u32::try_from(index).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block_on, Document, Immediate};
    use pdfboss_testkit::PdfBuilder;

    fn id(parents: u32, mcid: u32) -> MarkedContentId {
        MarkedContentId { parents, mcid }
    }

    /// A one-page document whose catalog names object 10 as the structure
    /// tree root; `objects` supplies the tree (10 and up) and `page_extra`
    /// lands in the page dictionary.
    fn tagged_doc(page_extra: &str, objects: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.object(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>",
        );
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {page_extra} >>"),
        );
        for (num, body) in objects {
            b.object(*num, body);
        }
        Document::load(b.build(1)).expect("load")
    }

    /// Two paragraphs on one page, the left one holding ids 0 and 2, the
    /// right one 1 and 3: tree order is 0, 2, 1, 3.
    fn two_paragraphs(parent_tree: &str) -> Document {
        tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R] >>",
                ),
                (12, parent_tree),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0 2] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1 3] >>",
                ),
            ],
        )
    }

    fn ranks(doc: &Document, ids: &[MarkedContentId]) -> FastMap<MarkedContentId, u32> {
        let tree = doc.structure_tree().expect("tree");
        let page = doc.page(0).unwrap();
        block_on(tree.ranks_with(&Immediate(doc), &page, ids))
    }

    fn placements(doc: &Document, ids: &[MarkedContentId]) -> FastMap<MarkedContentId, Placement> {
        let tree = doc.structure_tree().expect("tree");
        let page = doc.page(0).unwrap();
        block_on(tree.place_with(&Immediate(doc), &page, ids))
    }

    // Covers ISO 32000-1 §14.7.3.
    #[test]
    fn structure_types_are_read_and_mapped_through_the_role_map() {
        // Four elements typed Para, Sub, Loop1 and P. The role map, an
        // indirect object, maps Para to P, Sub to Head and Head to H1, and
        // Loop1 and Loop2 to each other.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R /RoleMap 20 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R 15 0 R 16 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R 14 0 R 15 0 R 16 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /Para /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /Sub /P 11 0 R /Pg 3 0 R /K [1] >>",
                ),
                (
                    15,
                    "<< /Type /StructElem /S /Loop1 /P 11 0 R /Pg 3 0 R /K [2] >>",
                ),
                (
                    16,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [3] >>",
                ),
                (
                    20,
                    "<< /Para /P /Sub /Head /Head /H1 /Loop1 /Loop2 /Loop2 /Loop1 >>",
                ),
            ],
        );
        let placed = placements(&doc, &[id(0, 3), id(0, 2), id(0, 1), id(0, 0)]);
        let typed = |n: u32| {
            let p = &placed[&id(0, n)];
            (p.structure_type.as_deref(), p.mapped_type.as_deref())
        };
        assert_eq!(typed(0), (Some("Para"), Some("P")));
        assert_eq!(typed(1), (Some("Sub"), Some("H1")));
        assert_eq!(typed(2), (Some("Loop1"), Some("Loop1")));
        assert_eq!(typed(3), (Some("P"), Some("P")));
        assert_eq!(placed[&id(0, 3)].rank, 3);
        assert_eq!(placed[&id(0, 0)].standard_type, Some(StandardType::P));
        assert_eq!(placed[&id(0, 1)].standard_type, Some(StandardType::H1));
        assert_eq!(placed[&id(0, 2)].standard_type, None);
        let tree = doc.structure_tree().unwrap();
        assert_eq!(tree.mapped_type("Head"), "H1");
        assert_eq!(tree.mapped_type("Span"), "Span");
    }

    // Covers ISO 32000-1 §14.7.3.
    #[test]
    fn an_element_without_a_type_places_with_none() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (11, "<< /Type /StructElem /P 10 0 R /Pg 3 0 R /K [0] >>"),
                (12, "<< /Nums [0 [11 0 R]] >>"),
            ],
        );
        let placed = placements(&doc, &[id(0, 0)]);
        assert_eq!(placed[&id(0, 0)].rank, 0);
        assert_eq!(placed[&id(0, 0)].structure_type, None);
        assert_eq!(placed[&id(0, 0)].mapped_type, None);
        assert_eq!(placed[&id(0, 0)].standard_type, None);
    }

    // Covers ISO 32000-1 §14.7.3 and §14.8.4.3.
    #[test]
    fn placements_carry_the_standard_typed_ancestry() {
        // Document > Sect > H holds id 0; Document > L > LI > Lbl holds 1
        // and > LBody > P holds 2; a Sidebar element of no standard type
        // holds a P with id 3.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 15 0 R 20 0 R] >>",
                ),
                (12, "<< /Nums [0 [14 0 R 17 0 R 19 0 R 21 0 R]] >>"),
                (13, "<< /Type /StructElem /S /Sect /P 11 0 R /K [14 0 R] >>"),
                (
                    14,
                    "<< /Type /StructElem /S /H /P 13 0 R /Pg 3 0 R /K [0] >>",
                ),
                (15, "<< /Type /StructElem /S /L /P 11 0 R /K [16 0 R] >>"),
                (
                    16,
                    "<< /Type /StructElem /S /LI /P 15 0 R /K [17 0 R 18 0 R] >>",
                ),
                (
                    17,
                    "<< /Type /StructElem /S /Lbl /P 16 0 R /Pg 3 0 R /K [1] >>",
                ),
                (
                    18,
                    "<< /Type /StructElem /S /LBody /P 16 0 R /K [19 0 R] >>",
                ),
                (
                    19,
                    "<< /Type /StructElem /S /P /P 18 0 R /Pg 3 0 R /K [2] >>",
                ),
                (
                    20,
                    "<< /Type /StructElem /S /Sidebar /P 11 0 R /K [21 0 R] >>",
                ),
                (
                    21,
                    "<< /Type /StructElem /S /P /P 20 0 R /Pg 3 0 R /K [3] >>",
                ),
            ],
        );
        let placed = placements(&doc, &[id(0, 0), id(0, 1), id(0, 2), id(0, 3)]);
        let kinds = |n: u32| -> Vec<StandardType> {
            placed[&id(0, n)]
                .path
                .iter()
                .map(|e| e.standard_type)
                .collect()
        };
        let objects = |n: u32| -> Vec<u32> {
            placed[&id(0, n)]
                .path
                .iter()
                .map(|e| e.object.num)
                .collect()
        };
        assert_eq!(
            kinds(0),
            [StandardType::Document, StandardType::Sect, StandardType::H]
        );
        assert_eq!(objects(0), [11, 13, 14]);
        assert_eq!(
            kinds(1),
            [
                StandardType::Document,
                StandardType::L,
                StandardType::LI,
                StandardType::Lbl
            ]
        );
        assert_eq!(
            kinds(2),
            [
                StandardType::Document,
                StandardType::L,
                StandardType::LI,
                StandardType::LBody,
                StandardType::P
            ]
        );
        assert_eq!(kinds(3), [StandardType::Document, StandardType::P]);
        assert_eq!(objects(3), [11, 21]);
    }

    // Covers ISO 32000-1 §14.9.3.
    #[test]
    fn placements_carry_the_nearest_alternate_description() {
        // A Figure with /Alt holding a P with id 0; a P with no /Alt anywhere
        // above it holding id 1; a Span with a UTF-16 /Alt holding id 2.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 15 0 R 16 0 R] >>",
                ),
                (12, "<< /Nums [0 [14 0 R 15 0 R 16 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /Figure /P 11 0 R /Alt (A chart) /K [14 0 R] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 13 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    15,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1] >>",
                ),
                (
                    16,
                    "<< /Type /StructElem /S /Span /P 11 0 R /Pg 3 0 R /K [2] /Alt <FEFF00E9> >>",
                ),
            ],
        );
        let placed = placements(&doc, &[id(0, 0), id(0, 1), id(0, 2)]);
        assert_eq!(placed[&id(0, 0)].alt.as_deref(), Some("A chart"));
        assert_eq!(placed[&id(0, 1)].alt, None);
        assert_eq!(placed[&id(0, 2)].alt.as_deref(), Some("\u{e9}"));
    }

    // Covers ISO 32000-1 §14.9.5.
    #[test]
    fn placements_carry_the_nearest_expansion() {
        // A Span with /E holding a Span with id 0; a P with no /E anywhere
        // above it holding id 1; a Span with a UTF-16 /E holding id 2.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 15 0 R 16 0 R] >>",
                ),
                (12, "<< /Nums [0 [14 0 R 15 0 R 16 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /Span /P 11 0 R /E (Portable Document Format) /K [14 0 R] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /Span /P 13 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    15,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1] >>",
                ),
                (
                    16,
                    "<< /Type /StructElem /S /Span /P 11 0 R /Pg 3 0 R /K [2] /E <FEFF00C9> >>",
                ),
            ],
        );
        let placed = placements(&doc, &[id(0, 0), id(0, 1), id(0, 2)]);
        assert_eq!(
            placed[&id(0, 0)].expansion.as_deref(),
            Some("Portable Document Format")
        );
        assert_eq!(placed[&id(0, 1)].expansion, None);
        assert_eq!(placed[&id(0, 2)].expansion.as_deref(), Some("\u{c9}"));
    }

    // Covers ISO 32000-1 §14.9.2 and §14.9.2.3.
    #[test]
    fn placements_carry_the_nearest_language() {
        // The Document element says en; the second paragraph says de for
        // itself; the tree of `two_paragraphs` says nothing at all.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /Lang (en) /K [13 0 R 14 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R 14 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /Lang (de) /K [1] >>",
                ),
            ],
        );
        let placed = placements(&doc, &[id(0, 0), id(0, 1)]);
        assert_eq!(placed[&id(0, 0)].lang.as_deref(), Some("en"));
        assert_eq!(placed[&id(0, 1)].lang.as_deref(), Some("de"));
        let untagged = two_paragraphs("<< /Nums [0 [13 0 R 14 0 R 13 0 R 14 0 R]] >>");
        assert_eq!(placements(&untagged, &[id(0, 0)])[&id(0, 0)].lang, None);
    }

    // Covers ISO 32000-1 §14.7.5, §14.7.5.2 and §14.7.5.3.
    #[test]
    fn placements_carry_the_elements_attribute_objects() {
        // Cell 14 has one direct attribute object and revision 1; cell 15 a
        // class from the root's /ClassMap, numbered 4 in its /C array, and a
        // direct object overriding one of the class's entries; cell 16 an
        // /A array whose first object is followed by its revision number and
        // whose second is indirect; paragraph 17 has none.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R \
                     /ClassMap << /Wide << /O /Table /ColSpan 3 /RowSpan 2 >> >> >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Table /P 10 0 R /K [13 0 R 17 0 R] >>",
                ),
                (12, "<< /Nums [0 [14 0 R 15 0 R 16 0 R 17 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /TR /P 11 0 R /K [14 0 R 15 0 R 16 0 R] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /TD /P 13 0 R /Pg 3 0 R /K [0] /R 1 \
                     /A << /O /Table /ColSpan 2 >> >>",
                ),
                (
                    15,
                    "<< /Type /StructElem /S /TD /P 13 0 R /Pg 3 0 R /K [1] \
                     /C [/Wide 4] /A << /O /Table /ColSpan 1 >> >>",
                ),
                (
                    16,
                    "<< /Type /StructElem /S /TD /P 13 0 R /Pg 3 0 R /K [2] \
                     /A [<< /O /Layout /Placement /Block >> 2 20 0 R] >>",
                ),
                (
                    17,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [3] >>",
                ),
                (20, "<< /O /Table /RowSpan 3 >>"),
            ],
        );
        let placed = placements(&doc, &[id(0, 0), id(0, 1), id(0, 2), id(0, 3)]);
        let cell = |num: u32| ObjRef { num, gen: 0 };
        let int = |value: Option<&Object>| value.and_then(Object::as_int);
        let objects = |p: &Placement, element: ObjRef| -> Vec<(String, i64)> {
            p.attributes
                .iter()
                .filter(|a| a.element == element)
                .map(|a| (a.owner.clone(), a.revision))
                .collect()
        };

        let p0 = &placed[&id(0, 0)];
        assert_eq!(int(p0.attribute(cell(14), "Table", "ColSpan")), Some(2));
        assert_eq!(p0.attribute(cell(14), "Table", "RowSpan"), None);
        assert_eq!(p0.attributes.len(), 1, "the row and table carry none");
        assert_eq!(p0.path.last().unwrap().revision, 1);

        // Class objects come first, direct objects after and win.
        let p1 = &placed[&id(0, 1)];
        assert_eq!(
            objects(p1, cell(15)),
            [("Table".to_string(), 4), ("Table".to_string(), 0)]
        );
        assert_eq!(int(p1.attribute(cell(15), "Table", "ColSpan")), Some(1));
        assert_eq!(int(p1.attribute(cell(15), "Table", "RowSpan")), Some(2));
        assert_eq!(p1.path.last().unwrap().revision, 0);

        let p2 = &placed[&id(0, 2)];
        assert_eq!(
            objects(p2, cell(16)),
            [("Layout".to_string(), 2), ("Table".to_string(), 0)]
        );
        assert_eq!(int(p2.attribute(cell(16), "Table", "RowSpan")), Some(3));
        assert_eq!(
            p2.attribute(cell(16), "Layout", "Placement")
                .and_then(Object::as_name)
                .map(|n| n.0.as_str()),
            Some("Block")
        );
        assert_eq!(p2.attribute(cell(16), "Table", "ColSpan"), None);

        assert!(placed[&id(0, 3)].attributes.is_empty());
    }

    // Covers ISO 32000-1 §14.8.5.2.
    #[test]
    fn standard_attribute_owners_are_recognized_by_name() {
        assert_eq!(
            StandardOwner::from_name("Table"),
            Some(StandardOwner::Table)
        );
        assert_eq!(
            StandardOwner::from_name("XML-1.00"),
            Some(StandardOwner::Xml100)
        );
        assert_eq!(StandardOwner::from_name("Acme"), None);
        assert_eq!(StandardOwner::from_name("table"), None);
        assert_eq!(StandardOwner::ALL.len(), 11);
        for owner in StandardOwner::ALL {
            assert_eq!(
                StandardOwner::from_name(owner.name()),
                Some(owner),
                "{}",
                owner.name()
            );
        }
        let object = AttributeObject {
            element: ObjRef { num: 1, gen: 0 },
            owner: "Layout".to_string(),
            revision: 0,
            entries: Dict::default(),
        };
        assert_eq!(object.standard_owner(), Some(StandardOwner::Layout));
        let own = AttributeObject {
            owner: "Acme".to_string(),
            ..object
        };
        assert_eq!(own.standard_owner(), None);
    }

    // Covers ISO 32000-1 §14.8.4, §14.8.4.2, §14.8.4.3, §14.8.4.4 and
    // §14.8.4.5.
    #[test]
    fn standard_types_are_recognized_by_name_and_grouped_by_clause() {
        assert_eq!(StandardType::from_name("H1"), Some(StandardType::H1));
        assert_eq!(StandardType::H1.kind(), StandardKind::BlockLevel);
        assert_eq!(
            StandardType::from_name("TOCI").map(StandardType::kind),
            Some(StandardKind::Grouping)
        );
        assert_eq!(
            StandardType::from_name("LBody").map(StandardType::kind),
            Some(StandardKind::BlockLevel)
        );
        assert_eq!(
            StandardType::from_name("TFoot").map(StandardType::kind),
            Some(StandardKind::BlockLevel)
        );
        assert_eq!(
            StandardType::from_name("Ruby").map(StandardType::kind),
            Some(StandardKind::InlineLevel)
        );
        assert_eq!(
            StandardType::from_name("Formula").map(StandardType::kind),
            Some(StandardKind::Illustration)
        );
        // Names are case-sensitive and a document's own types are not standard.
        assert_eq!(StandardType::from_name("h1"), None);
        assert_eq!(StandardType::from_name("Para"), None);
        assert_eq!(StandardType::ALL.len(), 49);
        for t in StandardType::ALL {
            assert_eq!(StandardType::from_name(t.name()), Some(t), "{}", t.name());
        }
        let grouping = StandardType::ALL
            .iter()
            .filter(|t| t.kind() == StandardKind::Grouping)
            .count();
        assert_eq!(grouping, 12);
    }

    // Covers ISO 32000-1 §7.7.2.
    #[test]
    fn no_struct_tree_root_means_no_tree() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        let doc = Document::load(b.build(1)).unwrap();
        assert!(doc.structure_tree().is_none());
    }

    // Covers ISO 32000-1 §14.7.2 and §7.9.7.
    #[test]
    fn ranks_follow_the_tree_not_the_ids() {
        let doc = two_paragraphs("<< /Nums [0 [13 0 R 14 0 R 13 0 R 14 0 R]] >>");
        let ranks = ranks(&doc, &[id(0, 2), id(0, 3), id(0, 0), id(0, 1)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        assert_eq!(ranks[&id(0, 2)], 1);
        assert_eq!(ranks[&id(0, 1)], 2);
        assert_eq!(ranks[&id(0, 3)], 3);
    }

    // Covers ISO 32000-1 §14.7.4, §14.7.4.4 and §7.9.7.
    #[test]
    fn parent_tree_kids_and_limits_are_descended() {
        let doc = tagged_doc(
            "/StructParents 7",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
                ),
                (12, "<< /Kids [15 0 R 16 0 R] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (15, "<< /Limits [0 3] /Nums [0 [] 3 []] >>"),
                (16, "<< /Limits [7 9] /Nums [7 [13 0 R] 9 []] >>"),
            ],
        );
        let ranks = ranks(&doc, &[id(7, 0)]);
        assert_eq!(ranks[&id(7, 0)], 0);
    }

    // Covers ISO 32000-1 §14.7.4.4.
    #[test]
    fn a_page_without_a_parent_tree_entry_has_no_ranks() {
        let doc = two_paragraphs("<< /Nums [5 [13 0 R]] >>");
        assert!(ranks(&doc, &[id(0, 0), id(0, 1)]).is_empty());
    }

    #[test]
    fn untagged_and_out_of_range_ids_are_absent() {
        let doc = two_paragraphs("<< /Nums [0 [13 0 R 14 0 R 13 0 R 14 0 R]] >>");
        let ranks = ranks(&doc, &[id(0, 0), id(0, 9), id(4, 0)]);
        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks[&id(0, 0)], 0);
    }

    // Covers ISO 32000-1 §14.7.2.
    #[test]
    fn a_broken_ancestry_leaves_only_that_element_unranked() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R 14 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                // Not among its parent's kids: its path cannot be read.
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1] >>",
                ),
            ],
        );
        let ranks = ranks(&doc, &[id(0, 0), id(0, 1)]);
        assert_eq!(ranks.len(), 1);
        assert_eq!(ranks[&id(0, 0)], 0);
    }

    // Covers ISO 32000-1 §14.7.4 and §14.7.4.2.
    #[test]
    fn marked_content_references_name_their_page() {
        // One element spanning two pages: the same id number on each. The
        // reference naming this page wins the index, the other is skipped.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R \
                     /K [<< /Type /MCR /Pg 99 0 R /MCID 0 >> 15 0 R] >>",
                ),
                (12, "<< /Nums [0 [11 0 R]] >>"),
                (15, "<< /Type /MCR /Pg 3 0 R /MCID 0 >>"),
            ],
        );
        let ranks = ranks(&doc, &[id(0, 0)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        let tree = doc.structure_tree().unwrap();
        let page = doc.page(0).unwrap();
        let mut walk = Walk {
            src: &Immediate(&doc),
            page_ref: page.object_ref(),
            root_ref: Some(ObjRef { num: 10, gen: 0 }),
            class_map: &tree.class_map,
            dicts: FastMap::default(),
            paths: FastMap::default(),
            parents: FastMap::default(),
            parent_elements: FastMap::default(),
        };
        let element = block_on(walk.dict(ObjRef { num: 11, gen: 0 })).unwrap();
        assert_eq!(block_on(walk.mcid_index(&element, id(0, 0))), Some(1));
        drop(tree);
    }

    // Covers ISO 32000-1 §14.7.4.2.
    #[test]
    fn a_bare_integer_on_another_page_is_not_this_page() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R /Pg 99 0 R /K [0] >>",
                ),
                (12, "<< /Nums [0 [11 0 R]] >>"),
            ],
        );
        assert!(ranks(&doc, &[id(0, 0)]).is_empty());
    }

    // Covers ISO 32000-1 §14.7.4.4.
    #[test]
    fn a_form_key_ranks_alongside_the_page() {
        // The form's marked content (key 1) is a child of the second
        // paragraph; the page's (key 0) fills the first.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R] >>",
                ),
                (12, "<< /Nums [0 [13 0 R] 1 [14 0 R]] >>"),
                (
                    13,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
                (
                    14,
                    "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
                ),
            ],
        );
        let ranks = ranks(&doc, &[id(1, 0), id(0, 0)]);
        assert_eq!(ranks[&id(0, 0)], 0);
        assert_eq!(ranks[&id(1, 0)], 1);
    }

    // Covers ISO 32000-1 §14.7.2.
    #[test]
    fn a_single_kid_needs_no_array() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K 11 0 R /ParentTree 12 0 R >>",
                ),
                (11, "<< /Type /StructElem /S /P /P 10 0 R /Pg 3 0 R /K 0 >>"),
                (12, "<< /Nums [0 [11 0 R]] >>"),
            ],
        );
        assert_eq!(ranks(&doc, &[id(0, 0)])[&id(0, 0)], 0);
    }

    fn r(num: u32) -> ObjRef {
        ObjRef { num, gen: 0 }
    }

    fn content_items(doc: &Document) -> Vec<PlacedItem> {
        let tree = doc.structure_tree().expect("tree");
        let page = doc.page(0).unwrap();
        block_on(tree.content_items_with(&Immediate(doc), &page))
    }

    fn items_of(placed: &[PlacedItem]) -> Vec<(ContentItem, u32)> {
        placed
            .iter()
            .map(|p| (p.item, p.placement.rank))
            .collect()
    }

    /// A page with two paragraphs and a link between them: paragraph 13
    /// holds sequence 0, the Link element 14 holds the annotation 20 through
    /// `objr` (its `/K`), paragraph 15 holds sequence 1. The annotation
    /// carries `/StructParent 1`, the parent tree maps 0 to the page's array
    /// and 1 to the Link element.
    fn linked_doc(objr: &str, extra: &[(u32, &str)]) -> Document {
        let mut objects = vec![
            (
                10,
                "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
            ),
            (
                11,
                "<< /Type /StructElem /S /Document /P 10 0 R /K [13 0 R 14 0 R 15 0 R] >>",
            ),
            (12, "<< /Nums [0 [13 0 R 15 0 R] 1 14 0 R] >>"),
            (
                13,
                "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [0] >>",
            ),
            (
                15,
                "<< /Type /StructElem /S /P /P 11 0 R /Pg 3 0 R /K [1] >>",
            ),
            (
                20,
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /StructParent 1 >>",
            ),
        ];
        let link = format!("<< /Type /StructElem /S /Link /P 11 0 R /Pg 3 0 R /K {objr} >>");
        objects.push((14, link.as_str()));
        objects.extend_from_slice(extra);
        tagged_doc("/StructParents 0 /Annots [20 0 R]", &objects)
    }

    // Covers ISO 32000-1 §14.7.4.3, §14.7.4.4 and §14.8.2.3.2.
    #[test]
    fn an_annotation_takes_its_place_between_the_sequences() {
        let doc = linked_doc("[<< /Type /OBJR /Obj 20 0 R >>]", &[]);
        let placed = content_items(&doc);
        assert_eq!(
            items_of(&placed),
            vec![
                (ContentItem::Sequence(id(0, 0)), 0),
                (ContentItem::Object(r(20)), 1),
                (ContentItem::Sequence(id(0, 1)), 2),
            ]
        );
        let link = &placed[1].placement;
        assert_eq!(link.structure_type.as_deref(), Some("Link"));
        assert_eq!(link.standard_type, Some(StandardType::Link));
        assert_eq!(
            link.path
                .iter()
                .map(|e| e.standard_type)
                .collect::<Vec<_>>(),
            vec![StandardType::Document, StandardType::Link]
        );
    }

    // Covers ISO 32000-1 §14.7.4.3.
    #[test]
    fn an_indirect_object_reference_is_read_after_the_direct_kids() {
        let doc = linked_doc(
            "[21 0 R]",
            &[(21, "<< /Type /OBJR /Obj 20 0 R /Pg 3 0 R >>")],
        );
        let placed = content_items(&doc);
        assert_eq!(items_of(&placed)[1], (ContentItem::Object(r(20)), 1));
    }

    // Covers ISO 32000-1 §14.7.4.3.
    #[test]
    fn the_object_reference_page_overrides_the_elements() {
        // The Link element sits on page 99; only its object reference says
        // this page, which is what counts.
        let doc = tagged_doc(
            "/StructParents 0 /Annots [20 0 R]",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Link /P 10 0 R /Pg 99 0 R \
                     /K [<< /Type /OBJR /Obj 20 0 R /Pg 3 0 R >>] >>",
                ),
                (12, "<< /Nums [1 11 0 R] >>"),
                (20, "<< /Type /Annot /Subtype /Link /StructParent 1 >>"),
            ],
        );
        assert_eq!(
            items_of(&content_items(&doc)),
            vec![(ContentItem::Object(r(20)), 0)]
        );
        // Without its own page the reference inherits the element's, and
        // page 99 is not this page.
        let doc = tagged_doc(
            "/StructParents 0 /Annots [20 0 R]",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Link /P 10 0 R /Pg 99 0 R \
                     /K [<< /Type /OBJR /Obj 20 0 R >>] >>",
                ),
                (12, "<< /Nums [1 11 0 R] >>"),
                (20, "<< /Type /Annot /Subtype /Link /StructParent 1 >>"),
            ],
        );
        assert!(content_items(&doc).is_empty());
    }

    // Covers ISO 32000-1 §14.7.4.3 and §14.7.4.4.
    #[test]
    fn an_annotation_the_tree_does_not_hold_is_no_content_item() {
        // 21 has no /StructParent, 22's key is missing from the parent
        // tree, 23's element has no reference naming it, and the direct
        // dictionary has no reference to name.
        let doc = tagged_doc(
            "/StructParents 0 /Annots [20 0 R 21 0 R 22 0 R 23 0 R << /Subtype /Link /StructParent 1 >>]",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Link /P 10 0 R /Pg 3 0 R \
                     /K [<< /Type /OBJR /Obj 20 0 R >>] >>",
                ),
                (12, "<< /Nums [1 11 0 R 3 11 0 R] >>"),
                (20, "<< /Type /Annot /Subtype /Link /StructParent 1 >>"),
                (21, "<< /Type /Annot /Subtype /Link >>"),
                (22, "<< /Type /Annot /Subtype /Link /StructParent 2 >>"),
                (23, "<< /Type /Annot /Subtype /Link /StructParent 3 >>"),
            ],
        );
        assert_eq!(
            items_of(&content_items(&doc)),
            vec![(ContentItem::Object(r(20)), 0)]
        );
    }

    // Covers ISO 32000-1 §14.7.4.3 and §14.8.2.3.2.
    #[test]
    fn a_link_element_orders_its_sequence_and_annotation_by_kid_index() {
        let doc = tagged_doc(
            "/StructParents 0 /Annots [20 0 R]",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Link /P 10 0 R /Pg 3 0 R \
                     /K [<< /Type /OBJR /Obj 20 0 R >> << /Type /MCR /MCID 0 >>] >>",
                ),
                (12, "<< /Nums [0 [11 0 R] 1 11 0 R] >>"),
                (20, "<< /Type /Annot /Subtype /Link /StructParent 1 >>"),
            ],
        );
        assert_eq!(
            items_of(&content_items(&doc)),
            vec![
                (ContentItem::Object(r(20)), 0),
                (ContentItem::Sequence(id(0, 0)), 1),
            ]
        );
    }

    // Covers ISO 32000-1 §14.7.4.3.
    #[test]
    fn a_form_xobject_is_placed_when_handed_in() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /Figure /P 10 0 R /Pg 3 0 R \
                     /K [<< /Type /OBJR /Obj 30 0 R >>] >>",
                ),
                (12, "<< /Nums [5 11 0 R] >>"),
                (
                    30,
                    "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /StructParent 5 /Length 0 >>\nstream\n\nendstream",
                ),
            ],
        );
        let tree = doc.structure_tree().unwrap();
        let page = doc.page(0).unwrap();
        let placed = block_on(tree.place_items_with(
            &Immediate(&doc),
            &page,
            &[ContentItem::Object(r(30)), ContentItem::Object(r(30))],
        ));
        assert_eq!(placed.len(), 1);
        assert_eq!(
            placed[&ContentItem::Object(r(30))].standard_type,
            Some(StandardType::Figure)
        );
        // Not in /Annots, so the page enumeration does not find it.
        assert!(content_items(&doc).is_empty());
    }

    // Covers ISO 32000-1 §14.7.4.2.
    #[test]
    fn a_reference_naming_a_stream_matches_only_that_streams_sequence() {
        // The element holds the form's sequence 0 (kid 0, an MCR naming the
        // form) and the page's sequence 0 (kid 1, a bare integer). Both are
        // numbered 0; the MCR's /Stm tells them apart.
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R /Pg 3 0 R \
                     /K [<< /Type /MCR /Stm 30 0 R /MCID 0 >> 0] >>",
                ),
                (12, "<< /Nums [0 [11 0 R] 1 [11 0 R]] >>"),
                (
                    30,
                    "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /StructParents 1 /Length 0 >>\nstream\n\nendstream",
                ),
            ],
        );
        let ranks = ranks(&doc, &[id(0, 0), id(1, 0)]);
        assert_eq!(ranks[&id(1, 0)], 0);
        assert_eq!(ranks[&id(0, 0)], 1);
    }

    // Covers ISO 32000-1 §14.7.4 and §14.7.4.4.
    #[test]
    fn the_page_enumeration_skips_null_entries_and_needs_a_parent_tree() {
        let doc = tagged_doc(
            "/StructParents 0",
            &[
                (
                    10,
                    "<< /Type /StructTreeRoot /K [11 0 R] /ParentTree 12 0 R >>",
                ),
                (
                    11,
                    "<< /Type /StructElem /S /P /P 10 0 R /Pg 3 0 R /K [0 2] >>",
                ),
                (12, "<< /Nums [0 [11 0 R null 11 0 R]] >>"),
            ],
        );
        assert_eq!(
            items_of(&content_items(&doc)),
            vec![
                (ContentItem::Sequence(id(0, 0)), 0),
                (ContentItem::Sequence(id(0, 2)), 1),
            ]
        );
        let doc = tagged_doc(
            "/StructParents 0",
            &[(10, "<< /Type /StructTreeRoot /K [] >>")],
        );
        assert!(content_items(&doc).is_empty());
    }
}
