# pdfboss glyph painting — full font coverage, gated

**Date:** 2026-07-14
**Goal:** paint every font the rasterizer currently leaves blank, behind a tiered
render option; deliver non-embedded substitution as a compile-time cargo feature
(Rust/CLI) and an optional `pdfboss[full]` extra (Python).

Clean-room note: this document describes techniques generically from ISO 32000 and
the public CFF/Type1/OpenType specifications; no reference engine is named.

## 1. Background — what paints today, and what doesn't

The rasterizer (`pdfboss-render`) paints outlines for exactly two font shapes:
simple `/TrueType` and `/Type0`→`CIDFontType2`, both requiring an embedded
`FontFile2` (TrueType `glyf`). `GlyphFont::load` returns `None` for everything
else, and the executor then advances the text position without painting (so
surrounding text stays aligned).

**Text extraction is unaffected and already complete** — `pdfboss-text` decodes
WinAnsi/MacRoman/Standard encodings, `/Differences`, and `ToUnicode` for every
font. This work is *purely* about turning character codes into filled outlines.

Four buckets are blank today:

1. **Embedded, non-`glyf`** — `FontFile3` (CFF / OpenType-CFF: `Type1C`,
   `CIDFontType0C`) and `FontFile` (Type1). The outline program is in the file;
   we cannot read the format. Highest real-world impact (subset OpenType is the
   modern norm). No bundled assets; exact outlines.
2. **Under-mapped embedded TrueType** — simple TT with no usable `cmap`, or driven
   by `/Encoding`+`/Differences`. We already load the program but pick the wrong
   glyph. Cheapest; no assets; exact.
3. **Truly non-embedded** — the standard 14 and any font with no `FontFile*`. No
   outline data anywhere; requires substitution with bundled faces. Approximate.
4. **Type3** — glyphs are PDF content streams (`/CharProcs`). Reuse the executor.
   Niche; no assets; exact.

## 2. Scope

In scope: all four buckets, the render-time gate, the `substitute-fonts` cargo
feature, and the `pdfboss[full]` Python extra with a companion `pdfboss-fonts`
data package.

Out of scope (tracked elsewhere): shadings/tiling patterns, `JPXDecode`, and
vertical writing-mode glyph metrics beyond what exists.

## 3. Architecture

### 3.1 Render options and the gate

```rust
/// How hard the rasterizer tries to turn text into filled outlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GlyphPainting {
    /// Fastest: only embedded TrueType `glyf` (today's behavior).
    EmbeddedTrueTypeOnly,
    /// Every embedded program: glyf + CFF + Type1 + Type3. No bundled assets.
    #[default]
    AllEmbedded,
    /// Also substitute bundled/provided faces for non-embedded fonts.
    Full,
}

#[derive(Clone, Debug, Default)]
pub struct RenderOptions {
    pub glyph_painting: GlyphPainting,
    /// Where substitute faces come from when `glyph_painting == Full`.
    /// `Builtin` needs the `substitute-fonts` feature; `Dir` is supplied at
    /// runtime (the Python layer points this at the `pdfboss-fonts` package).
    pub substitutes: SubstituteSource, // Builtin | Dir(PathBuf) | None (default)
}

pub fn render_page(doc, page, scale) -> Result<Pixmap> {
    render_page_with_options(doc, page, scale, &RenderOptions::default())
}
pub fn render_page_with_options(doc, page, scale, &RenderOptions) -> Result<Pixmap>;
```

`Default` is `AllEmbedded`: self-contained, exact, no assets. `EmbeddedTrueTypeOnly`
is the "disable for speed" tier and is byte-for-byte today's output. `Full` with no
available substitute source degrades to `AllEmbedded` (never an error inside the
renderer). The executor reads the tier when resolving each font resource and only
attempts the loaders that tier permits.

### 3.2 GlyphFont becomes an outline-source enum

Today `GlyphFont` wraps a `TrueType`. Generalize the outline source while keeping
the small public surface the executor already uses (`two_byte`, `gid`, `outline`,
`advance`, `units_per_em`):

```rust
enum Outlines {
    TrueType(TrueType),          // FontFile2 (existing)
    Cff(CffFont),                // FontFile3 / CIDFontType0C
    Type1(Type1Font),            // FontFile
    Type3(Type3Font),            // CharProcs (drawn via the executor)
    Substitute(TrueType),        // bundled/provided face (Full tier)
}
```

Each source exposes: code/CID → glyph id, glyph id → `Vec<Seg>` outline in font
units, and units-per-em. `Type3` is special-cased in the executor (it paints by
recursion, not by returning segments).

### 3.3 Advances come from the PDF, not the program

