//! Core PDF machinery: syntax, objects, filters, cross-references, the
//! document model, and lazy element iteration (physical file structure with
//! byte spans plus logical document structure), implemented from the PDF
//! specification (ISO 32000).

pub mod article;
pub mod cmap;
pub mod content;
pub mod crypt;
pub mod date;
pub mod destination;
pub mod document;
pub mod elements;
pub mod embedded_file;
pub mod error;
pub mod extension;
pub mod filters;
pub mod form;
pub mod geom;
pub mod hash;
pub mod language;
pub mod lexer;
pub mod linearization;
pub mod names;
pub mod object;
pub mod objstm;
pub mod oc;
pub mod outline;
pub mod output_intent;
pub mod page_label;
pub mod parser;
pub mod permission;
pub mod piece_info;
pub mod presentation;
pub mod pretty;
pub mod requirement;
pub mod source;
pub mod structure;
pub mod thumbnail;
pub mod tree;
pub mod viewer_preferences;
pub mod xref;

pub use article::{articles_with, page_beads_with, ArticleThread, Bead};
pub use cmap::{cid_to_unicode, type0_encoding, CidCmap, CidToUnicode, Type0Encoding};
pub use crypt::{
    crypt_filter_refs, direct_crypt_filters, Decryptor, Encryptor, Permissions, PERMISSION_NAMES,
};
pub use date::Date;
pub use destination::{
    destination_value_with, destination_with, named_destination_with, named_destinations_with,
    Destination, DestinationPage, Fit,
};
pub use document::{
    content_stream_data_with, decoded_stream_data_with, map_pages, page_content_with, Document,
    DocumentSeed, Metadata, Page,
};
pub use elements::{Element, ElementOpts, Span, XrefKind};
pub use embedded_file::{
    embedded_file_data_with, embedded_files_with, file_spec_with, spec_components, EmbeddedFile,
    FileSpec,
};
pub use error::{Error, Result};
pub use extension::{extensions_with, DeveloperExtension};
pub use form::{
    form_fields_with, interactive_form_with, AppearanceCharacteristics, ButtonKind,
    CaptionPosition, ChoiceOption, DefaultAppearance, FieldFlags, FieldType, FormField,
    InteractiveForm, Quadding, Signature, SignatureFlags, Widget,
};
pub use geom::{Matrix, Point, Rect};
pub use hash::{FastMap, FastSet, FxHasher};
pub use language::{language_with, LanguageTag};
pub use linearization::{linearization_dictionary, Linearization};
pub use names::{name_tree_root_with, named_with, names_with, NameTree};
pub use object::{Dict, Name, ObjRef, Object, Stream};
pub use oc::OcState;
pub use outline::{outline_with, OutlineItem};
pub use output_intent::{output_intents_with, OutputIntent};
pub use page_label::{page_label, page_labels_with, LabelStyle, PageLabel};
pub use permission::{permission_handlers_with, PermissionHandlers};
pub use piece_info::{document_piece_info_with, piece_info_with, PagePiece};
pub use presentation::{
    presentation_with, Dimension, Motion, Presentation, Transition, TransitionDirection,
    TransitionStyle,
};
pub use requirement::{requirements_with, Requirement};
pub use source::{
    block_on, resolve_sync_with, resolve_with, AsyncObjectSource, BoxFuture, Immediate,
    ObjectSource, MAX_RESOLVE_DEPTH,
};
pub use structure::{
    AttributeObject, MarkedContentId, Placement, StandardKind, StandardOwner, StandardType,
    StructureElement, StructureTree,
};
pub use thumbnail::{thumbnail_with, Thumbnail};
pub use viewer_preferences::{
    viewer_preferences_with, Direction, Duplex, NonFullScreenPageMode, PageBoundary, PrintScaling,
    ViewerPreferences,
};
