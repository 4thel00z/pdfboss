//! The object-source abstraction shared by the synchronous and asynchronous
//! APIs.
//!
//! `pdfboss` keeps exactly one implementation of every algorithm that reads a
//! document — text extraction, rasterization — and two ways of delivering
//! bytes to it. [`ObjectSource`] is what a caller holding the whole file
//! provides. [`AsyncObjectSource`] is what a caller streaming from a file or
//! a network provides. Algorithms are written once against the asynchronous
//! trait and reach synchronous callers through [`Immediate`] and
//! [`block_on`].
//!
//! [`block_on`] is a complete single-threaded driver: it polls, and on
//! [`Poll::Pending`] parks the calling thread until a waker unparks it. A
//! future built over [`Immediate`] resolves every leaf against a
//! [`std::future::Ready`] and so completes on its first poll, never reaching
//! the parking path — but that is an optimisation, not a precondition. Any
//! future is driven correctly.
//!
//! Nothing here needs an async runtime: `Future`, `Pin`, `Box`, `Arc`,
//! `Wake` and `std::future::ready` are all in the standard library, so
//! `pdfboss-core` stays free of executor dependencies.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use crate::error::Result;
use crate::object::{ObjRef, Object, Stream};

/// Reference-chase depth limit for the provided [`ObjectSource::resolve`] and
/// for [`resolve_with`], matching `Document::resolve`.
const MAX_RESOLVE_DEPTH: usize = 32;

/// A boxed future, as returned by every [`AsyncObjectSource`] method.
///
/// Boxing keeps the trait object-safe (`dyn AsyncObjectSource` is usable) at
/// the cost of one allocation per object fetch. Fetches are per *resource* —
/// a font, an image, a form — never per glyph or per pixel, and the hot
/// non-fetching helpers keep taking already-loaded `&Object`, so the
/// allocation does not enter the rasterizer's inner loops.
///
/// `Send` is required so that a future built over one of these can cross
/// `tokio::spawn` and reach the Python bindings, which need `'static + Send`
/// streams.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
    ///
    /// There is no default body. The shared chasing loop lives in
    /// [`resolve_with`], which needs `Self: Sync` because it holds `&self`
    /// across an await; a `where` clause on a provided method would bind
    /// overriding implementors too, and [`Immediate`] wraps sources that are
    /// deliberately not `Sync`. An implementor that *is* `Sync` should
    /// delegate to [`resolve_with`].
    fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>>;
}

