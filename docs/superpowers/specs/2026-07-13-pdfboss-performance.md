# pdfboss performance deep-dive & optimization plan

**Date:** 2026-07-13
**Goal:** close the speed gap between a straightforward safe-Rust PDF reader and a
mature native PDF engine, with every change verified by an in-repo benchmark.

This is a clean-room document: it describes techniques and comparisons
generically ("a mature native PDF engine"), never by product name.

## 1. Why a mature native engine is faster (the structural story)

A from-scratch safe-Rust reader like pdfboss starts correct but slow for a small
number of recurring reasons. A battle-tested engine wins because it:

1. **Does the minimum work per request.** It resolves and decodes objects
   *on demand*, caches the results, and never re-does them. pdfboss currently
   re-clones cached objects on every access and re-decodes streams every call.
2. **Borrows instead of copies.** It parses over the original file buffer and
   hands out slices/offsets; the raw bytes of a stream are never copied until a
   filter actually needs to transform them. pdfboss copies every stream body and
   every name out of the buffer eagerly.
3. **Uses the right data structures in the inner loop.** Its rasterizer keeps an
   *active-edge table* so each scanline touches only the handful of edges that
   cross it, instead of scanning every edge of the path for every row.
4. **Amortizes fixed costs with caches.** Fonts, decoded object streams, and
   glyph data are built once and reused. pdfboss rebuilds font/encoding tables
   per content-stream invocation.
5. **Parallelizes embarrassingly-parallel work** (pages, tiles) across cores.
   pdfboss cannot do this yet because its document object cache is `!Sync`.

Items 1–4 are allocation/algorithmic and safe to land incrementally. Item 5 and
the deepest form of item 2 require API-level changes and are staged as Phase 2.

## 2. Baseline (this machine, `cargo bench`, default release profile)

| Benchmark | Input | Time |
|---|---|---|
| `parse/load_300_pages` | 300-page doc, load only | ~917 µs |
| `parse/load_and_walk_300_pages` | load + resolve every page | ~1.18 ms |
| `filter/flate_decode_1mib` | inflate 1 MiB stream | ~243 µs (~4 GiB/s) |
| `text/extract_text_warm_500_lines` | extract, doc cached | ~503 µs |
| `text/extract_text_cold_500_lines` | load + extract | ~522 µs |
| `render/render_1000_rects_scale2` | 1000 filled rects @2× | **~18.9 ms** |
| `render/render_400_curves_scale2` | 400 filled beziers + strokes @2× | **~65.5 ms** |

**Rendering dominates by 3–4 orders of magnitude.** The curves case being 3.5×
the rects case is the fingerprint of a per-scanline O(edges) fill: flattened
beziers produce many edges, and every one is re-tested on every row.

## 3. Findings (from static analysis of the current code)

15 high-impact, 20 medium, 16 low. The high-impact set, ranked by expected
gain-to-effort against the measured hot paths:

### Tier 1 — biggest measured ROI, contained, safe
- **R1. Active-edge table in the rasterizer** — `render/src/raster.rs`. Replace
  the brute-force "test every edge on every scanline" with a y-sorted edge list
  and an incrementally-maintained active set. Expected 2–10× on edge-dense fills
  (the curves benchmark). *The headline win.*
- **R2. `Rc<Mask>` for the clip mask** — `render/src/executor.rs`. `q`/form
  invocation deep-clones the full-page clip buffer; make it a refcount bump with
  clone-on-write only when `W`/`W*` replaces the mask.
- **O1. Object cache hands out `Rc<Object>`** — `core/src/document.rs`. On a
  cache hit `get()` does `(**cached).clone()`, deep-copying the whole object
  (including large stream `data`) every time. Add an internal `get_rc` returning
  the shared handle and route hot callers through it; keep public `get -> Object`.

### Tier 2 — strong, contained
- **O2. Cache decoded object-stream bytes + parsed header** — `document.rs`,
  `objstm.rs`. Object-stream contents are re-decompressed for every contained
  object, and the header is re-scanned from the start each time (O(n²) across a
  stream's objects). Cache both, keyed by stream number.
- **L1. Parse integer tokens in place** — `lexer.rs`. Numbers are the most
  frequent token; today each digit is pushed into a fresh `String` then
  `parse()`d. Accumulate `value = value*10 + digit` directly.
- **L2. `lex_name` no-escape fast path** — `lexer.rs`. Names (every dict key)
  always allocate a `Vec` and re-validate UTF-8 over the copy; slice directly
  from the buffer when there is no `#xx` escape.
- **T1. Hoist the font cache to the executor** — `text/src/extract.rs`. Fonts and
  256-entry encoding tables are rebuilt per `run()` (per content stream and per
  form). Build once per page.
- **T2. `decode()` returns `char` on the fast path** — `text/src/font.rs`. Avoid
  a `String` allocation per glyph; only the ToUnicode multi-unit case needs one.
- **F1. TIFF predictor in place** — `filters/predictor.rs`. Mutate the caller's
  buffer instead of `to_vec()`-ing a copy.
- **F2. Index-based LZW dictionary** — `filters/lzw.rs`. Replace `Vec<Vec<u8>>`
  (full-prefix clone per new code) with flat `(prefix, suffix, len)` entries.

### Tier 3 — deferred to Phase 2 (API-invasive or blocked)
- **S1. Borrowed stream data / offset ranges** (`parser.rs:210`) — the single
  biggest structural win (no stream body ever copied), but it threads a lifetime
  through `Object`/`Stream`/`Document`/filters and breaks the public API.
- **S2. Memory-map the input** (`document.rs:125`) — pairs with S1; only touched
  pages fault in. Keep the owned-buffer path for the Python `data=` case.
- **S3. Parallel pages/tiles** — blocked: `Document`'s cache is `RefCell`-based
  and `!Sync`. Requires a thread-safe cache (RwLock or sharded) first.
- **S4. String interning + fast hasher for `Name`** — large allocation-count cut
  on object-dense docs; deferred to keep the dependency surface stable for now.
- **S5. Python `Mutex` narrowing** — release the lock around CPU-heavy
  render/encode so multithreaded Python callers aren't serialized.

### Global, zero-code
- **G1. `flate2` `zlib-rs` backend** — pure-Rust, faster inflate, no C dependency
  (stays clean-room-friendly).
- **G2. Release profile:** `lto = "thin"`, `codegen-units = 1`. (Not
  `panic = "abort"`: PyO3 relies on unwinding to turn Rust panics into Python
  exceptions.)

## 4. Feature gaps (tracked, not addressed here)

Out of scope for this performance pass and unchanged from v0.1: encrypted
documents, glyph painting in rasterized output, shadings/tiling patterns, and
`JPXDecode`. These are feature work, not speed work, and each is a substantial
project in its own right.

## 5. Methodology

- Criterion benches live in `crates/*/benches/` and drive the real public API
  over large synthetic fixtures built by the testkit.
- Every optimization is landed only if (a) the relevant benchmark improves and
  (b) the full suite stays green (`cargo test --workspace`, `pytest`), with
  `clippy -D warnings`, `fmt`, and `cargo doc -D warnings` clean.
- Changes are committed in small, individually-verified batches so any
  regression is trivially bisectable.
