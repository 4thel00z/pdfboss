//! The object-source abstraction shared by the synchronous and asynchronous
//! APIs.
//!
//! `pdfboss` keeps exactly one implementation of every algorithm that reads a
//! document — text extraction, rasterization — and two ways of delivering
//! bytes to it. [`ObjectSource`] is what a caller holding the whole file
//! provides. [`AsyncObjectSource`] is what a caller streaming from a file or
//! a network provides. Algorithms are written once against the asynchronous
//! trait and reach synchronous callers through [`Immediate`] and
//! [`block_on_ready`].
//!
//! Nothing here needs an async runtime: `Future`, `Pin`, `Box` and
//! `std::future::ready` are all in the standard library, so `pdfboss-core`
//! stays free of executor dependencies.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::error::Result;
use crate::object::{ObjRef, Object, Stream};

/// Reference-chase depth limit for the provided [`ObjectSource::resolve`],
/// matching `Document::resolve`.
const MAX_RESOLVE_DEPTH: usize = 32;

/// A boxed future, as returned by every [`AsyncObjectSource`] method.
///
/// Boxing keeps the trait object-safe (`dyn AsyncObjectSource` is usable) at
/// the cost of one allocation per object fetch. Fetches are per *resource* —
/// a font, an image, a form — never per glyph or per pixel, and the hot
/// non-fetching helpers keep taking already-loaded `&Object`, so the
/// allocation does not enter the rasterizer's inner loops.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

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

/// A shared reference to a source is itself a source, forwarding every method.
///
/// This is what lets a synchronous entry point that only holds `&self` wrap
/// itself for the asynchronous implementation — `Immediate(self)` builds an
/// `Immediate<&Self>` — without cloning or owning the document. Forwarding
/// `resolve` explicitly keeps the wrapped implementor's own reference chasing
/// rather than falling back to the provided loop over `get`.
impl<T: ObjectSource + ?Sized> ObjectSource for &T {
    fn get(&self, r: ObjRef) -> Result<Object> {
        (**self).get(r)
    }

    fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
        (**self).stream_data(s)
    }

    fn resolve(&self, o: &Object) -> Result<Object> {
        (**self).resolve(o)
    }
}

/// Reads indirect objects and decodes streams, awaiting whatever I/O that
/// takes.
///
/// This is the trait the shared algorithms are written against. A caller who
/// already holds the whole file reaches them through [`Immediate`], which
/// implements this trait with futures that are already complete.
pub trait AsyncObjectSource {
    /// Fetches an indirect object by reference.
    fn get(&self, r: ObjRef) -> BoxFuture<'_, Result<Object>>;

    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this source.
    fn stream_data<'a>(&'a self, s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>>;

    /// Chases reference chains, depth-capped at `MAX_RESOLVE_DEPTH`.
    ///
    /// Lenient in the same way as [`ObjectSource::resolve`]: a reference to a
    /// missing or unreadable object resolves to [`Object::Null`].
    fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
        Box::pin(async move {
            let mut current = o.clone();
            let mut last_num = 0;
            for _ in 0..MAX_RESOLVE_DEPTH {
                match current {
                    Object::Ref(r) => {
                        last_num = r.num;
                        current = match self.get(r).await {
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
        })
    }
}

/// Presents a synchronous [`ObjectSource`] as an [`AsyncObjectSource`] whose
/// futures are already complete.
///
/// Wrapping a source in `Immediate` is what lets the synchronous entry points
/// share the asynchronous implementation. Because every future this produces
/// is [`std::future::Ready`], a future tree built over it completes on its
/// first poll — see [`block_on_ready`].
pub struct Immediate<S>(pub S);

impl<S: ObjectSource> AsyncObjectSource for Immediate<S> {
    fn get(&self, r: ObjRef) -> BoxFuture<'_, Result<Object>> {
        Box::pin(std::future::ready(self.0.get(r)))
    }

    fn stream_data<'a>(&'a self, s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>> {
        Box::pin(std::future::ready(self.0.stream_data(s)))
    }

    fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
        Box::pin(std::future::ready(self.0.resolve(o)))
    }
}

/// Drives a future that cannot park, returning its output.
///
/// This is how the synchronous entry points run the shared asynchronous
/// implementation. It is sound for exactly one shape of future: one whose
/// every leaf await resolves against an [`Immediate`] source. Those leaves
/// are [`std::future::Ready`], so the tree returns [`Poll::Ready`] on its
/// first poll, the waker is never consulted, and parking cannot occur.
/// [`Poll::Pending`] is therefore unreachable rather than merely unlikely.
///
/// Do not call this on a future that performs real I/O; it would panic.
pub fn block_on_ready<F: Future>(future: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => {
            unreachable!("a future driven over an Immediate source cannot park")
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::document::Document;
    use crate::object::{ObjRef, Object};
    use crate::source::{block_on_ready, AsyncObjectSource, Immediate, ObjectSource};

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

    /// Reading through Immediate must agree with reading synchronously.
    #[test]
    fn immediate_get_matches_the_sync_source() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let r = ObjRef { num: 1, gen: 0 };
        let expected = ObjectSource::get(&doc, r).unwrap();

        let src = Immediate(&doc);
        let actual = block_on_ready(src.get(r)).unwrap();

        assert_eq!(actual, expected);
    }

    /// The provided async `resolve` must reproduce the sync leniency: a
    /// dangling reference becomes Null, not an error.
    #[test]
    fn immediate_resolve_is_lenient_about_missing_targets() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let missing = Object::Ref(ObjRef { num: 9_999, gen: 0 });

        let src = Immediate(&doc);
        assert_eq!(block_on_ready(src.resolve(&missing)).unwrap(), Object::Null);
    }

    /// `block_on_ready` completes a nested future tree — several awaits deep,
    /// which is the shape the executor will produce — on one poll.
    #[test]
    fn block_on_ready_drives_a_nested_future_tree() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let src = Immediate(&doc);

        async fn depth_three<S: AsyncObjectSource>(src: &S, r: ObjRef) -> Object {
            async fn inner<S: AsyncObjectSource>(src: &S, r: ObjRef) -> Object {
                src.resolve(&Object::Ref(r)).await.unwrap()
            }
            inner(src, r).await
        }

        let got = block_on_ready(depth_three(&src, ObjRef { num: 1, gen: 0 }));
        assert!(
            !matches!(got, Object::Null),
            "object 1 of a simple document must resolve to something"
        );
    }
}
