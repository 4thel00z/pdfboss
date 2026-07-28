# Changelog

## [0.7.2](https://github.com/4thel00z/pdfboss/compare/v0.7.1...v0.7.2) (2026-07-28)


### Bug Fixes

* **text:** read a Windows subset's high codes instead of dropping them ([b66765b](https://github.com/4thel00z/pdfboss/commit/b66765bd57396b7dcb6059ef19085b684571372f))

## [0.7.1](https://github.com/4thel00z/pdfboss/compare/v0.7.0...v0.7.1) (2026-07-28)


### Performance Improvements

* **core:** read a generic region's reference rows as slices ([0dca72e](https://github.com/4thel00z/pdfboss/commit/0dca72e25b22acc766c45be6d37d2418e399cdbd))
* **core:** work a row at a time when composing and packing bitmaps ([5348280](https://github.com/4thel00z/pdfboss/commit/5348280b7d88bff69cec01418234da66574111d9))
* **render:** copy an opaque image pixel instead of blending it ([79671bb](https://github.com/4thel00z/pdfboss/commit/79671bbd0d63fae01afb7a046c8ce211a0b10f4f))
* **render:** sample a one-component image where it is drawn ([ad59df0](https://github.com/4thel00z/pdfboss/commit/ad59df0ad0af52e2732076e30ef27a93942657f5))


### Documentation

* remeasure the scan benchmark after the rendering work ([eb8694f](https://github.com/4thel00z/pdfboss/commit/eb8694f103b07a222d7eac0ff3b78a9d9f36e7d9))

## [0.7.0](https://github.com/4thel00z/pdfboss/compare/v0.6.0...v0.7.0) (2026-07-28)


### Features

* **core:** add the Annex B Huffman table machinery ([6789c50](https://github.com/4thel00z/pdfboss/commit/6789c50fa89a44752cd2f2a675de6c0c805214f8))
* **core:** add the standard Huffman tables ([90627f8](https://github.com/4thel00z/pdfboss/commit/90627f8057da910305481a515d603c4b68571e7d))
* **core:** decode Huffman-coded symbol dictionaries ([a68b0a5](https://github.com/4thel00z/pdfboss/commit/a68b0a5b220d07eb1074a126d5039c99cd12ad25))
* **core:** decode Huffman-coded text regions ([4947f06](https://github.com/4thel00z/pdfboss/commit/4947f06d52c3a6a67d304990be1f72d54a07c165))
* **core:** keep the Huffman tables a segment refers to ([2c79544](https://github.com/4thel00z/pdfboss/commit/2c7954419d49816c3fc070c8e597cd88a80cb313))
* **core:** parse custom Huffman table segments ([935f21a](https://github.com/4thel00z/pdfboss/commit/935f21a8ccd30aec766efc32f3b6197fe0a5bc41))


### Bug Fixes

* **core:** charge a Huffman text region for its symbol ID table ([8c6789f](https://github.com/4thel00z/pdfboss/commit/8c6789fca3365a33eff693b038affc3751204f85))
* **core:** charge for the custom tables a segment's selectors copy ([c9a5b3f](https://github.com/4thel00z/pdfboss/commit/c9a5b3fc828691f6d365455ae00e0bfc6ca0b2b4))
* **core:** charge for the symbol list a segment's referred-to names build ([e0e01cd](https://github.com/4thel00z/pdfboss/commit/e0e01cddf51f80020f525e7f1e2f1706ec39055d))
* **core:** decode a text region that assigns no symbol ID codes ([8311c5d](https://github.com/4thel00z/pdfboss/commit/8311c5d1e782d476e1e3da315e02d87a796cbfff))
* **core:** price a retained Huffman table, not only its lines ([f361927](https://github.com/4thel00z/pdfboss/commit/f361927db2a909fa6af248bfdf7eb1b0c9a8ab0f))


### Documentation

* **core:** correct the segment-syntax clause numbers ([2744597](https://github.com/4thel00z/pdfboss/commit/2744597d542052589ad9b1a50f6187f8ded43779))
* **core:** state the rule the budget's charges follow ([a2b25bd](https://github.com/4thel00z/pdfboss/commit/a2b25bd5108339c1fa9c8da452d723a49085eef8))
* say that Huffman symbol dictionaries decode ([74e446d](https://github.com/4thel00z/pdfboss/commit/74e446da14c5b409a5cc9e1716283b277231524a))
* state what JBIG2Decode covers after Huffman text regions ([7a723ad](https://github.com/4thel00z/pdfboss/commit/7a723add1f919caac952a62d080a36c41b537dc8))

## [0.6.0](https://github.com/4thel00z/pdfboss/compare/v0.5.0...v0.6.0) (2026-07-27)


### Features

* **core:** add an MSB-first bit reader for fax coding ([27ef2ba](https://github.com/4thel00z/pdfboss/commit/27ef2bae67f8f060bfc23cf33f8ad5358c04e058))
* **core:** add JBIG2 arithmetic integer decoding procedures ([ff86342](https://github.com/4thel00z/pdfboss/commit/ff86342d38c88f2b0f3c07cc1a6e69bca34a8e3a))
* **core:** add the JBIG2 bilevel bitmap and composition operators ([c60b56c](https://github.com/4thel00z/pdfboss/commit/c60b56c5829c7dbb0cd37791c279dfb2a153cd91))
* **core:** add the JBIG2 MQ arithmetic decoder ([98e85b8](https://github.com/4thel00z/pdfboss/commit/98e85b8bae71f290342c9228049faf56866c777f))
* **core:** add the T.4 run-length code tables ([36b8dae](https://github.com/4thel00z/pdfboss/commit/36b8daee8b5d8ccbc5fb49b216ef0221761faa4e))
* **core:** assemble JBIG2 pages from embedded segment streams ([0e0feb7](https://github.com/4thel00z/pdfboss/commit/0e0feb7fcf8279a7e780e7b964ba5ae4d2ddd0e0))
* **core:** decode arithmetic JBIG2 symbol dictionaries ([5208910](https://github.com/4thel00z/pdfboss/commit/5208910ad2d77c5fce088e623c6b05f06cb199f7))
* **core:** decode arithmetic JBIG2 text regions ([d12a319](https://github.com/4thel00z/pdfboss/commit/d12a319dca8fbdd9c198453b45b76cd6499930ed))
* **core:** decode CCITTFaxDecode streams ([9a96b1d](https://github.com/4thel00z/pdfboss/commit/9a96b1df7887b1314ae7cf9f51d8d4858d2074a4))
* **core:** decode JBIG2 generic regions for all four templates ([48f022b](https://github.com/4thel00z/pdfboss/commit/48f022b1e4ec881973283ef564e2631aa9e9a1e7))
* **core:** decode JBIG2Decode streams to 1-bit DeviceGray samples ([ab22639](https://github.com/4thel00z/pdfboss/commit/ab22639d6e477a69973e450834262e6229034d7e))
* **core:** decode MMR-coded JBIG2 generic regions ([3f1ab08](https://github.com/4thel00z/pdfboss/commit/3f1ab08bdb4f0f5e0bcdf00818cf05bbd0fd5020))
* **core:** decode one-dimensional and mixed fax coding ([e09b354](https://github.com/4thel00z/pdfboss/commit/e09b3549e12ac91920e92834d60acb4cf89584e5))
* **core:** decode two-dimensional fax rows ([9057070](https://github.com/4thel00z/pdfboss/commit/9057070298d5643429ba4269184e4afacc02b9fb))
* **core:** dispatch JBIG2 symbol dictionaries and text regions ([5aada44](https://github.com/4thel00z/pdfboss/commit/5aada44afbe8de5d5c7cd3571acd23f4791d597b))
* **core:** let the MQ decoder report that the coded data is spent ([dcb1696](https://github.com/4thel00z/pdfboss/commit/dcb1696bc6e7853f26bbcfd418d6061e5bd4f492))
* **core:** parse JBIG2 segment headers and embedded streams ([b95b3b8](https://github.com/4thel00z/pdfboss/commit/b95b3b83a88408786990dd934eaffc1680b7b22c))
* **render:** report images dropped during rasterization ([3b28ff7](https://github.com/4thel00z/pdfboss/commit/3b28ff7306bd2e768fbb2bce37d8d269014f9472))


### Bug Fixes

* **core:** bound facsimile decoding by each dimension, not just by area ([4088f9a](https://github.com/4thel00z/pdfboss/commit/4088f9a0f37ef26f3ae91342836ca212ae1cc538))
* **core:** bound JBIG2 decoding work with a per-stream budget ([3af8507](https://github.com/4thel00z/pdfboss/commit/3af8507a4bac7a42c4d8b5d66ff8a8551c73f55f))
* **core:** charge the work budget for every JBIG2 symbol, not just its pixels ([383ef68](https://github.com/4thel00z/pdfboss/commit/383ef68fecb9df2c5512b4270f68ef70c2bceb64))
* **core:** make the Annex A list terminator reachable on every input ([91fbd2b](https://github.com/4thel00z/pdfboss/commit/91fbd2b65816e6e82222e916e83b3939746db959))
* **core:** reject JPXDecode instead of painting the codestream as pixels ([7a73864](https://github.com/4thel00z/pdfboss/commit/7a7386414f98003a455dd9439e5d9d40dbc59bd5))
* **core:** stop a zero-width bitmap from costing a pass per row to pack ([28bcfa4](https://github.com/4thel00z/pdfboss/commit/28bcfa4c4ebd48b1cf9a0e147d15a9afef4c2df7))
* **render:** report every drop, bound the report, and surface it to callers ([bf45bc2](https://github.com/4thel00z/pdfboss/commit/bf45bc25a10bccde8c9d71a916ba29cca8137700))


### Performance Improvements

* **core:** form JBIG2 contexts incrementally for nominal AT pixels ([3a7a5f8](https://github.com/4thel00z/pdfboss/commit/3a7a5f86f65b01a95b7c0de03f713ad4a0db703a))
* **core:** step over white bytes when materialising a buffered row ([bccfbbb](https://github.com/4thel00z/pdfboss/commit/bccfbbb5800b0b532caf473f62a59cd0789b0ad5))
* **render:** convert one-component samples through a lookup table ([bb74698](https://github.com/4thel00z/pdfboss/commit/bb74698a17c8b1474c9514281bd4529d2ad118e4))


### Documentation

* benchmark scanned-document rendering ([32dbf47](https://github.com/4thel00z/pdfboss/commit/32dbf47cf4c2bcb48abdc2a80e53451919633ae9))
* **core:** bound the wait for the MQ end-of-data signal ([637ab2a](https://github.com/4thel00z/pdfboss/commit/637ab2ac25101945f60299a7dd7e042ac36c8dc2))
* **core:** describe the row loop the fax decoder now runs ([754035f](https://github.com/4thel00z/pdfboss/commit/754035fbaf175663ceb35fd65c1310f368a4c478))
* **core:** place generic region decoding in the JBIG2 module layering ([623f880](https://github.com/4thel00z/pdfboss/commit/623f880baeddc44d4a233099064c6c50d9c3989b))
* **core:** say precisely which page information fields are read ([18326ce](https://github.com/4thel00z/pdfboss/commit/18326ce99c9bee94e6331829dbf081d005b953a6))
* **core:** say what the fax row decoder does not yet read ([64574ac](https://github.com/4thel00z/pdfboss/commit/64574acc60a7ea1566ae3543441666aa135faf2f))
* **core:** state the windowed context update's true read count ([10dfe80](https://github.com/4thel00z/pdfboss/commit/10dfe804ec55a4bbe6fc73fd6a2989e1dd87c93c))
* **core:** state which Table 11 entries the fax filter reads ([719f4e2](https://github.com/4thel00z/pdfboss/commit/719f4e2a8f398028817bd8d335b6bc937e5a1b93))
* state what JBIG2Decode covers and where it stops ([e99a3e3](https://github.com/4thel00z/pdfboss/commit/e99a3e37a33e2fa8ac7e0e6fe0fbd92972ae8620))

## [0.5.0](https://github.com/4thel00z/pdfboss/compare/v0.4.1...v0.5.0) (2026-07-25)


### Features

* **cli:** bundle substitute fonts by default ([0112ee6](https://github.com/4thel00z/pdfboss/commit/0112ee65d3a354ddd21edcfc9a6f0d33576f364d))

## [0.4.1](https://github.com/4thel00z/pdfboss/compare/v0.4.0...v0.4.1) (2026-07-25)


### Documentation

* list all library crates in the cargo add line ([b46a1ce](https://github.com/4thel00z/pdfboss/commit/b46a1ceb662d80d9424a003fc4abd550f3cb1a12))

## [0.4.0](https://github.com/4thel00z/pdfboss/compare/v0.3.0...v0.4.0) (2026-07-25)


### Features

* **aio:** AsyncDocument open flow with span-only xref chain walk ([7a9f9ae](https://github.com/4thel00z/pdfboss/commit/7a9f9ae9c32580579d04dab69d4111d565f3bd66))
* **aio:** chunked LRU CachedBackend with 64 KiB chunks and 32 MiB cap ([e76b590](https://github.com/4thel00z/pdfboss/commit/e76b59007bd250aeb0580653ed2fb68a9730f255))
* **aio:** decode_stream, read_span and metadata on AsyncDocument ([76d7d29](https://github.com/4thel00z/pdfboss/commit/76d7d29062f72f55847aeecfc62ddc51c196114f))
* **aio:** ElementStream logical layer with per-op content spans ([7f35cbe](https://github.com/4thel00z/pdfboss/commit/7f35cbee9e05fd960d2e66a622367d950edca6a7))
* **aio:** ElementStream physical layer with salvage semantics ([11f8a1d](https://github.com/4thel00z/pdfboss/commit/11f8a1d800d920a8b6efd95a150281b2542f0956))
* **aio:** fetch helper, tail scan and header version parse ([1f89196](https://github.com/4thel00z/pdfboss/commit/1f89196d7ae1a267a06b5b867b52ae5b7890d678))
* **aio:** FileBackend with positioned reads on the blocking pool ([aa56cdd](https://github.com/4thel00z/pdfboss/commit/aa56cdd0ffb36f2fa1962eb6b358eb98a7adb8ba))
* **aio:** HTTP range backend and open_url behind the http feature ([e70ccf4](https://github.com/4thel00z/pdfboss/commit/e70ccf4023d266b034c790472a4d4f3b538768ea))
* **aio:** new pdfboss-aio workspace crate with layered error type ([a0a26ad](https://github.com/4thel00z/pdfboss/commit/a0a26ad1f2f680a6cd8982e88cae10f774807734))
* **aio:** object-safe Backend trait and MemBackend ([8809e5a](https://github.com/4thel00z/pdfboss/commit/8809e5a529c78bf1528303a69b35dafac1545205))
* **aio:** page-tree index built at open with span-only fetches ([0e167b1](https://github.com/4thel00z/pdfboss/commit/0e167b1bf235be594992fb35aabc93971c5e9d24))
* **aio:** window parsers for classic and stream xref sections ([0e9e1f2](https://github.com/4thel00z/pdfboss/commit/0e9e1f2a89124f9982d6d9f5bb49ab4eb4f75351))
* **aio:** windowed get_object, resolve and object-stream cache ([722ba15](https://github.com/4thel00z/pdfboss/commit/722ba1524842364fc6f92d3af91bd71c0ec27edb))
* **cli:** add pdfboss hex with selectors and --annotate ([eaa21ca](https://github.com/4thel00z/pdfboss/commit/eaa21ca0a67a19c84a6a3e60ad98519626aaf2b2))
* **cli:** add pdfboss json value-tree dump ([0eba71b](https://github.com/4thel00z/pdfboss/commit/0eba71b435acc6c5e0b3042c66495b24b48ba264))
* **cli:** add pdfboss q with -r and --hex span dumps ([f15dbe0](https://github.com/4thel00z/pdfboss/commit/f15dbe0ec0e65b35f2d3e4280c66c745f09cff59))
* **cli:** compile and run jq programs via the jaq engine ([0b50218](https://github.com/4thel00z/pdfboss/commit/0b50218c8580a1d8226304ade1168ed514a5f9ba))
* **cli:** convert documents to the fq-style JSON value tree ([785c76c](https://github.com/4thel00z/pdfboss/commit/785c76ce89c5c80071275e4d18b45b80921f259d))
* **cli:** hexyl-style hexdump engine ([3c6596d](https://github.com/4thel00z/pdfboss/commit/3c6596d225f22537f728a6ee9e135cfbaec42496))
* **cli:** input abstraction and exit-code plumbing for explorer subcommands ([8e6cdee](https://github.com/4thel00z/pdfboss/commit/8e6cdee5160150fad32871f31d58b93d7b20cdef))
* **cli:** pdfboss tui subcommand over file or http targets ([f4debbd](https://github.com/4thel00z/pdfboss/commit/f4debbd09fab6d59c791d5ca34568e776d530e92))
* **core:** content-operator elements with in-content spans ([aa0e37a](https://github.com/4thel00z/pdfboss/commit/aa0e37a2634d36ca5921514b30296c18fc6d9b31))
* **core:** document byte/xref accessors, spanned parses, page object refs ([15b13d3](https://github.com/4thel00z/pdfboss/commit/15b13d37621b342eb00d2fe51880b268f8ae74d3))
* **core:** element model types (Span, Element, ElementOpts, XrefKind) ([074a799](https://github.com/4thel00z/pdfboss/commit/074a7993cf710af0f043de4767341556154e0a97))
* **core:** lazy physical element iteration with byte spans ([4bfc6e2](https://github.com/4thel00z/pdfboss/commit/4bfc6e23035199fe76ab8f2cef76bf62a939ed63))
* **core:** logical element iteration (pages, fonts, images, annotations) ([5636d21](https://github.com/4thel00z/pdfboss/commit/5636d21f43f0bb92f48db1237504dd6f74bb285c))
* **core:** public xref iteration and span-reporting section parser ([fcf613f](https://github.com/4thel00z/pdfboss/commit/fcf613f84c55d1e21096c33936ca80eb1157fcec))
* **core:** span-reporting content-stream parser ([4917a2a](https://github.com/4thel00z/pdfboss/commit/4917a2a477f65b82bebc788c08530aa96fedc531))
* distribute the Rust crates on crates.io ([76ed3f0](https://github.com/4thel00z/pdfboss/commit/76ed3f0080c3914f5f6632cda864e2a671abafe9))
* **python:** async element streaming via AsyncDocument.elements() ([49d0ca6](https://github.com/4thel00z/pdfboss/commit/49d0ca67ef682e34cd1c98117b014bfcd3df54e4))
* **python:** AsyncDocument with open/from_bytes/metadata/get_object ([b220198](https://github.com/4thel00z/pdfboss/commit/b220198159d7f18122919d2799168b4dccf9d3de))
* **python:** AsyncDocument.open_url over HTTP range requests ([1753277](https://github.com/4thel00z/pdfboss/commit/1753277962506ffff08b07d0e4685aedc20f5a70))
* **python:** Element.value() lazy object-to-Python conversion ([0ce7509](https://github.com/4thel00z/pdfboss/commit/0ce7509cb80dc933300fed133adbd0459059ff14))
* **python:** sync Document.elements() iterator with Element kind/span/ref/page ([489f9c8](https://github.com/4thel00z/pdfboss/commit/489f9c809f2bb72b467025f690c9d3d1abca5f25))
* **python:** type stubs for Element, elements() and AsyncDocument ([7b66fb7](https://github.com/4thel00z/pdfboss/commit/7b66fb713beeb149687b77b04cd989bf2d9a69a0))
* **tui:** app state machine with msg/cmd update loop ([9bcad29](https://github.com/4thel00z/pdfboss/commit/9bcad2945e4b85b55d8ba4aab10b65c1a5a88f2b))
* **tui:** frame rendering with TestBackend snapshots ([1f9eb99](https://github.com/4thel00z/pdfboss/commit/1f9eb9999738a50f74b476d2d98ac2e980442553))
* **tui:** half-block page preview model with debounce and spinner ([ec7d283](https://github.com/4thel00z/pdfboss/commit/ec7d283f5d1e04ee612d001adc0bd3961526da0e))
* **tui:** hexyl-style hex pane model with windowed spans ([df1541b](https://github.com/4thel00z/pdfboss/commit/df1541b37501cbfb3b8d3ead9b73cac714650eb4))
* **tui:** incremental search model with generation-tagged hits ([0e7d9e7](https://github.com/4thel00z/pdfboss/commit/0e7d9e785d4a4dc0424410680121124b0101e9c5))
* **tui:** inspector with pretty/raw/decoded/ops views and ref cursor ([eb64423](https://github.com/4thel00z/pdfboss/commit/eb64423a60d4ebe9f70e8bf6c9808aca2f3e1da3))
* **tui:** key-to-action mapping ([427e507](https://github.com/4thel00z/pdfboss/commit/427e507b90ba84c5edca9d17de4079a3b3f115be))
* **tui:** lazy element tree state machine ([5fce7a6](https://github.com/4thel00z/pdfboss/commit/5fce7a6c602457451a80f448ac037ce7b916b06b))
* **tui:** scaffold pdfboss-tui workspace crate ([9b1e1fa](https://github.com/4thel00z/pdfboss/commit/9b1e1faf015b1e73bcc93ddda7f4659f324a8299))
* **tui:** tokio event loop with background command executor ([4f79d5a](https://github.com/4thel00z/pdfboss/commit/4f79d5a55a8567eab910b9d9fb1297458f00e7c8))


### Bug Fixes

* **aio:** emit classic section before its hybrid stream for core parity ([2fbccad](https://github.com/4thel00z/pdfboss/commit/2fbccad8577673f8b1359eb63aadd592db285ebd))
* **aio:** keep cache byte accounting exact when a chunk is re-inserted ([3332536](https://github.com/4thel00z/pdfboss/commit/3332536cf1959a0e004b01a4ea5bf2c0a2018817))
* **aio:** phase-review fixes — encrypted-doc rejection, salvage parity, owned stream, body cap ([c789f2d](https://github.com/4thel00z/pdfboss/commit/c789f2d59146ebc1b7b049c9fbaa38d293d18741))
* **aio:** re-export FileBackend from the crate root ([2a421a9](https://github.com/4thel00z/pdfboss/commit/2a421a9deedeb2cec23385f19511e0a48ac5d61c))
* **aio:** reuse core content spans and resolve resource categories ([f074129](https://github.com/4thel00z/pdfboss/commit/f07412955f82a1ad645f4e7093486915bebc8eec))
* **aio:** signal window growth for truncated stream sections ([5be941d](https://github.com/4thel00z/pdfboss/commit/5be941d2d33dd8e0b33cc212c5280c28f96cf258))
* **cli:** bound fabricated --hex spans against the file length ([07ab339](https://github.com/4thel00z/pdfboss/commit/07ab339654f2d712d1c0cc975a3b2504a02cf6c3))
* **cli:** tui accepts URLs unconditionally like the other subcommands ([d2fc113](https://github.com/4thel00z/pdfboss/commit/d2fc1137887dceeae7c148921b4785c5848142d0))
* **core:** surface hybrid /XRefStm sections in element iteration ([5fdf1cc](https://github.com/4thel00z/pdfboss/commit/5fdf1ccb26036b2a73e3c9ffa92dfcad3585a8b4))
* pre-merge polish — docs refresh, error-message normalization, tui target context, aio window ceiling ([e9c5cc8](https://github.com/4thel00z/pdfboss/commit/e9c5cc87ab04d0ac4a0b99fbbda0c53fa0ce1900))
* **tui:** construct the terminal guard before EnterAlternateScreen ([62a67ad](https://github.com/4thel00z/pdfboss/commit/62a67ad03678e47d2b5c1c6e98ed660d829c4db2))
* **tui:** correct straddling-window fetch and align byte classes with the CLI ([1c03105](https://github.com/4thel00z/pdfboss/commit/1c031056027b70112b46122e43db2d94dd6e879c))
* **tui:** idempotent batch delivery and failed-section retry ([cd0c981](https://github.com/4thel00z/pdfboss/commit/cd0c98185a9a9bb3193790f1296c438e38d4944d))
* **tui:** phase-review fixes — search-cancel epoch, preview byte cache, trailer dead-end, tree failure signal ([a6d3aa8](https://github.com/4thel00z/pdfboss/commit/a6d3aa880d7c3db7a4a9416841a3b3c38999d15c))
* **tui:** resolve broken intra-doc links for cargo doc -D warnings ([5fdb6b4](https://github.com/4thel00z/pdfboss/commit/5fdb6b4a0d2eb1c3467b106262f6a6725d4c2e1b))


### Documentation

* **cli:** update hexdump module doc now that hex is wired ([c52f275](https://github.com/4thel00z/pdfboss/commit/c52f2750fddb1ac653a2f48d236c97431e6c4453))
* **core:** document element iteration ([3ed5d45](https://github.com/4thel00z/pdfboss/commit/3ed5d45086d25fe7e4da457c1f200396e02dec95))
* **core:** pin element ordering contract; guard testkit fixture casts ([e4e8378](https://github.com/4thel00z/pdfboss/commit/e4e8378dde54833e7ebc9f6dac44b3354f58245f))
* design spec for element iterator, async IO, TUI explorer, fq-style CLI ([b8ffd08](https://github.com/4thel00z/pdfboss/commit/b8ffd08c464497f1070e65a167dc192424428efb))
* **plans:** element-explorer plan 01 — core element model ([c44a8da](https://github.com/4thel00z/pdfboss/commit/c44a8dad0b65d495f4c0cd0830491f6ff8a90307))
* **plans:** element-explorer plans 02-05 — aio, python, cli, tui ([0666d73](https://github.com/4thel00z/pdfboss/commit/0666d7322b7d2334721fefb0682cd5f1579138bd))
* reword README element-iteration feature item to list style ([108ace0](https://github.com/4thel00z/pdfboss/commit/108ace0555fa9b7935f1401c9b541283de58185e))

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
