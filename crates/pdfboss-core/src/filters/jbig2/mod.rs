//! JBIG2 (ISO/IEC 14492 / ITU-T T.88) decoding.
//!
//! Implemented from the published standard. The entry point is the
//! `JBIG2Decode` arm of [`crate::filters::decode_stream`], which returns
//! packed 1-bit-per-pixel rows so the image layer can treat the result
//! exactly like any other `/BitsPerComponent 1` `/DeviceGray` sample data.
//!
//! Layering, bottom-up: [`mq`] is the binary arithmetic decoder (Annex E);
//! the integer procedures of Annex A build on top of it.

#![allow(dead_code)] // Consumed by the segment layer, which lands next.

pub(crate) mod mq;