/// Chases reference chains against an asynchronous source, depth-capped at
/// `MAX_RESOLVE_DEPTH`.
///
/// Lenient in the same way as [`ObjectSource::resolve`]: a reference to a
/// missing or unreadable object resolves to [`Object::Null`]. An implementor
/// that is `Sync` can satisfy [`AsyncObjectSource::resolve`] by delegating
/// here; one that is not — such as [`Immediate`] over a source with
/// thread-local interior state — supplies its own.
///
/// # Errors
///
/// Returns [`crate::error::Error::CircularReference`] when the chain exceeds
/// `MAX_RESOLVE_DEPTH` hops, or when the underlying source reports one.
pub async fn resolve_with<S>(src: &S, o: &Object) -> Result<Object>
where
    S: AsyncObjectSource + Sync + ?Sized,
{
    let mut current = o.clone();
    let mut last_num = 0;
    for _ in 0..MAX_RESOLVE_DEPTH {
        match current {
            Object::Ref(r) => {
                last_num = r.num;
                current = match src.get(r).await {
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

/// Presents a synchronous [`ObjectSource`] as an [`AsyncObjectSource`] whose
/// futures are already complete.
///
/// Wrapping a source in `Immediate` is what lets the synchronous entry points
/// share the asynchronous implementation. Because every future this produces
/// is [`std::future::Ready`], a future tree built over it completes on its
/// first poll and never parks — see [`block_on`].
///
/// Note the divergence from a genuinely asynchronous source: these futures do
/// their work eagerly, when the method is called, rather than lazily on first
/// poll. Constructing one and dropping it unpolled still performs the read.
/// That is invisible to an algorithm that awaits what it constructs, but it
/// means `Immediate` is not a timing-faithful stand-in for a streaming source.
#[derive(Debug, Clone, Copy)]
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

/// Unparks the thread blocked inside [`block_on`].
struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Runs `future` to completion on the current thread.
///
/// This is how the synchronous entry points run the shared asynchronous
/// implementation. A future built over [`Immediate`] resolves every leaf
/// against a [`std::future::Ready`], so it completes on its first poll and
/// the parking path below is never entered; any other future is driven
/// correctly rather than panicking.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            // `park` may return spuriously, so re-poll rather than assume a
            // wake means readiness.
            Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use crate::document::Document;
    use crate::error::{Error, Result};
    use crate::object::{ObjRef, Object, Stream};
    use crate::source::{
        block_on, resolve_with, AsyncObjectSource, BoxFuture, Immediate, ObjectSource,
    };

    /// Fetches the fixture's page content stream, which `simple_doc` writes as
    /// object 4.
    fn content_stream(doc: &Document) -> Stream {
        match ObjectSource::get(doc, ObjRef { num: 4, gen: 0 }).unwrap() {
            Object::Stream(s) => s,
            other => panic!("expected object 4 to be the content stream, got {other:?}"),
        }
    }

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
        let actual = block_on(src.get(r)).unwrap();

        assert_eq!(actual, expected);
    }

    /// `Immediate`'s `resolve` override delegates to the wrapped source, so it
    /// must reproduce `Document::resolve`'s leniency: a dangling reference
    /// becomes Null, not an error.
    #[test]
    fn immediate_resolve_is_lenient_about_missing_targets() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let missing = Object::Ref(ObjRef { num: 9_999, gen: 0 });

        let src = Immediate(&doc);
        assert_eq!(block_on(src.resolve(&missing)).unwrap(), Object::Null);
    }

    /// Decoding a stream through Immediate must agree with decoding it
    /// synchronously.
    #[test]
    fn immediate_stream_data_matches_the_sync_source() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let stream = content_stream(&doc);
        let expected = ObjectSource::stream_data(&doc, &stream).unwrap();

        let src = Immediate(&doc);
        let actual = block_on(src.stream_data(&stream)).unwrap();

        assert_eq!(actual, expected);
        assert!(
            !actual.is_empty(),
            "the fixture's content stream must decode to something"
        );
    }

    /// `block_on` completes a nested future tree — several awaits deep, which
    /// is the shape the shared algorithms produce.
    #[test]
    fn block_on_drives_a_nested_future_tree() {
        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let src = Immediate(&doc);

        async fn depth_three<S: AsyncObjectSource>(src: &S, r: ObjRef) -> Object {
            async fn inner<S: AsyncObjectSource>(src: &S, r: ObjRef) -> Object {
                src.resolve(&Object::Ref(r)).await.unwrap()
            }
            inner(src, r).await
        }

        let got = block_on(depth_three(&src, ObjRef { num: 1, gen: 0 }));
        assert!(
            !matches!(got, Object::Null),
            "object 1 of a simple document must resolve to something"
        );
    }

    /// A source whose every object is a reference to itself, so chasing a
    /// chain never terminates and must hit the depth cap. Reaching the cap is
    /// unreachable through `Immediate`, whose `resolve` delegates to the
    /// wrapped source, so this stub is what exercises `resolve_with`'s loop.
    struct SelfReferential;

    impl AsyncObjectSource for SelfReferential {
        fn get(&self, r: ObjRef) -> BoxFuture<'_, Result<Object>> {
            Box::pin(std::future::ready(Ok(Object::Ref(r))))
        }

        fn stream_data<'a>(&'a self, s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>> {
            Box::pin(std::future::ready(Ok(s.data.clone())))
        }

        fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
            Box::pin(resolve_with(self, o))
        }
    }

    /// `resolve_with` must stop at the depth cap and name the last reference
    /// it followed, rather than looping forever.
    #[test]
    fn resolve_with_stops_at_the_depth_cap() {
        let chain = Object::Ref(ObjRef { num: 7, gen: 0 });
        let err = block_on(resolve_with(&SelfReferential, &chain)).unwrap_err();
        assert!(
            matches!(err, Error::CircularReference(7)),
            "a self-referential chain must exhaust the cap and report \
             CircularReference for the last reference seen, got {err:?}"
        );
    }

    /// A non-reference passes straight through `resolve_with` without a fetch.
    #[test]
    fn resolve_with_returns_a_direct_object_unchanged() {
        let direct = Object::Int(42);
        assert_eq!(
            block_on(resolve_with(&SelfReferential, &direct)).unwrap(),
            Object::Int(42)
        );
    }

    /// The parking path must actually resume. This future returns Pending
    /// once — waking itself first, so the unpark token is already set and the
    /// test cannot deadlock — then Ready.
    #[test]
    fn block_on_resumes_after_parking() {
        struct YieldOnce {
            yielded: bool,
        }

        impl Future for YieldOnce {
            type Output = u32;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
                if self.yielded {
                    Poll::Ready(7)
                } else {
                    self.yielded = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }

        assert_eq!(
            block_on(YieldOnce { yielded: false }),
            7,
            "block_on must re-poll after parking rather than panicking or hanging"
        );
    }

    /// `BoxFuture` promises `Send`, which the asynchronous API depends on to
    /// cross `tokio::spawn` and reach the Python bindings. Pin that here: the
    /// futures must be `Send` even though `Document` — with its `Rc`/`RefCell`
    /// caches — is itself neither `Send` nor `Sync`.
    #[test]
    fn immediate_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}

        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        let stream = content_stream(&doc);
        let src = Immediate(&doc);
        let object = Object::Ref(ObjRef { num: 1, gen: 0 });

        assert_send(&src.get(ObjRef { num: 1, gen: 0 }));
        assert_send(&src.stream_data(&stream));
        assert_send(&src.resolve(&object));
    }

    /// `BoxFuture`'s documentation claims `dyn AsyncObjectSource` is usable.
    /// Hold the trait to it, so a later signature change cannot quietly break
    /// object-safety.
    #[test]
    fn async_object_source_is_object_safe() {
        fn assert_dyn(_: &dyn AsyncObjectSource) {}

        let doc = Document::load(pdfboss_testkit::simple_doc("Hello")).unwrap();
        assert_dyn(&Immediate(&doc));
        assert_dyn(&SelfReferential);
    }
}
