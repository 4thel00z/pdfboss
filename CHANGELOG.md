# Changelog

## [0.2.1](https://github.com/4thel00z/pdfboss/compare/v0.2.0...v0.2.1) (2026-07-16)


### Performance Improvements

* **core:** add an FxHash-based FastMap; use for Dict, caches, and xref ([e8adb40](https://github.com/4thel00z/pdfboss/commit/e8adb40ab55a8f123cfd5cbf4a28381b23c48aaa))
* **render:** cache flattened glyph outlines, not just parsed ones ([7a2654e](https://github.com/4thel00z/pdfboss/commit/7a2654e82fdc7b99bb86b17af1be9ce5f5e4253d))
* **render:** drop per-curve alloc and finish-clone in the path flattener ([16950ce](https://github.com/4thel00z/pdfboss/commit/16950ce8008112c567401f6499b85728af5ca879))
* **render:** memoize glyph outlines per gid ([7c13df6](https://github.com/4thel00z/pdfboss/commit/7c13df6fdfe77f937816a3984cefc5feb04ab599))
* **render:** route glyph and font-load maps through the fast hasher ([a9ead58](https://github.com/4thel00z/pdfboss/commit/a9ead58b101db1ba56b5f61c0a2b892da5a0b97f))

## [0.2.0](https://github.com/4thel00z/pdfboss/compare/v0.1.0...v0.2.0) (2026-07-15)


### Features

* **cli:** add --fonts tier flag to render command ([a4034ea](https://github.com/4thel00z/pdfboss/commit/a4034ea9936e7bf9908038b939e04f8bedb5d3b2))
* **content:** parse Type3 d0/d1 glyph-metric operators ([2c2d194](https://github.com/4thel00z/pdfboss/commit/2c2d19457388675e9f524da146087db03c1dc8a2))
* **encoding:** standard-14 AFM advance width tables ([6d5775d](https://github.com/4thel00z/pdfboss/commit/6d5775d5489ece4289d6eba05cb6fe93d9169d3a))
* **py:** add fonts= tier parameter to Page.render ([6a1319c](https://github.com/4thel00z/pdfboss/commit/6a1319c386c7bf8ff6f56601bb56e4a04b8905ae))
* **python:** discover pdfboss-fonts for fonts=full; font_dir override ([be518f3](https://github.com/4thel00z/pdfboss/commit/be518f3b7fc17a257a9b2c018403e9b692a1f005))
* **python:** pdfboss-fonts data package with the OFL substitute faces ([d2d02be](https://github.com/4thel00z/pdfboss/commit/d2d02beb8bda0bd8666b6b881cd9b815dac5dae5))
* **python:** pdfboss[full] extra + pdfboss-fonts release pipeline ([b60f704](https://github.com/4thel00z/pdfboss/commit/b60f704c30305267b64e52841cb4e05b7f8cb732))
* **render:** add GlyphPainting tier and RenderOptions gate ([eef8b17](https://github.com/4thel00z/pdfboss/commit/eef8b17d929c921bb67d6d13e7a949490cab580f))
* **render:** advance glyphs by the PDF /Widths, program advance as fallback ([9711bfd](https://github.com/4thel00z/pdfboss/commit/9711bfd904dbeb2902393bdcb5f91b0ec619a7dc))
* **render:** bundle OFL substitute faces behind the substitute-fonts feature ([0518969](https://github.com/4thel00z/pdfboss/commit/0518969b0360c798c14686ee063051088382b868))
* **render:** decrypt and segment Type1 FontFile programs ([c176f34](https://github.com/4thel00z/pdfboss/commit/c176f3428a62090f0bf68f717828922e91d5e99e))
* **render:** honor built-in StandardEncoding for embedded Type1 fonts ([23473d3](https://github.com/4thel00z/pdfboss/commit/23473d3d14ac6a7cdb73fc7149e42c00a7b544e4))
* **render:** honor Type3 d0/d1 colored vs uncolored glyphs ([bb98088](https://github.com/4thel00z/pdfboss/commit/bb9808824419268d8c769d18f1918613841b58c9))
* **render:** interpret CFF Type2 charstrings into outlines ([7aa88e4](https://github.com/4thel00z/pdfboss/commit/7aa88e4ea72145a6a43609c81ddd501f699df522))
* **render:** interpret Type1 charstrings into outlines ([dbae39c](https://github.com/4thel00z/pdfboss/commit/dbae39c814c0f071e271e7d715f2892294dd5b71))
* **render:** map simple TrueType glyphs via /Encoding and /Differences ([e626893](https://github.com/4thel00z/pdfboss/commit/e62689395c5b523421d8b5c88ad40d947d99d1f2))
* **render:** paint embedded CFF fonts, gated by the AllEmbedded tier ([f1410a2](https://github.com/4thel00z/pdfboss/commit/f1410a273c9d69b807ab9a2c488955a869005928))
* **render:** paint embedded Type1 fonts, gated by the AllEmbedded tier ([601744d](https://github.com/4thel00z/pdfboss/commit/601744d56212be7fb5618c2d0e4290f1bfac9e12))
* **render:** paint Type3 glyphs by re-entering the executor, gated ([7849b1c](https://github.com/4thel00z/pdfboss/commit/7849b1c80496d1be74861f059c6dca392b7fc0f5))
* **render:** parse the CFF container (INDEX/DICT/charset) ([9c1b047](https://github.com/4thel00z/pdfboss/commit/9c1b0472c0ee9d1a2159499c34810c9f1c2bb95d))
* **render:** parse the post table for glyph-name lookup ([dcf77d7](https://github.com/4thel00z/pdfboss/commit/dcf77d7997b5ac5a24be50bb877e1167cca0715c))
* **render:** parse Type1 FontMatrix, Encoding, Subrs, CharStrings ([59f1833](https://github.com/4thel00z/pdfboss/commit/59f18339c02cedab2a25ee0fcf0e51065c584504))
* **render:** parse Type3 font dicts (CharProcs, FontMatrix, widths) ([bb22596](https://github.com/4thel00z/pdfboss/commit/bb22596250b7b870d3abf9a4c8a66a19e9b254ad))
* **render:** substitute non-embedded fonts at Full, AFM-14 advances ([4b03a79](https://github.com/4thel00z/pdfboss/commit/4b03a79c62d1e638e529ee5ab32af8de4a6f8b26))
* **render:** substitute-source option, provider trait, face request ([65d4a1d](https://github.com/4thel00z/pdfboss/commit/65d4a1def2addd1eac9cc1bc17d078a48fd5948d))


### Bug Fixes

* **encoding:** reject non-Core-14 siblings in standard-14 width lookup ([e8e8056](https://github.com/4thel00z/pdfboss/commit/e8e8056b97840dc1db32f1480b91e892e09cb02f))
* **render:** bound callothersubr passthrough; flex open-guard; 255 test ([117f27e](https://github.com/4thel00z/pdfboss/commit/117f27e8042bfe5e1fa11fc641d70f2df5b0807d))
* **render:** cap aggregate CID /W expansion; correct tier-test comment ([46bfe3b](https://github.com/4thel00z/pdfboss/commit/46bfe3b4c9ea4ab34bc3a55d47cd764652d1767d))
* **render:** consume a single eexec separator; leniency tests ([31334e4](https://github.com/4thel00z/pdfboss/commit/31334e4c80ea8278522946252251c15b7d8b0501))
* **render:** paint bare-encoding standard-14; correct NOTICE; Symbol prefix; CLI feature ([53e09af](https://github.com/4thel00z/pdfboss/commit/53e09af67daeac0da48c77bb8e4a63c387f6b571))
* **render:** saturate /Differences code increment and document load_simple tiers ([f4aa144](https://github.com/4thel00z/pdfboss/commit/f4aa1441cebe00ebf6f08574b906e3fb3dcb2530))
* **render:** scope substitution to simple fonts; preserve Type3/Type0 ([985368c](https://github.com/4thel00z/pdfboss/commit/985368ccdc0e4edfa961200de35ad4fcd3bfb34a))


### Documentation

* **render:** design spec for full glyph painting ([6fb3476](https://github.com/4thel00z/pdfboss/commit/6fb34762b5e79c5e988820d1613379ec8a72ea95))

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