Positioning is defined by the PDF's `/Widths` (simple) or `/W`+`/DW` (CID), in
glyph space (1000/em) — **not** the program's own advances. Today the renderer
reads TrueType `hmtx`, which happens to agree for embedded subsets but is wrong for
substitutes. Switch the advance source to the PDF widths, falling back to the
program's advance only when the PDF omits them.

Width/encoding parsing already exists in `pdfboss-text::font`. To avoid two
divergent copies, move the shared pieces — standard-encoding tables, the Adobe
Glyph List (glyph-name→Unicode), `/Widths`+`/W`/`/DW` parsing, and the standard-14
AFM width tables — into a small shared crate `pdfboss-encoding` (depends only on
`pdfboss-core`), consumed by both `pdfboss-text` and `pdfboss-render`.

### 3.4 Bucket 2 — TrueType code→GID (module: `truetype.rs`)

Build the simple-font 256-entry code→GID table by trying, in order:
1. `/Encoding`+`/Differences` → glyph name → AGL → Unicode → `(3,1)` cmap.
2. `(3,0)` symbol cmap with the `0xF000` offset.
3. `(1,0)` Macintosh cmap.
4. `post` table (glyph name → GID) for `/Differences` names lacking a cmap route.
5. No-cmap subset last resort: code == GID.

Add `post`, `(1,0)`, and `(3,0)` subtable parsing to the TrueType reader
(bounds-checked, no panics).

### 3.5 Bucket 1a — CFF (module: `cff.rs`)

Clean-room CFF reader + Type2 charstring interpreter:
- Header, INDEX (Name/TopDICT/String/GlobalSubr), Top DICT, Private DICT
  (local subrs, `defaultWidthX`/`nominalWidthX`), charset.
- CID-keyed CFF (`ROS`, `FDArray`, `FDSelect`) for `CIDFontType0C`.
- Type2 charstring interpreter → `Seg` outlines (moveto/lineto/curveto family,
  hstem/vstem no-ops for fill, `hintmask`/`cntrmask`, `callsubr`/`callgsubr` with a
  recursion/step guard, `endchar` incl. legacy `seac`).
- units-per-em from `FontMatrix` (default 1000).
- code→GID: simple uses `/Encoding`+`/Differences`→name→charset(SID)→GID;
  CID uses CID→GID via charset.

### 3.6 Bucket 1b — Type1 (module: `type1.rs`)

- PFB segmentation / raw detection; `eexec` decrypt; charstring decrypt (lenIV).
- Parse `/Encoding`, `/CharStrings`, `/Subrs`, `/FontMatrix`.
- Type1 charstring interpreter (hsbw/sbw, moveto/lineto/curveto, `callsubr`,
  `div`, `callothersubr`/flex/hint-replacement, `seac`) → `Seg` outlines.
- code→GID via the font's `/Encoding` (or the PDF `/Encoding`+`/Differences`).

### 3.7 Bucket 4 — Type3 (executor)

`/Type3` fonts carry `/CharProcs`, `/Encoding`, `/FontMatrix`, `/Resources`.
Painting a glyph = run its CharProc content stream through the executor with the
`/FontMatrix` folded into the text/CTM chain and a recursion depth guard. Honor
`d0` (colored, uses current fill) vs `d1` (uncolored glyph description). Requires
making the executor re-entrant for glyph procs (extract a callable entry point).

### 3.8 Bucket 3 — substitution (feature `substitute-fonts`)

```rust
struct FaceRequest { family: Family, italic: bool, bold: bool } // Family: Serif|Sans|Mono
trait SubstituteProvider { fn face(&self, req: &FaceRequest) -> Option<&[u8]>; }
```

- `FaceRequest` derived from `BaseFont` (name heuristics: "Times"/"Georgia"→Serif,
  "Courier"/"Mono"→Mono, else Sans; "Bold"/"Italic"/"Oblique") backed by the
  descriptor `/Flags` (Serif bit 2, FixedPitch bit 1, Italic bit 7).
- code→GID for substitutes: glyph name (PDF `/Encoding`+`/Differences`, else the
  base encoding) → AGL → Unicode → substitute `cmap`. Advances from PDF `/Widths`,
  or the standard-14 AFM tables when `/Widths` is absent.
- **Bundled faces:** Croscore — Arimo (Sans/Helvetica), Tinos (Serif/Times),
  Cousine (Mono/Courier). All **Apache-2.0**, compatible with the repo's MIT OR
  Apache-2.0 license. `Symbol` and `ZapfDingbats` have no license-clean substitute
  in v1 and remain unpainted (documented partial).
- **Delivery — Rust/CLI:** the `substitute-fonts` cargo feature `include_bytes!`s
  the three TTFs and supplies a `BuiltinProvider`. Without the feature the type
  still exists but yields no faces, so `Full` degrades to `AllEmbedded`.

