# Limitations

Rendering is lenient and it says so: content pdfboss cannot read is skipped so the rest of the page still rasterizes, and every dropped or approximated item lands in a report — `pdfboss render` warns on stderr, the terminal explorer raises a notice, and the libraries return it through `render_page_reporting` (Rust) and `Page.render_reporting()` (Python). See [Rendering pages](../guide/rendering.md) for the reporting APIs.

The whole not-yet-supported list is two faces: `/Symbol` and `/ZapfDingbats` have no license-clean substitute, so they stay blank rather than borrowing an unrelated face's glyphs.

## Fonts

Glyph painting is staged in tiers — `embedded-only`, `all-embedded`, `full` — selected with `--fonts` (CLI), the `fonts` parameter (Python) or `RenderOptions::glyph_painting` (Rust); the tiers are described in [Rendering pages](../guide/rendering.md#font-tiers). What stays limited:

- `full` substitutes only **non-embedded simple** fonts, and a bold *sans* substitute is not visually distinct from regular weight.
- Standard-14 advance widths come from the Adobe Core-14 AFM tables when a substitute is used, behind the PDF's own `/Widths`.

## CMaps and CID fonts

Type0 `/Encoding` CMaps resolve — the predefined ISO 32000 Table 118 CJK set is compiled in (behind the `predefined-cmaps` feature, on by default in the CLI and the wheel), embedded CMap streams parse the same way, widths key on the mapped CID, vertical text (`WMode` 1) advances by `/W2`/`/DW2` with the default position vector, and extraction maps CIDs to Unicode through the character collection when `/ToUnicode` is absent.

Deferred: vertical runs still extract as horizontal-schema spans, one per show operator, with x/y at the glyph origin.

## JBIG2

`JBIG2Decode` covers the embedded stream format end to end: generic regions (all four templates, with TPGDON, arithmetic or MMR-coded), symbol dictionaries and text regions in both the arithmetic and the Huffman variant — refinement/aggregate-coded symbols and refined instance placements included — pattern dictionaries and halftone regions, generic refinement regions (both templates, with TPGRON) refining either the page or a retained intermediate region, intermediate regions of every type, and custom code table segments. Nothing in the standard's segment type table is refused; a malformed or truncated stream fails with a message naming what was wrong instead of rendering a blank.

## Colour

Colour converts to sRGB. `ICCBased` spaces parse their embedded profile (v2 and v4; matrix/TRC and grayTRC models, and `A2B0` lookup pipelines): a profile equivalent to sRGB keeps the exact device-RGB path, others transform per colour with Bradford adaptation from the D50 connection space, and a profile that will not parse falls back to the `/N` channel-count reduction. `CalRGB`, `CalGray`, and `Lab` convert through CIE XYZ the same way.

Only a profile's default transform is used — rendering intents are not switched — and `DeviceN` keeps a tint approximation.

## JPEG 2000

`JPXDecode` implements ITU-T T.800 (JPEG 2000 Part 1); what it approximates it reports as a render warning rather than passing off silently.

ICC profiles embedded in the JPEG 2000 container are interpreted through the same ICC engine as `ICCBased` colour — a profile equivalent to sRGB or device gray keeps the exact device path, others transform per sample — and only a profile that will not parse falls back to the channel-count approximation. sYCC converts with the exact IEC 61966-2-1 Amd. 1 inverse.

Part 2 (ISO/IEC 15444-2) extensions are tolerated in the container but not decoded. Every output sample is normalized to 8 bits per channel with round-to-nearest, so sources deeper than 8 bits (the spec allows up to 38) still land on an 8-bit output grid.

## Optional content

Optional content groups (PDF layers, ISO 32000 §8.11) are honored per the document's default configuration: rendering and text extraction skip layers it turns off, counting them on the reports' `hidden` counters.
