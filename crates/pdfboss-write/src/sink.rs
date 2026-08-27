//! The byte-sink abstraction shared by the synchronous and asynchronous
//! write APIs.
//!
//! `pdfboss-write` keeps exactly one implementation of file emission —
//! the algorithm behind [`crate::Writer::finish_into_with`] — and several
//! ways of accepting its bytes. [`AsyncByteSink`] is what an asynchronous
//! consumer provides; `Vec<u8>` is a sink in its own right for the
//! in-memory path; [`Immediate`] presents any [`std::io::Write`] as a sink
//! whose futures are already complete, which is how the synchronous entry
//! points share the asynchronous implementation through
//! [`pdfboss_core::block_on`] — exactly the pattern the read side's
//! `pdfboss_core::source` module documents.
//!
//! Emission follows that module's three signing rules, mirrored for
//! writing: entry points take the sink by value, carry no `Send`/`Sync`
//! bounds of their own, and call the trait method rather than a free twin.

use std::io::Write;

use pdfboss_core::source::BoxFuture;

use crate::error::{Error, Result};

/// Accepts emitted file bytes, awaiting whatever I/O that takes.
///
/// This is the trait the shared emission algorithm is written against.
/// [`BoxFuture`] is `Send`-bounded, so an implementation over a non-`Send`
/// writer must do its work eagerly and return an already-complete future —
/// as [`Immediate`] does — rather than capture the writer.
pub trait AsyncByteSink {
    /// Writes all of `buf`, erroring if any byte cannot be accepted.
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, Result<()>>;
}

/// An exclusive reference to a sink is itself a sink, forwarding the
/// write — the write-side counterpart of `pdfboss_core::source`'s `&T`
/// impl, and what lets a caller keep one sink across several by-value
/// entry-point calls.
impl<S: AsyncByteSink + ?Sized> AsyncByteSink for &mut S {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        (**self).write_all(buf)
    }
}

/// The in-memory sink: bytes accumulate in the vector and the returned
/// future is already complete.
impl AsyncByteSink for Vec<u8> {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        self.extend_from_slice(buf);
        Box::pin(std::future::ready(Ok(())))
    }
}

/// Presents a synchronous [`std::io::Write`] as an [`AsyncByteSink`]
/// whose futures are already complete.
///
/// The write happens eagerly, when the method is called; the returned
/// future merely reports its result. That keeps the future free of any
/// borrow of the writer — so it is `Send` whatever the writer is — and a
/// future tree built over this type completes on its first poll, never
/// parking inside [`pdfboss_core::block_on`]. No flush is ever performed:
/// the writer comes back (or drops) exactly as buffered.
#[derive(Debug, Clone)]
pub struct Immediate<W>(pub W);

impl<W: Write> AsyncByteSink for Immediate<W> {
    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        let outcome = self.0.write_all(buf).map_err(Error::from);
        Box::pin(std::future::ready(outcome))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    use pdfboss_core::block_on;

    use super::{AsyncByteSink, Immediate};

    #[test]
    fn a_vec_is_a_sink() {
        let mut sink = Vec::new();
        block_on(AsyncByteSink::write_all(&mut sink, b"abc")).expect("a Vec accepts everything");
        block_on(AsyncByteSink::write_all(&mut sink, b"def")).expect("a Vec accepts everything");
        assert_eq!(sink, b"abcdef");
    }

    #[test]
    fn a_reference_to_a_sink_is_a_sink() {
        fn feed<S: AsyncByteSink>(mut sink: S) -> S {
            block_on(sink.write_all(b"xy")).expect("the test sinks accept everything");
            sink
        }

        let mut sink = Vec::new();
        feed(&mut sink);
        let sink = feed(sink);
        assert_eq!(sink, b"xyxy");
    }

    #[test]
    fn immediate_writes_through_to_the_writer() {
        let mut sink = Immediate(Vec::new());
        block_on(sink.write_all(b"hello")).expect("a Vec accepts everything");
        assert_eq!(sink.0, b"hello");
    }

    /// A writer that shares state through `Rc` — deliberately not `Send` —
    /// so this pins the design point: `Immediate`'s eager write keeps the
    /// future `Send` without capturing the writer.
    struct SharedWriter(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn immediate_futures_are_send_even_over_a_non_send_writer() {
        fn assert_send<T: Send>(_: &T) {}

        let shared = Rc::new(RefCell::new(Vec::new()));
        let mut sink = Immediate(SharedWriter(Rc::clone(&shared)));
        let future = sink.write_all(b"eager");
        assert_send(&future);
        block_on(future).expect("the shared writer accepts everything");
        assert_eq!(*shared.borrow(), b"eager");
    }

    /// The write happens when the method is called, not when the future is
    /// polled — the same eagerness divergence `pdfboss_core::source`
    /// documents for its `Immediate`.
    #[test]
    fn immediate_writes_eagerly() {
        let shared = Rc::new(RefCell::new(Vec::new()));
        let mut sink = Immediate(SharedWriter(Rc::clone(&shared)));
        let unpolled = sink.write_all(b"already there");
        assert_eq!(*shared.borrow(), b"already there");
        drop(unpolled);
    }

    /// A writer that refuses everything, so the error path is covered.
    struct Refusing;

    impl Write for Refusing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("refused"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn immediate_surfaces_write_errors() {
        let mut sink = Immediate(Refusing);
        let err = block_on(sink.write_all(b"x")).unwrap_err();
        assert!(matches!(err, crate::error::Error::Io(_)));
    }
}