### 3.9 Standard-14 AFM widths

Ship the 14 core AFM width tables (numbers only; from the redistributable Adobe
core-14 metrics) in `pdfboss-encoding`. These give correct advances for standard-14
fonts (which usually omit `/Widths`) and are independent of the `substitute-fonts`
feature — they improve positioning even at the `AllEmbedded` tier.

## 4. Python packaging — reconciling a compile-time feature with a pip extra

A cargo feature is chosen at build time; a pip extra is chosen at install time and
can only add *dependencies*. So the prebuilt wheel cannot itself carry the fonts
behind an extra. Resolution:

- The published `pdfboss` wheel stays **lean** (no compiled-in fonts).
- A separate **pure-data** package `pdfboss-fonts` ships the three Croscore TTFs
  plus a tiny `__init__` exposing their directory. No compiled code.
- `pdfboss[full]` = `pdfboss` + `pdfboss-fonts`
  (`[project.optional-dependencies] full = ["pdfboss-fonts>=0.1"]`).
- At runtime, when `Full` is requested, the binding imports `pdfboss_fonts`, gets
  the directory, and passes it to Rust as `SubstituteSource::Dir`. If the package
  is absent, raise a clear, actionable error naming `pip install pdfboss[full]`.

`pdfboss-fonts` is its own PyPI project and needs its own trusted publisher
(mirrors the `release-please.yaml` setup). Its build is a trivial data-only wheel.

## 5. Public API changes

- **Rust:** add `RenderOptions`, `GlyphPainting`, `SubstituteSource`,
  `render_page_with_options`; keep `render_page` as the default wrapper. New crate
  `pdfboss-encoding`. New cargo feature `substitute-fonts` on `pdfboss-render`.
- **CLI:** `pdfboss render --fonts embedded-only|all-embedded|full` (default
  `all-embedded`). `full` without the built-in feature and without a discoverable
  font dir errors with guidance.
- **Python:** `page.render(scale=…, fonts="embedded-only"|"all-embedded"|"full")`
  (default `"all-embedded"`); `full` resolves `pdfboss_fonts`. New `full` extra.

## 6. Error handling and leniency

Every new parser is bounds-checked and panic-free. Any parse/mapping failure
degrades that font to unpainted (today's behavior): a strict superset with zero
regressions on the existing corpora. Recursion is bounded in the CFF/Type1 subr
interpreters and in Type3 glyph-proc execution.

## 7. Testing

- Unit tests per parser over synthetic fixtures (testkit): CFF INDEX/DICT/charset,
  a Type2 charstring producing a known box, Type1 eexec round-trip, `post`/`(1,0)`/
  `(3,0)` cmap mapping, Type3 recursion, substitute name→GID.
- Golden-ink assertions: render one known glyph and assert non-empty ink within its
  expected device bbox (per source type).
- Corpus regression: the 259-file set and ba-qa (645) must still render crash-free;
  report the painted-glyph coverage delta (blank-text pages before/after).
- Benches: confirm `EmbeddedTrueTypeOnly` is unchanged; measure `AllEmbedded`
  per-glyph overhead for CFF vs TrueType.
- Feature matrix: build/test with and without `substitute-fonts`.

## 8. Rollout — bisectable commits (each keeps all gates green)

1. `pdfboss-encoding` crate: move shared encoding/AGL/width tables + AFM-14; rewire
   `pdfboss-text`. No behavior change.
2. `RenderOptions`/`GlyphPainting` + `render_page_with_options` + executor threading
   + CLI/Python plumbing. `AllEmbedded` still == today until loaders land.
3. GlyphFont outline-source refactor + PDF `/Widths` advance source.
4. Bucket 2 — TrueType mapping (`post`/`(1,0)`/`(3,0)`/AGL/`/Differences`).
5. Bucket 1a — CFF.
6. Bucket 1b — Type1.
7. Bucket 4 — Type3.
8. Bucket 3 — substitution engine + provider + `substitute-fonts` feature.
9. Python `pdfboss-fonts` package + `[full]` extra + runtime discovery + its
   release pipeline.

## 9. Risks / decisions to confirm

- **Font licensing:** Croscore (Apache-2.0) chosen for license cleanliness over
  metric coverage. `Symbol`/`ZapfDingbats` left unpainted in v1.
- **CFF/Type1 interpreters** are the largest, highest-risk pieces; landed
  independently (steps 5–6) behind the already-shipping gate so partial progress
  never regresses `AllEmbedded`.
- **AFM redistribution:** confirm the core-14 metrics are shipped as plain width
  tables authored in-repo (numbers), not copied verbatim from a licensed file.
