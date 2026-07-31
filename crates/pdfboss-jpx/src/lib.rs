//! Cleanroom JPEG 2000 decoder for the PDF `JPXDecode` filter (ISO 32000
//! 7.4.9), implemented purely from ITU-T T.800 (08/2002).
//!
//! Scope: JP2 box containers and raw codestreams; the full Annex A marker
//! set; Tier-2 packet decoding with all five progression orders and POC;
//! Tier-1 EBCOT (Annex D) over the Annex C MQ coder; Annex E
//! dequantization with RGN maxshift; the Annex F 5-3 and 9-7 inverse
//! wavelets; Annex G component transforms and Annex I palette/colour
//! metadata.
//!
//! Contract: header-level problems (bad signature, unparsable SIZ/COD,
//! exceeded [`DecodeLimits`]) are hard errors. Once the first tile begins
//! decoding, the decoder is lenient — a corrupt packet or code-block
//! zeroes the remainder of its scope, appends one warning to
//! [`DecodedImage::warnings`], and decoding continues. Output samples are
//! 8-bit: 16-bit sources are right-shifted to 8, signed samples are
//! level-shifted per T.800 G.1.2. The decoder never panics on hostile
//! input and contains no `unsafe`.

mod boxes;
mod color;
mod dequant;
mod dwt;
mod error;
mod geometry;
mod markers;
mod mq;
mod packet;
mod t1;
mod tagtree;

pub use error::{JpxError, Result};

/// Hard bounds on attacker-controlled allocation and work, checked before
/// the corresponding allocations happen.
#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    /// Maximum image-region pixels on the reference grid, per component:
    /// `(Xsiz - XOsiz) * (Ysiz - YOsiz)` (T.800 A.5.1/B-2).
    pub max_pixels: u64,
    /// Maximum component count (SIZ Csiz allows up to 16384).
    pub max_components: u16,
    /// Maximum tile count `numXtiles * numYtiles` (Equation (B-5)).
    pub max_tiles: u32,
    /// Maximum bytes of decoded output, checked before allocation.
    pub max_decoded_bytes: u64,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        DecodeLimits {
            max_pixels: 1 << 27,
            max_components: 16,
            max_tiles: 65_535,
            max_decoded_bytes: 1 << 30,
        }
    }
}

/// Colour interpretation of the decoded samples, from the JP2 colr box
/// (T.800 I.5.3.3) or, for raw codestreams, guessed from the component
/// count.
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorKind {
    /// Single-channel greyscale (EnumCS 17, or 1 component).
    Gray,
    /// Three-channel RGB (EnumCS 16 sRGB, sYCC already converted, or 3
    /// components).
    Rgb,
    /// Four-channel CMYK (from an ICC guess over 4 colour channels).
    Cmyk,
    /// An ICC profile the decoder does not interpret: colour approximated
    /// by component count (T.800 I.5.3.3 METH 2); a warning records it.
    IccGuess {
        /// Colour channel count the guess is based on.
        components: u8,
    },
    /// An enumerated colourspace this crate does not convert.
    Other {
        /// The EnumCS value (I.5.3.3).
        enumeration: u32,
        /// Colour channel count.
        components: u8,
    },
}

/// A fully decoded image.
#[derive(Clone, Debug)]
pub struct DecodedImage {
    /// Image-region width: `Xsiz - XOsiz` after the canvas crop (T.800 B-1).
    pub width: u32,
    /// Image-region height: `Ysiz - YOsiz`.
    pub height: u32,
    /// Channel count after palette and component transforms, including any
    /// alpha channel.
    pub components: u8,
    /// Interleaved samples, 8-bit normalized, row-major,
    /// `width * height * components` bytes.
    pub samples: Vec<u8>,
    /// Colour interpretation of the colour channels.
    pub color: ColorKind,
    /// Channel index of the opacity channel, when the JP2 cdef box defines
    /// one (T.800 I.5.3.6, association 0 with type 1 or 2).
    pub alpha_index: Option<u8>,
    /// Soft failures encountered after headers parsed (leniency doctrine).
    pub warnings: Vec<String>,
}

