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
//! future whose wakeup does not depend on an external reactor is driven
//! correctly; one that does will park forever, so see [`block_on`]'s own
//! documentation before handing it a future from elsewhere.
//!
//! Nothing here needs an async runtime: `Future`, `Pin`, `Box`, `Arc`,
//! `Wake` and `std::future::ready` are all in the standard library, so
//! `pdfboss-core` stays free of executor dependencies.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use crate::error::Result;
use crate::object::{ObjRef, Object, Stream};

/// Reference-chase depth limit for every reference chase in the crate:
/// [`resolve_sync_with`], [`resolve_with`], the provided
/// [`ObjectSource::resolve`] and [`crate::document::Document::resolve`].
///
/// This is a denial-of-service guard — a file can encode a reference chain of
/// any length, and a malicious one can encode a cycle — so it is deliberately
/// one definition. It is public because the cap is part of the observable
/// contract: a caller that hands `pdfboss` a legitimately deep chain needs to
/// know where the crate stops chasing and reports
/// [`crate::error::Error::CircularReference`] instead.
pub const MAX_RESOLVE_DEPTH: usize = 32;

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
/// Implement `get` and `stream_data`; `resolve` has a provided implementation
/// that chases reference chains through `get` via [`resolve_sync_with`]. An
/// implementor should override `resolve` only if its chasing genuinely
/// differs; one that just wants the shared chase — as `Document` does — needs
/// nothing.
pub trait ObjectSource {
    /// Fetches an indirect object by reference.
    fn get(&self, r: ObjRef) -> Result<Object>;

    /// Decodes a stream's data through its filter chain, resolving indirect
    /// filter parameters against this source.
    fn stream_data(&self, s: &Stream) -> Result<Vec<u8>>;

