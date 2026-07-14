# Changelog

## 0.1.0 (2026-07-14)


### Features

* **core:** decrypt AES (AESV2 and AESV3) Standard-handler files ([c8529ee](https://github.com/4thel00z/pdfboss/commit/c8529ee614339a7dff0f704058f4970a7aefc3a4))
* **core:** decrypt Standard-handler RC4 files (empty user password) ([253be12](https://github.com/4thel00z/pdfboss/commit/253be124a6c3735fc5854b0527e715304abb8bd8))
* initial pdfboss release — clean-room PDF toolkit in Rust ([42a46db](https://github.com/4thel00z/pdfboss/commit/42a46db0468ba9682067b13e1e39fb97ac129c7a))
* **render:** paint embedded TrueType glyph outlines ([161aaad](https://github.com/4thel00z/pdfboss/commit/161aaad54c0e0ab29752a4d152178ec6262adaa3))
* **render:** TrueType glyf outline parser ([ec2f9b0](https://github.com/4thel00z/pdfboss/commit/ec2f9b0a48dc176a974fee8362a19a30161b98a9))


### Performance Improvements

* **core:** allocation-free lexing of well-formed numbers and names ([675fa81](https://github.com/4thel00z/pdfboss/commit/675fa811c95109723ec9027b36a11c596fc09d0e))
* **core:** apply the TIFF predictor in place on owned data ([a589a2d](https://github.com/4thel00z/pdfboss/commit/a589a2dfdd04904808ebc076433417030a760257))
* **core:** cache decoded object streams and parse their header once ([1cfdabd](https://github.com/4thel00z/pdfboss/commit/1cfdabdefdb7e929b450a9407a39d098113a22bb))
* **core:** lazy page-tree loading with cheap page_count ([99e8f4e](https://github.com/4thel00z/pdfboss/commit/99e8f4ec6c81a819b699657d184a314ac197c248))
* **core:** use the zlib-rs FlateDecode backend ([14b03df](https://github.com/4thel00z/pdfboss/commit/14b03dfc01dbdfbafd5111755d04f2d2497fec00))
* enable thin LTO and codegen-units=1 for release builds ([30b8f69](https://github.com/4thel00z/pdfboss/commit/30b8f6931f2298cd00232d44f74280f063fd2b69))
* **render:** active-edge table + row-extent-bounded fill ([ab89290](https://github.com/4thel00z/pdfboss/commit/ab89290ef3ea2f6fb7c0036ef8b4594d5b839485))
* **render:** share clip mask behind Rc (clone-on-write) ([d99f5d1](https://github.com/4thel00z/pdfboss/commit/d99f5d14981a5ab234af15fb6479f82b12945966))
* **text:** decode glyphs without a per-glyph String allocation ([cfc4bcc](https://github.com/4thel00z/pdfboss/commit/cfc4bcce7ffd4d5d27c605f437ad98ab9d4051b7))


### Documentation

* note AES encryption support in the performance spec ([c3a98a6](https://github.com/4thel00z/pdfboss/commit/c3a98a6e2ed2f5bde5f4f6026c07df019597a727))
* note embedded-TrueType glyph painting support ([2c19230](https://github.com/4thel00z/pdfboss/commit/2c19230f1aff5f2ce61762ca8d961c20c3938825))
* note RC4 encryption support in the performance spec ([19bb5d3](https://github.com/4thel00z/pdfboss/commit/19bb5d3a310c318d4af52028784a2b5dbea0e0f8))
* record performance results and deferred work in the spec ([4d4e1ee](https://github.com/4thel00z/pdfboss/commit/4d4e1ee94afb760fcdd033b6d94544381dfb3704))
