//! The object-source abstraction shared by the synchronous and asynchronous
//! APIs.
//!
//! `pdfboss` keeps exactly one implementation of every algorithm that reads a
//! document — text extraction, rasterization — and two ways of delivering
//! bytes to it. [`ObjectSource`] is what a caller holding the whole file
//! provides. `AsyncObjectSource` is what a caller streaming from a file or
//! a network provides. Algorithms are written once against the asynchronous
//! trait and reach synchronous callers through `Immediate` and
//! `block_on_ready`.
//!
//! Nothing here needs an async runtime: `Future`, `Pin`, `Box` and
//! `std::future::ready` are all in the standard library, so `pdfboss-core`
//! stays free of executor dependencies.

use crate::error::Result;
use crate::object::{ObjRef, Object, Stream};

/// Reference-chase depth limit for the provided [`ObjectSource::resolve`],
/// matching `Document::resolve`.
const MAX_RESOLVE_DEPTH: usize = 32;

/// Reads indirect objects and decodes streams, with the whole file already
/// available.
///
/// Implement `get` and `stream_data`; `resolve` has a provided
/// implementation over `get`. An implementor whose reference chasing has its
/// own semantics — as `Document`'s does — should override `resolve` too.
pub trait ObjectSource {
    /// Fetches an indirect object by reference.
    fn get(&self, r: ObjRef) -> Result<Object>;

    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this source.
    fn stream_data(&self, s: &Stream) -> Result<Vec<u8>>;

    /// Chases reference chains, depth-capped at `MAX_RESOLVE_DEPTH`.
    ///
    /// Lenient: a reference to a missing or unreadable object resolves to
    /// [`Object::Null`]. Exceeding the depth cap is
    /// [`crate::error::Error::CircularReference`].
    fn resolve(&self, o: &Object) -> Result<Object> {
        let mut current = o.clone();
        let mut last_num = 0;
        for _ in 0..MAX_RESOLVE_DEPTH {
            match current {
                Object::Ref(r) => {
                    last_num = r.num;
                    current = match self.get(r) {
                        Ok(object) => object,
                        Err(crate::error::Error::CircularReference(n)) => {
                            return Err(crate::error::Error::CircularReference(n))
                        }
                        Err(_) => return Ok(Object::Null),
                    };
                }
                other => return Ok(other),
            }
        }
        Err(crate::error::Error::CircularReference(last_num))
    }
}

#[cfg(test)]
mod tests {
    use crate::document::Document;
    use crate::object::{ObjRef, Object};
    use crate::source::ObjectSource;

    /// The trait impl must be a pure forward to the inherent methods: the
    /// same reference read both ways yields the same object.
    #[test]
    fn document_trait_get_matches_inherent_get() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let r = ObjRef { num: 1, gen: 0 };
        let inherent = doc.get(r).unwrap();
        let through_trait = ObjectSource::get(&doc, r).unwrap();
        assert_eq!(inherent, through_trait);
    }

    /// A reference to a missing object resolves to Null rather than erroring
    /// (Document::resolve is lenient); the trait must preserve that.
    #[test]
    fn document_trait_resolve_is_lenient_about_missing_targets() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let missing = Object::Ref(ObjRef { num: 9_999, gen: 0 });
        assert_eq!(
            ObjectSource::resolve(&doc, &missing).unwrap(),
            Object::Null,
            "a dangling reference must resolve to Null through the trait, \
             matching Document::resolve"
        );
    }
}