    /// Chases reference chains, depth-capped at [`MAX_RESOLVE_DEPTH`].
    ///
    /// Lenient: a reference to a missing or unreadable object resolves to
    /// [`Object::Null`]. Exceeding the depth cap is
    /// [`crate::error::Error::CircularReference`].
    ///
    /// The loop itself lives in [`resolve_sync_with`], which this delegates
    /// to; an implementor overriding this method should delegate there too
    /// unless its chasing genuinely differs.
    fn resolve(&self, o: &Object) -> Result<Object> {
        resolve_sync_with(self, o)
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

    /// Chases reference chains, depth-capped at [`MAX_RESOLVE_DEPTH`].
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

/// Chases reference chains against a synchronous source, depth-capped at
/// [`MAX_RESOLVE_DEPTH`].
///
/// This is the one canonical synchronous chase: the provided
/// [`ObjectSource::resolve`] and [`crate::document::Document::resolve`] both
/// delegate here, so their leniency and their error reporting cannot drift
/// apart. [`resolve_with`] is the same algorithm awaiting its fetches.
///
/// Lenient: a reference whose target is missing or unreadable resolves to
/// [`Object::Null`], because a real-world file routinely dangles a reference
/// into nothing and ISO 32000 makes such a reference equivalent to `null`.
/// A non-reference is returned unchanged without a fetch.
///
/// # Errors
///
/// Returns [`crate::error::Error::CircularReference`] when the chain exceeds
/// `MAX_RESOLVE_DEPTH` hops — naming the last reference followed — or when
/// the underlying source reports one, which is propagated unchanged rather
/// than flattened to `Null`.
pub fn resolve_sync_with<S>(src: &S, o: &Object) -> Result<Object>
where
    S: ObjectSource + ?Sized,
{
    let mut current = o.clone();
    let mut last_num = 0;
    for _ in 0..MAX_RESOLVE_DEPTH {
        match current {
            Object::Ref(r) => {
                last_num = r.num;
                current = match src.get(r) {
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

/// Chases reference chains against an asynchronous source, depth-capped at
/// [`MAX_RESOLVE_DEPTH`].
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

/// Unparks the thread blocked inside one particular [`block_on`] call.
///
/// `notified` is what makes the wakeup belong to that call rather than to the
/// thread at large. Unpark tokens are per-thread and not counted: a single
/// token satisfies the next `park` whoever set it, so without a per-call flag
/// a nested `block_on` would consume the outer call's token and leave the
/// outer call parked forever.
///
/// A stray token in the other direction is accepted deliberately. Because
/// [`block_on`] must test this flag before parking to survive a nested call
/// stealing the thread's single token, a wake that arrives during a poll can
/// leave its token unconsumed, and a `Waker` clone outliving its call can set
/// one afterwards. Either way a later unrelated `park` on this thread may
/// return spuriously — which the standard library permits, and which is
/// strictly better than the hang the opposite order produces. [`block_on`]'s
/// `Poll::Ready` arm drains the token in the common case; the two remaining
/// paths are documented there and cannot be closed from inside the call.
struct ThreadWaker {
    thread: std::thread::Thread,
    notified: AtomicBool,
}

impl ThreadWaker {
    /// Records the wakeup and unparks, but only on the `false -> true`
    /// transition, so however many times a future wakes this waker at most
    /// one unpark token is outstanding.
    fn notify(&self) {
        // `swap` rather than a load-then-store: the read-modify-write is what
        // makes the transition test atomic, so two concurrent wakers cannot
        // both observe `false` and both unpark.
        //
        // `Release` publishes everything the waking side wrote before waking
        // — the state that made the future ready. It pairs with the `Acquire`
        // swap in `block_on`'s park loop below, giving the driver a
        // happens-before edge to that state before it re-polls. `park`/
        // `unpark` synchronize on their own, but the flag is what the loop
        // actually trusts to decide it may stop waiting, so the edge has to
        // exist on the flag too. Nothing needs to be acquired *from* the flag
        // here, so the read half stays relaxed.
        if !self.notified.swap(true, Ordering::Release) {
            self.thread.unpark();
        }
    }
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.notify();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.notify();
    }
}

/// Runs `future` to completion on the current thread.
///
/// This is how the synchronous entry points run the shared asynchronous
/// implementation. A future built over [`Immediate`] resolves every leaf
/// against a [`std::future::Ready`], so it completes on its first poll and
/// the parking path below is never entered.
///
/// Any other future is driven correctly *provided something eventually wakes
/// it*. This is a bare driver, not a runtime: it owns no reactor, no timer
/// wheel and no I/O registration. A future that returns
/// [`Poll::Pending`] while waiting on a wakeup that only an external reactor
/// could deliver — a socket becoming readable, a timer firing — **blocks this
/// thread forever**. Such a future belongs on the runtime that owns its
/// reactor; pass it to that runtime's own driver instead.
///
/// Nesting is safe: an inner `block_on` on the same thread cannot steal the
/// outer call's wakeup, because each call waits on a flag private to itself
/// rather than on the thread's shared unpark token.
pub fn block_on<F: Future>(future: F) -> F::Output {
    // The `Arc` is kept alongside the `Waker` so the loop can read and clear
    // the flag the waker sets; `Waker::from` would otherwise consume it.
    let waker_state = Arc::new(ThreadWaker {
        thread: std::thread::current(),
        notified: AtomicBool::new(false),
    });
    let waker = Waker::from(Arc::clone(&waker_state));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => {
                // A future may legally wake and then complete in the same
                // poll, which leaves the flag set and an unpark token
                // outstanding that no `park` above ever consumed. Drain it,
                // so a completed call cannot make an unrelated later `park`
                // on this thread return spuriously. `park_timeout` with a
                // zero duration consumes a waiting token and returns at once
                // either way, so this never blocks.
                if waker_state.notified.swap(false, Ordering::Acquire) {
                    std::thread::park_timeout(std::time::Duration::ZERO);
                }
                return value;
            }
            // Wait for *this* call's wakeup, testing the flag BEFORE parking.
            // That order is load-bearing and the alternative deadlocks.
            //
            // An unpark token is a single per-thread boolean, not a count. So
            // when this future woke during its own poll and then, still inside
            // that poll, drove a nested `block_on` whose future also parked,
            // the nested call consumed the one token — including the share
            // this call was relying on. Our flag is set but no token remains.
            // Parking first would block forever;
            // `nested_block_on_does_not_strand_the_outer_call` is that
            // deadlock, and it fails within its 30 s budget if this is
            // reordered.
            //
            // The cost of testing first is that when nobody stole the token it
            // goes unconsumed, so a later unrelated `park` on this thread can
            // return spuriously. That is permitted by the standard library and
            // is strictly preferable to a hang; the `Poll::Ready` arm drains
            // the token in the common case anyway.
            //
            // Looping absorbs a spurious `park` return: the flag is still
            // false, so it parks again. `Acquire` pairs with the `Release` in
            // `ThreadWaker::notify`.
            Poll::Pending => {
                while !waker_state.notified.swap(false, Ordering::Acquire) {
                    std::thread::park();
                }
            }
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
        block_on, resolve_sync_with, resolve_with, AsyncObjectSource, BoxFuture, Immediate,
        ObjectSource, MAX_RESOLVE_DEPTH,
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

    /// The synchronous mirror of `SelfReferential`. Reaching the cap is
    /// unreachable through a real `Document`, whose own re-entrancy guard
    /// fires first, so this stub is what exercises `resolve_sync_with`'s loop.
    struct SyncSelfReferential;

    impl ObjectSource for SyncSelfReferential {
        fn get(&self, r: ObjRef) -> Result<Object> {
            Ok(Object::Ref(r))
        }

        fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
            Ok(s.data.clone())
        }
    }

    /// Reports a cycle for every fetch, so the propagating branch is covered.
    struct CircularSource;

    impl ObjectSource for CircularSource {
        fn get(&self, r: ObjRef) -> Result<Object> {
            Err(Error::CircularReference(r.num))
        }

        fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
            Ok(s.data.clone())
        }
    }

    /// Fails every fetch for a reason that is *not* a cycle, so the lenient
    /// branch is covered.
    struct UnreadableSource;

    impl ObjectSource for UnreadableSource {
        fn get(&self, _: ObjRef) -> Result<Object> {
            Err(Error::MissingKey("Length"))
        }

        fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
            Ok(s.data.clone())
        }
    }

    /// A finite chain: object `n` refers to object `n - 1`, and object 0 is a
    /// direct integer. Chasing from `Ref(k)` therefore terminates after a
    /// known number of hops, which is what lets the cap be measured.
    struct Chain;

    impl ObjectSource for Chain {
        fn get(&self, r: ObjRef) -> Result<Object> {
            if r.num == 0 {
                Ok(Object::Int(0))
            } else {
                Ok(Object::Ref(ObjRef {
                    num: r.num - 1,
                    gen: 0,
                }))
            }
        }

        fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
            Ok(s.data.clone())
        }
    }

    /// `resolve_sync_with` must stop at the depth cap and name the last
    /// reference it followed, rather than looping forever.
    #[test]
    fn resolve_sync_with_stops_at_the_depth_cap() {
        let chain = Object::Ref(ObjRef { num: 7, gen: 0 });
        let err = resolve_sync_with(&SyncSelfReferential, &chain).unwrap_err();
        assert!(
            matches!(err, Error::CircularReference(7)),
            "a self-referential chain must exhaust the cap and report \
             CircularReference for the last reference seen, got {err:?}"
        );
    }

    /// A non-reference passes straight through `resolve_sync_with` without a
    /// fetch.
    #[test]
    fn resolve_sync_with_returns_a_direct_object_unchanged() {
        assert_eq!(
            resolve_sync_with(&SyncSelfReferential, &Object::Int(42)).unwrap(),
            Object::Int(42)
        );
    }

    /// The two halves of the shared chase's error handling, both load-bearing
    /// for `Document::resolve`: an unreadable target becomes `Null`, while a
    /// cycle reported by the source propagates unchanged.
    #[test]
    fn resolve_sync_with_is_lenient_but_propagates_cycles() {
        let r = Object::Ref(ObjRef { num: 5, gen: 0 });
        assert_eq!(
            resolve_sync_with(&UnreadableSource, &r).unwrap(),
            Object::Null,
            "a fetch failing for any reason other than a cycle must flatten \
             to Null"
        );
        assert!(matches!(
            resolve_sync_with(&CircularSource, &r).unwrap_err(),
            Error::CircularReference(5)
        ));
    }

    /// The provided `ObjectSource::resolve` must *be* the shared chase, not a
    /// second copy of it: same answer on a direct object and same cap error.
    #[test]
    fn the_provided_resolve_is_the_shared_chase() {
        let direct = Object::Int(9);
        assert_eq!(
            ObjectSource::resolve(&SyncSelfReferential, &direct).unwrap(),
            resolve_sync_with(&SyncSelfReferential, &direct).unwrap()
        );

        let chain = Object::Ref(ObjRef { num: 3, gen: 0 });
        assert!(matches!(
            ObjectSource::resolve(&SyncSelfReferential, &chain).unwrap_err(),
            Error::CircularReference(3)
        ));
    }

    /// The synchronous and asynchronous chases must share one cap, which is
    /// the point of `MAX_RESOLVE_DEPTH` having a single definition. Chasing
    /// the longest chain that fits succeeds both ways; one hop more fails both
    /// ways, with the same reference named.
    #[test]
    fn both_chases_share_one_depth_cap() {
        // From `Ref(k)` the loop follows k + 1 references and spends one more
        // iteration returning the direct object it lands on: k + 2 iterations
        // for a cap of MAX_RESOLVE_DEPTH.
        let longest = u32::try_from(MAX_RESOLVE_DEPTH - 2).expect("the cap fits in a u32");
        let fits = Object::Ref(ObjRef {
            num: longest,
            gen: 0,
        });
        let too_long = Object::Ref(ObjRef {
            num: longest + 1,
            gen: 0,
        });

        assert_eq!(resolve_sync_with(&Chain, &fits).unwrap(), Object::Int(0));
        assert_eq!(
            block_on(resolve_with(&Immediate(&Chain), &fits)).unwrap(),
            Object::Int(0)
        );

        assert!(matches!(
            resolve_sync_with(&Chain, &too_long).unwrap_err(),
            Error::CircularReference(0)
        ));
        assert!(matches!(
            block_on(resolve_with(&Immediate(&Chain), &too_long)).unwrap_err(),
            Error::CircularReference(0)
        ));
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

    /// A nested `block_on` on the same thread must not consume the outer
    /// call's wakeup. The outer future arms its own waker and then, still
    /// inside that same poll, runs a complete nested driver before yielding.
    /// Unpark tokens are per-thread and not counted, so a driver that trusted
    /// `park` alone let the inner call swallow the outer wake and left the
    /// outer call parked forever. Driven on a worker thread with a bounded
    /// wait, so a regression fails this test instead of hanging the suite.
    #[test]
    fn nested_block_on_does_not_strand_the_outer_call() {
        /// Yields once, waking itself first so the nested driver has a wakeup
        /// to observe.
        struct SelfWaking {
            yielded: bool,
        }

        impl Future for SelfWaking {
            type Output = u32;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
                if self.yielded {
                    return Poll::Ready(7);
                }
                self.yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }

        struct Outer {
            polled: bool,
            inner: u32,
        }

        impl Future for Outer {
            type Output = u32;

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
                if self.polled {
                    return Poll::Ready(self.inner);
                }
                self.polled = true;
                cx.waker().wake_by_ref();
                self.inner = block_on(SelfWaking { yielded: false });
                Poll::Pending
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(block_on(Outer {
                polled: false,
                inner: 0,
            }));
        });

        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(inner) => assert_eq!(
                inner, 7,
                "the nested block_on must have run its future to completion"
            ),
            Err(e) => panic!(
                "the outer block_on never finished ({e:?}): the nested call \
                 consumed its wakeup"
            ),
        }
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