/// Decodes a JPEG 2000 image (JP2 file or raw codestream) into 8-bit
/// interleaved samples.
///
/// This is the crate's single entry point (sans-I/O: the caller supplies
/// the bytes). See the crate docs for the error-vs-warning contract.
pub fn decode(data: &[u8], limits: &DecodeLimits) -> Result<DecodedImage> {
    // Container sniff + box walk (T.800 Annex I) -> codestream slice.
    let container = boxes::scan(data)?;
    // Main header + every tile-part header/body (Annex A).
    let cs = markers::parse_codestream(container.codestream, limits)?;
    let siz = &cs.main.siz;
    validate_limits(siz, limits)?;

    let mut warnings = container.warnings;
    warnings.extend(cs.warnings.iter().cloned());

    let (tiles_wide, tiles_high) = geometry::tile_grid(siz)?;
    let tile_total = u64::from(tiles_wide) * u64::from(tiles_high);

    // Group tile-parts by tile, keeping codestream appearance order within
    // each tile. TNsot is ADVISORY (Table A.6): real-world streams ship
    // more tile-parts than declared, so extras decode instead of rejecting,
    // recorded in one summary warning per codestream.
    let mut tiles: Vec<Vec<(usize, &markers::TilePart<'_>)>> =
        (0..tile_total).map(|_| Vec::new()).collect();
    for (pos, part) in cs.tile_parts.iter().enumerate() {
        let index = u64::from(part.sot.tile_index);
        if index >= tile_total {
            warnings.push(format!("tile-part for out-of-range tile {index} skipped"));
            continue;
        }
        tiles[index as usize].push((pos, part));
    }
    if let Some(warning) = tnsot_advisory_warning(&tiles) {
        warnings.push(warning);
    }

    // PPM packed headers split per tile-part appearance order (A.7.4).
    let ppm_blobs = if cs.main.ppm.is_empty() {
        None
    } else {
        Some(markers::split_packed_headers(
            &cs.main.ppm,
            cs.tile_parts.len(),
        )?)
    };

    let mut assembler = color::ImageAssembler::new(siz, container.header.as_ref(), limits)?;
    for (tile_index, parts) in tiles.iter().enumerate() {
        if parts.is_empty() {
            // A tile without tile-parts renders as background (leniency).
            continue;
        }
        // A.4.2 requires TPsot order within a tile; be lenient and keep the
        // appearance order, but say so.
        if parts
            .windows(2)
            .any(|pair| pair[0].1.sot.tile_part_index > pair[1].1.sot.tile_part_index)
        {
            warnings.push(format!(
                "tile {tile_index}: tile-parts out of TPsot order; using appearance order"
            ));
        }

        let part_refs: Vec<&markers::TilePart<'_>> = parts.iter().map(|(_, part)| *part).collect();
        let overrides = markers::merge_tile_overrides(&part_refs)?;
        let tile_coding = markers::resolve_tile_coding(&cs.main, &overrides)?;

        // Tile index -> grid position (Equation (B-6)) -> tile rect
        // (Equations (B-7)..(B-10)).
        let p = tile_index as u32 % tiles_wide;
        let q = tile_index as u32 / tiles_wide;
        let tile_rect = geometry::tile_rect(siz, p, q);
        if tile_rect.is_empty() {
            continue;
        }

        let mut components = Vec::with_capacity(siz.components.len());
        for (index, component) in siz.components.iter().enumerate() {
            let coding = markers::resolve_component_coding(&cs.main, &overrides, index as u16)?;
            let geometry = geometry::tile_component_geometry(tile_rect, component, &coding.style)?;
            components.push(packet::ComponentContext {
                geometry,
                coding,
                xrsiz: component.xrsiz,
                yrsiz: component.yrsiz,
            });
        }

        // Packets flow across tile-part boundaries: concatenate bodies in
        // decoding order (B.11).
        let bitstream: Vec<u8> = parts
            .iter()
            .flat_map(|(_, part)| part.body.iter().copied())
            .collect();
        let packed_headers = packed_headers_for_tile(parts, &overrides, ppm_blobs.as_deref());

        let ctx = packet::TileDecodeContext {
            components,
            tile_rect,
            progression: tile_coding.progression,
            layers: tile_coding.layers,
            poc: tile_coding.poc.clone(),
            sop_markers: tile_coding.sop_markers,
            eph_markers: tile_coding.eph_markers,
            bitstream: &bitstream,
            packed_headers: packed_headers.as_deref(),
        };
        let mut packets = packet::read_tile_packets(&ctx, limits)?;
        warnings.append(&mut packets.warnings);

        let mut canvases = Vec::with_capacity(ctx.components.len());
        for (index, context) in ctx.components.iter().enumerate() {
            let component_packets = packets
                .components
                .get(index)
                .ok_or_else(|| JpxError::Malformed("tier-2 produced too few components".into()))?;
            let mut bands = Vec::with_capacity(component_packets.bands.len());
            for band in &component_packets.bands {
                let mut blocks = Vec::with_capacity(band.blocks.len());
                for block in &band.blocks {
                    let coefficients = t1::decode_code_block(block, &bitstream)?;
                    if coefficients.corrupt {
                        // One warning per damaged block; its partially
                        // decoded coefficients stay (leniency doctrine).
                        warnings.push(format!(
                            "tile {tile_index} component {index}: corrupt code-block \
                             [{}, {}) x [{}, {}) kept partially decoded",
                            block.rect.x0, block.rect.x1, block.rect.y0, block.rect.y1,
                        ));
                    }
                    blocks.push(coefficients);
                }
                bands.push(t1::BandCoefficients {
                    kind: band.kind,
                    level: band.level,
                    rect: band.rect,
                    blocks,
                });
            }
            let mut canvas = dequant::dequantize_tile_component(
                &context.geometry,
                &context.coding,
                &siz.components[index],
                &bands,
                limits,
            )?;
            dwt::inverse(&mut canvas)?;
            canvases.push(canvas);
        }
        assembler.push_tile(tile_rect, tile_coding.mct, canvases)?;
    }
    assembler.finish(warnings)
}

/// One summary warning per codestream when tiles ship more tile-parts
/// than their declared TNsot. The count is advisory (T.800 Table A.6),
/// so the surplus decodes; only this note records it (A.4.2 leniency).
///
/// A tile counts as affected when it holds more parts than some declared
/// TNsot, or when a TPsot index reaches the declared count (a surplus
/// index can appear even in a truncated tile that kept few parts).
fn tnsot_advisory_warning(tiles: &[Vec<(usize, &markers::TilePart<'_>)>]) -> Option<String> {
    let affected = tiles
        .iter()
        .filter(|parts| {
            parts.iter().any(|(_, part)| {
                let declared = part.sot.tile_part_count;
                declared != 0
                    && (parts.len() > usize::from(declared) || part.sot.tile_part_index >= declared)
            })
        })
        .count();
    if affected == 0 {
        return None;
    }
    Some(format!(
        "{affected} tile(s) ship more tile-parts than their declared TNsot; \
         the count is advisory (T.800 A.4.2)"
    ))
}

/// Enforces [`DecodeLimits`] against the SIZ header before any
/// size-derived allocation (`max_decoded_bytes` is enforced later, by the
/// colour stage, right before the output buffer is sized).
fn validate_limits(siz: &markers::Siz, limits: &DecodeLimits) -> Result<()> {
    let image = geometry::Rect {
        x0: siz.xosiz,
        y0: siz.yosiz,
        x1: siz.xsiz,
        y1: siz.ysiz,
    };
    let pixels = u64::from(image.width()) * u64::from(image.height());
    if pixels > limits.max_pixels {
        return Err(JpxError::LimitExceeded {
            what: "max_pixels",
            actual: pixels,
            limit: limits.max_pixels,
        });
    }
    let components = siz.components.len() as u64;
    if components > u64::from(limits.max_components) {
        return Err(JpxError::LimitExceeded {
            what: "max_components",
            actual: components,
            limit: u64::from(limits.max_components),
        });
    }
    let (tiles_wide, tiles_high) = geometry::tile_grid(siz)?;
    let tiles = u64::from(tiles_wide) * u64::from(tiles_high);
    if tiles > u64::from(limits.max_tiles) {
        return Err(JpxError::LimitExceeded {
            what: "max_tiles",
            actual: tiles,
            limit: u64::from(limits.max_tiles),
        });
    }
    Ok(())
}

/// Selects the packed packet headers for one tile: PPM blobs (already
/// split per tile-part, A.7.4) win over PPT segments (A.7.5); `None` means
/// the packet headers sit in the tile bit stream itself.
fn packed_headers_for_tile(
    parts: &[(usize, &markers::TilePart<'_>)],
    overrides: &markers::TileOverrides,
    ppm_blobs: Option<&[Vec<u8>]>,
) -> Option<Vec<u8>> {
    if let Some(blobs) = ppm_blobs {
        let mut buffer = Vec::new();
        for (pos, _) in parts {
            if let Some(blob) = blobs.get(*pos) {
                buffer.extend_from_slice(blob);
            }
        }
        return Some(buffer);
    }
    if overrides.ppt.is_empty() {
        return None;
    }
    // merge_tile_overrides already ordered the PPT segments (decoding
    // order, Zppt-sorted within each tile-part header).
    Some(
        overrides
            .ppt
            .iter()
            .flat_map(|segment| segment.data.iter().copied())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The B.4 worked example SIZ (see geometry::tests): 1432 x 954 grid,
    /// image offset (152, 234), 396 x 297 tiles, two components.
    fn example_siz() -> markers::Siz {
        markers::Siz {
            rsiz: 0,
            xsiz: 1432,
            ysiz: 954,
            xosiz: 152,
            yosiz: 234,
            xtsiz: 396,
            ytsiz: 297,
            xtosiz: 0,
            ytosiz: 0,
            components: vec![
                markers::SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 1,
                    yrsiz: 1,
                },
                markers::SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 2,
                    yrsiz: 2,
                },
            ],
        }
    }

    #[test]
    fn limits_default_to_the_documented_bounds() {
        let limits = DecodeLimits::default();
        assert_eq!(limits.max_pixels, 134_217_728); // 1 << 27
        assert_eq!(limits.max_components, 16);
        assert_eq!(limits.max_tiles, 65_535);
        assert_eq!(limits.max_decoded_bytes, 1_073_741_824); // 1 << 30
    }

    #[test]
    fn validate_limits_accepts_the_b4_example_under_defaults() {
        validate_limits(&example_siz(), &DecodeLimits::default()).unwrap();
    }

    #[test]
    fn validate_limits_measures_the_image_region() {
        // Image region: (1432 - 152) x (954 - 234) = 1280 x 720 = 921 600
        // reference-grid pixels.
        let limits = DecodeLimits {
            max_pixels: 921_599,
            ..DecodeLimits::default()
        };
        match validate_limits(&example_siz(), &limits) {
            Err(JpxError::LimitExceeded {
                what,
                actual,
                limit,
            }) => {
                assert_eq!(what, "max_pixels");
                assert_eq!(actual, 921_600);
                assert_eq!(limit, 921_599);
            }
            other => panic!("expected max_pixels breach, got {other:?}"),
        }
    }

    #[test]
    fn validate_limits_counts_components_and_tiles() {
        // Two components; 4 x 4 = 16 tiles (B.4 example).
        let limits = DecodeLimits {
            max_components: 1,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            validate_limits(&example_siz(), &limits),
            Err(JpxError::LimitExceeded {
                what: "max_components",
                actual: 2,
                ..
            })
        ));
        let limits = DecodeLimits {
            max_tiles: 15,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            validate_limits(&example_siz(), &limits),
            Err(JpxError::LimitExceeded {
                what: "max_tiles",
                actual: 16,
                ..
            })
        ));
    }

    /// A minimal tile-part carrying only the SOT fields the advisory
    /// summary inspects.
    fn part(tile_part_index: u8, tile_part_count: u8) -> markers::TilePart<'static> {
        markers::TilePart {
            sot: markers::Sot {
                tile_index: 0,
                tile_part_length: 0,
                tile_part_index,
                tile_part_count,
            },
            overrides: markers::TileOverrides::default(),
            body: &[],
        }
    }

    fn grouped<'a>(
        tiles: &'a [Vec<markers::TilePart<'a>>],
    ) -> Vec<Vec<(usize, &'a markers::TilePart<'a>)>> {
        tiles
            .iter()
            .map(|parts| parts.iter().enumerate().collect())
            .collect()
    }

    #[test]
    fn tnsot_summary_is_silent_when_the_declared_counts_hold() {
        // Matching counts, an unsignalled TNsot = 0, and an empty tile all
        // stay silent.
        let tiles = vec![
            vec![part(0, 2), part(1, 2)],
            vec![part(0, 0), part(1, 0)],
            vec![],
        ];
        assert_eq!(tnsot_advisory_warning(&grouped(&tiles)), None);
    }

    #[test]
    fn tnsot_summary_counts_affected_tiles_once_per_codestream() {
        // Tile 0: three parts against a declared TNsot of 2. Tile 1: only
        // one part kept, but its TPsot = 5 sits beyond the declared 2.
        // Tile 2 is honest. One warning, two tiles counted.
        let tiles = vec![
            vec![part(0, 2), part(1, 2), part(2, 2)],
            vec![part(5, 2)],
            vec![part(0, 1)],
        ];
        assert_eq!(
            tnsot_advisory_warning(&grouped(&tiles)).as_deref(),
            Some(
                "2 tile(s) ship more tile-parts than their declared TNsot; \
                 the count is advisory (T.800 A.4.2)"
            )
        );
    }
}
