//! Tier-2 decoding (ITU-T T.800 B.9-B.12): packet headers, progression
//! iterators, and per-code-block collection of compressed codeword
//! segments across layers — the packet → t1 seam.

use crate::error::{JpxError, Result};
use crate::geometry::{BandKind, Rect, TileComponentGeometry};
use crate::markers::{ComponentCoding, PocSegment, ProgressionOrder, QuantizationStyle};
use crate::tagtree::{BitReader, TagTree};
use crate::DecodeLimits;

/// One component's inputs to Tier-2: its Annex B partition plus its
/// resolved coding parameters.
pub(crate) struct ComponentContext {
    /// Full tile-component partition (geometry stage output).
    pub geometry: TileComponentGeometry,
    /// Resolved COD/COC + QCD/QCC + RGN parameters (markers stage output).
    pub coding: ComponentCoding,
    /// Horizontal sub-sampling XRsiz (Table A.11): the B.12.1.3-5
    /// positional walks fire precincts at reference-grid coordinates,
    /// which scale by the component's sub-sampling.
    pub xrsiz: u8,
    /// Vertical sub-sampling YRsiz (Table A.11).
    pub yrsiz: u8,
}

/// Everything Tier-2 needs to read one tile's packets.
pub(crate) struct TileDecodeContext<'a> {
    /// Per-component geometry and coding, codestream component order.
    pub components: Vec<ComponentContext>,
    /// The tile's REFERENCE-GRID rect (Equations (B-7)..(B-10)): the
    /// B.12.1.3-5 positional walks range over its `(tx0..tx1, ty0..ty1)`
    /// and fall back to its edges for unaligned first precincts.
    pub tile_rect: Rect,
    /// Progression order in force before any POC applies (Table A.16).
    pub progression: ProgressionOrder,
    /// Layer count (SGcod).
    pub layers: u16,
    /// POC chain; when non-empty it REPLACES `progression` for the packets
    /// it spans (A.6.6, B.12).
    pub poc: Vec<PocSegment>,
    /// SOP marker segments may precede packets (A.8.1); resynchronization
    /// points under the leniency doctrine.
    pub sop_markers: bool,
    /// An EPH marker terminates every packet header (A.8.2).
    pub eph_markers: bool,
    /// The tile's bit stream: all tile-part bodies concatenated in
    /// decoding order — packets flow across tile-part boundaries (B.11).
    pub bitstream: &'a [u8],
    /// Packed packet headers for this tile from PPM/PPT (A.7.4/A.7.5), in
    /// tile-part order; when present, packet headers are read from here
    /// and `bitstream` carries only packet bodies.
    pub packed_headers: Option<&'a [u8]>,
}

/// One contiguous compressed contribution to a code-block (B.10.7): a byte
/// range plus the coding passes it carries.
// Constructed by the packet stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CodeBlockSegment {
    /// Start offset into [`TileDecodeContext::bitstream`].
    pub start: usize,
    /// Byte length (B-19 length signalling, Lblock state).
    pub len: usize,
    /// Number of coding passes covered (Table B.4 codewords).
    pub passes: u32,
    /// The entropy coder terminates at the end of this contribution (D.4:
    /// per-pass termination, predictable termination, or a bypass-mode
    /// boundary per Table D.9). A non-terminated contribution concatenates
    /// with the next one before Tier-1 sees it.
    pub terminated: bool,
}

/// Everything Tier-1 needs for one code-block — the packet → t1 seam.
// Constructed by the packet stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct CodeBlockInput {
    /// Code-block rect in ABSOLUTE band coordinates, clipped to
    /// band ∩ precinct (B.7).
    pub rect: Rect,
    /// Owning band kind: selects the Annex D context assignment
    /// (Tables D.1-D.4).
    pub band: BandKind,
    /// Missing most-significant bit-planes P from the zero-bit-plane tag
    /// tree (B.10.5); those planes are all zero.
    pub missing_msbs: u32,
    /// Total magnitude bit-planes Mb = G + epsilon_b - 1 (Equation (E-2)),
    /// raised by the RGN maxshift when one is in force (A.6.3, H.2).
    pub magnitude_bits: u8,
    /// Code-block style flags (Table A.19) governing bypass, resets,
    /// termination, vertical causality and segmentation symbols.
    pub style: u8,
    /// Codeword segments in layer order (B.10.7); empty when the block
    /// never contributed to any packet.
    pub segments: Vec<CodeBlockSegment>,
}

/// All code-blocks of one sub-band, tagged with what dequantization needs
/// to place and scale them.
// Constructed by the packet stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct BandBlocks {
    /// Band kind (Table B.1 offsets / Table E.1 gain).
    pub kind: BandKind,
    /// Decomposition level nb of the band (B-15, Equation (E-5)).
    pub level: u8,
    /// Absolute band rect (B-15).
    pub rect: Rect,
    /// Code-blocks in geometry order (precinct raster order, then raster
    /// order within each precinct — the B.9 packet order).
    pub blocks: Vec<CodeBlockInput>,
}

/// One component's Tier-2 output.
// Constructed by the packet stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ComponentPackets {
    /// Bands in the packet/SPqcd order: resolution 0's LL first, then per
    /// resolution r > 0 the HL, LH, HH triple (B.9).
    pub bands: Vec<BandBlocks>,
}

/// Tier-2 result for one tile.
// Constructed by the packet stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TilePackets {
    /// Per component, codestream order (parallel to
    /// [`TileDecodeContext::components`]).
    pub components: Vec<ComponentPackets>,
    /// Soft findings: corrupt packet headers zero the rest of their scope
    /// and are reported here (leniency doctrine).
    pub warnings: Vec<String>,
}

/// Reads every packet of one tile in progression order (B.12: the five
/// base orders plus POC changes), decoding packet headers (B.10: zero
/// length, inclusion tag trees, zero bit-planes, pass counts, lengths) and
/// accumulating each code-block's codeword segments across layers.
///
/// After the first packet, corruption degrades to warnings: the remaining
/// packets of the damaged scope are treated as empty and decoding
/// continues (a partial image beats none). `limits` bounds every
/// allocation derived from header counts.
pub(crate) fn read_tile_packets(
    ctx: &TileDecodeContext<'_>,
    limits: &DecodeLimits,
) -> Result<TilePackets> {
    decode_tile_packets(ctx, limits).map(|outcome| outcome.packets)
}

/// [`read_tile_packets`] plus the final cursor positions, so tests can
/// prove the parse consumed the tile bit stream exactly.
#[derive(Debug)]
struct Tier2Outcome {
    packets: TilePackets,
    /// One past the last consumed packet-header byte (into the packed
    /// header stream when PPM/PPT is in force, else into the bitstream).
    // Read by the boundary-exactness tests only.
    #[allow(dead_code)]
    header_end: usize,
    /// One past the last consumed packet-body byte of the bitstream.
    // Read by the boundary-exactness tests only.
    #[allow(dead_code)]
    body_end: usize,
}

fn decode_tile_packets(ctx: &TileDecodeContext<'_>, limits: &DecodeLimits) -> Result<Tier2Outcome> {
    let headers = ctx.packed_headers.unwrap_or(ctx.bitstream);
    let (components, state, slot_total) = build_state(ctx);
    // The iteration budget: every real packet costs at least one header
    // bit, so anything far beyond 8 bits/byte worth of packets (plus the
    // per-volume slot walks a hostile POC could demand) is unfulfillable.
    let budget = 4_194_304u64
        .saturating_add(slot_total.saturating_mul(64))
        .saturating_add((headers.len() as u64).saturating_mul(8 * 64));
    let mut tier2 = Tier2 {
        ctx,
        limits,
        headers,
        packed: ctx.packed_headers.is_some(),
        header_pos: 0,
        body_pos: 0,
        components,
        state,
        warnings: Vec::new(),
        nsop_expected: 0,
        sop_warned: false,
        eph_warned: false,
        packets_done: 0,
        segment_count: 0,
    };
    let volumes = plan_volumes(&ctx.components, ctx.progression, ctx.layers, &ctx.poc);
    let mut sequencer = PacketSequencer::new(&ctx.components, ctx.tile_rect, volumes, budget);
    loop {
        let step = match sequencer.next_packet() {
            Ok(step) => step,
            Err(error) => {
                tier2.soften(error, None)?;
                break;
            }
        };
        let Some((layer, slot)) = step else { break };
        if let Err(error) = tier2.parse_packet(slot, layer) {
            tier2.soften(error, Some((layer, slot)))?;
            break;
        }
    }
    Ok(Tier2Outcome {
        packets: TilePackets {
            components: tier2.components,
            warnings: tier2.warnings,
        },
        header_end: tier2.header_pos,
        body_end: tier2.body_pos,
    })
}

// ---------------------------------------------------------------------
// Tier-2 parse state
// ---------------------------------------------------------------------

/// Per-code-block header-decoding state that persists across layers
/// (B.10.4, B.10.7.1).
#[derive(Clone, Copy)]
struct BlockParseState {
    /// The block appeared in an earlier packet: inclusion is one bit now.
    included: bool,
    /// Lblock length-indicator state, initially 3 (B.10.7.1).
    lblock: u32,
    /// Coding passes accumulated so far (indexes Table D.9 boundaries).
    passes: u32,
    /// Zero bit-planes P once known (B.10.5).
    missing: u32,
}

/// Per-precinct tag trees (B.10.2 causality: state persists across the
/// packets — layers — of the same precinct).
struct PrecinctState {
    inclusion: TagTree,
    zero_planes: TagTree,
    /// Index of this precinct's first block in the band-wide block list.
    base: usize,
}

/// Parse state of one sub-band (one entry per flattened band).
struct BandParseState {
    precincts: Vec<PrecinctState>,
    blocks: Vec<BlockParseState>,
    /// The band's (E-2) plane budget — every block shares it.
    magnitude_bits: u8,
}

/// Flattened band index in the B.9/SPqcd order: LL, then per resolution
/// r > 0 its HL, LH, HH triple.
fn flat_band_index(res: usize, band: usize) -> usize {
    if res == 0 {
        0
    } else {
        1 + 3 * (res - 1) + band
    }
}

/// Band exponent epsilon_b: signalled per band (Tables A.28-A.30) or
/// derived from the NL-LL pair via Equation (E-5),
/// `eps_b = eps_0 - NL + nb`. Truncated per-band lists fall back to the
/// derivation from their first entry.
fn band_exponent(coding: &ComponentCoding, levels: u8, level: u8, flat: usize) -> u32 {
    let derive =
        |first: u8| (u32::from(first) + u32::from(level)).saturating_sub(u32::from(levels));
    match &coding.quant.style {
        QuantizationStyle::None { exponents } => exponents
            .get(flat)
            .copied()
            .map(u32::from)
            .or_else(|| exponents.first().map(|&first| derive(first))),
        QuantizationStyle::ScalarDerived { exponent, .. } => Some(derive(*exponent)),
        QuantizationStyle::ScalarExpounded { steps } => steps
            .get(flat)
            .map(|step| u32::from(step.exponent))
            .or_else(|| steps.first().map(|step| derive(step.exponent))),
    }
    .unwrap_or(0)
}

/// Mb = G + eps_b - 1 (Equation (E-2)), raised by the RGN maxshift when
/// one is in force (A.6.3, H.2); clamped to the seam's u8.
fn band_magnitude_bits(coding: &ComponentCoding, levels: u8, level: u8, flat: usize) -> u8 {
    let exponent = band_exponent(coding, levels, level, flat);
    let mb = (u32::from(coding.quant.guard_bits) + exponent).saturating_sub(1)
        + u32::from(coding.roi_shift.unwrap_or(0));
    mb.min(255) as u8
}

/// Builds the output skeleton (every code-block of every band, no segments
/// yet) plus the parallel parse state, and counts the precinct slots.
fn build_state(
    ctx: &TileDecodeContext<'_>,
) -> (Vec<ComponentPackets>, Vec<Vec<BandParseState>>, u64) {
    let mut components = Vec::with_capacity(ctx.components.len());
    let mut state = Vec::with_capacity(ctx.components.len());
    let mut slot_total = 0u64;
    for component in &ctx.components {
        let geometry = &component.geometry;
        let coding = &component.coding;
        let style = coding.style.code_block_style;
        let band_count = 1 + 3 * usize::from(geometry.levels);
        let mut bands = Vec::with_capacity(band_count);
        let mut band_states = Vec::with_capacity(band_count);
        for (res, resolution) in geometry.resolutions.iter().enumerate() {
            slot_total +=
                u64::from(resolution.precincts_wide) * u64::from(resolution.precincts_high);
            for (band_index, band) in resolution.bands.iter().enumerate() {
                let flat = flat_band_index(res, band_index);
                let magnitude_bits = band_magnitude_bits(coding, geometry.levels, band.level, flat);
                let mut blocks = Vec::new();
                let mut parse_blocks = Vec::new();
                let mut precincts = Vec::with_capacity(band.precincts.len());
                for grid in &band.precincts {
                    precincts.push(PrecinctState {
                        inclusion: TagTree::new(grid.blocks_wide, grid.blocks_high),
                        zero_planes: TagTree::new(grid.blocks_wide, grid.blocks_high),
                        base: blocks.len(),
                    });
                    for rect in &grid.blocks {
                        blocks.push(CodeBlockInput {
                            rect: *rect,
                            band: band.kind,
                            missing_msbs: 0,
                            magnitude_bits,
                            style,
                            segments: Vec::new(),
                        });
                        parse_blocks.push(BlockParseState {
                            included: false,
                            lblock: 3,
                            passes: 0,
                            missing: 0,
                        });
                    }
                }
                bands.push(BandBlocks {
                    kind: band.kind,
                    level: band.level,
                    rect: band.rect,
                    blocks,
                });
                band_states.push(BandParseState {
                    precincts,
                    blocks: parse_blocks,
                    magnitude_bits,
                });
            }
        }
        components.push(ComponentPackets { bands });
        state.push(band_states);
    }
    (components, state, slot_total)
}

/// One codeword segment of the packet being parsed, before its byte range
/// is anchored.
struct SegmentPlan {
    passes: u32,
    length: usize,
    terminated: bool,
}

/// One included code-block's contribution to the packet being parsed;
/// committed only after the whole header decodes and the body fits.
struct PlanEntry {
    flat: usize,
    index: usize,
    first_missing: Option<u32>,
    added_passes: u32,
    lblock: u32,
    segments: Vec<SegmentPlan>,
}

/// The Tier-2 engine for one tile: cursors over the header and body
/// streams plus the accumulated output.
struct Tier2<'a, 'b> {
    ctx: &'b TileDecodeContext<'a>,
    limits: &'b DecodeLimits,
    /// Header byte source: the PPM/PPT stream when packed, else the tile
    /// bit stream itself (B.10).
    headers: &'b [u8],
    packed: bool,
    header_pos: usize,
    body_pos: usize,
    components: Vec<ComponentPackets>,
    state: Vec<Vec<BandParseState>>,
    warnings: Vec<String>,
    /// Nsop counts every packet whether or not an SOP appears (A.8.1).
    nsop_expected: u32,
    sop_warned: bool,
    eph_warned: bool,
    packets_done: u64,
    segment_count: u64,
}

impl Tier2<'_, '_> {
    /// Applies the leniency doctrine to a mid-tile failure: before the
    /// first packet (or on a limit breach) the error stays hard; afterwards
    /// it becomes a warning and the remaining packets stay empty.
    fn soften(&mut self, error: JpxError, at: Option<(u32, Slot)>) -> Result<()> {
        if self.packets_done == 0 || matches!(error, JpxError::LimitExceeded { .. }) {
            return Err(error);
        }
        let scope = match at {
            Some((layer, slot)) => format!(
                "packet (layer {layer}, component {}, resolution {}, precinct {})",
                slot.comp, slot.res, slot.precinct
            ),
            None => "packet progression".to_string(),
        };
        self.warnings.push(format!(
            "{scope}: {error}; the remaining packets of this tile are treated as empty"
        ));
        Ok(())
    }

    /// Consumes an SOP marker segment when one precedes the packet (A.8.1)
    /// and verifies its sequence number; the expected count advances for
    /// every packet whether or not the marker appears.
    fn check_sop(&mut self) -> Result<()> {
        let expected = self.nsop_expected % 65536;
        self.nsop_expected = self.nsop_expected.wrapping_add(1);
        if !self.ctx.sop_markers {
            return Ok(());
        }
        let stream = self.ctx.bitstream;
        let at = self.body_pos;
        let Some(window) = stream.get(at..at.saturating_add(6)) else {
            return Ok(());
        };
        if window[0] != 255 || window[1] != 145 {
            return Ok(());
        }
        let length = u32::from(window[2]) * 256 + u32::from(window[3]);
        if length != 4 {
            return Err(JpxError::Malformed(
                "SOP marker segment with Lsop != 4 (A.8.1)".into(),
            ));
        }
        let number = u32::from(window[4]) * 256 + u32::from(window[5]);
        if number != expected {
            if !self.sop_warned {
                self.warnings.push(format!(
                    "SOP sequence number {number} where {expected} was expected; \
                     resynchronizing (A.8.1)"
                ));
                self.sop_warned = true;
            }
            self.nsop_expected = number.wrapping_add(1);
        }
        self.body_pos = at + 6;
        if !self.packed {
            self.header_pos = self.body_pos;
        }
        Ok(())
    }

    /// Consumes the EPH marker after a packet header when the coding style
    /// demands one (A.8.2); a missing marker is a warning, not a failure.
    fn consume_eph(&mut self) {
        if !self.ctx.eph_markers {
            return;
        }
        let at = self.header_pos;
        if self.headers.get(at..at.saturating_add(2)) == Some([255u8, 146].as_slice()) {
            self.header_pos = at + 2;
        } else if !self.eph_warned {
            self.warnings
                .push("EPH marker signalled but missing after a packet header (A.8.2)".into());
            self.eph_warned = true;
        }
    }

    /// Parses one packet: the B.10.8 header walk, then anchors the body
    /// byte ranges (B.9: header order equals body order).
    fn parse_packet(&mut self, slot: Slot, layer: u32) -> Result<()> {
        self.check_sop()?;
        let ctx = self.ctx;
        let start = self.header_pos;
        let mut reader = BitReader::new(self.headers.get(start..).unwrap_or(&[]));
        let mut plan: Vec<PlanEntry> = Vec::new();
        // B.10.3: the first bit selects empty (0) vs non-empty (1).
        if reader.read_bit()? == 1 {
            let resolution = &ctx.components[slot.comp].geometry.resolutions[slot.res];
            let style = ctx.components[slot.comp].coding.style.code_block_style;
            for (band_index, band) in resolution.bands.iter().enumerate() {
                let flat = flat_band_index(slot.res, band_index);
                let grid = &band.precincts[slot.precinct];
                let entries = self.parse_band_blocks(
                    &mut reader,
                    slot,
                    layer,
                    flat,
                    style,
                    grid.blocks_wide,
                    grid.blocks_high,
                )?;
                plan.extend(entries);
            }
        }
        // B.10.1: pack out to the byte boundary (swallowing the stuffed
        // byte after a trailing 0xFF).
        reader.align()?;
        self.header_pos = start + reader.byte_position();
        self.consume_eph();
        self.commit(slot, plan)?;
        self.packets_done += 1;
        Ok(())
    }

    /// The per-band block walk of B.10.8: inclusion, zero bit-planes, pass
    /// count, Lblock growth and the (B-19) lengths for every code-block of
    /// `slot`'s precinct restricted to one band, in raster order.
    #[allow(clippy::too_many_arguments)]
    fn parse_band_blocks(
        &mut self,
        reader: &mut BitReader<'_>,
        slot: Slot,
        layer: u32,
        flat: usize,
        style: u8,
        blocks_wide: u32,
        blocks_high: u32,
    ) -> Result<Vec<PlanEntry>> {
        let mut entries = Vec::new();
        let band_state = &mut self.state[slot.comp][flat];
        let magnitude_bits = band_state.magnitude_bits;
        let BandParseState {
            precincts, blocks, ..
        } = band_state;
        let precinct = &mut precincts[slot.precinct];
        for row in 0..blocks_high {
            for column in 0..blocks_wide {
                let index = precinct.base + (row * blocks_wide + column) as usize;
                let parse = blocks[index];
                let mut first_missing = None;
                let included = if parse.included {
                    // B.10.4: previously included blocks use a single bit.
                    reader.read_bit()? == 1
                } else {
                    // B.10.4: first inclusion comes from the tag tree whose
                    // values are the first contributing layer.
                    let now = precinct.inclusion.decode(reader, column, row, layer + 1)?;
                    if now {
                        // B.10.5: zero bit-planes from the second tag tree —
                        // raise the threshold until the value resolves.
                        let mut missing = None;
                        for threshold in 1..=MAX_MISSING_MSBS + 1 {
                            if precinct
                                .zero_planes
                                .decode(reader, column, row, threshold)?
                            {
                                missing = Some(threshold - 1);
                                break;
                            }
                        }
                        let Some(value) = missing else {
                            return Err(JpxError::Malformed(
                                "zero bit-plane count exceeds the (E-2) plane budget \
                                 (B.10.5)"
                                    .into(),
                            ));
                        };
                        first_missing = Some(value);
                    }
                    now
                };
                if !included {
                    continue;
                }
                let added = read_pass_count(reader)?;
                if u64::from(parse.passes) + u64::from(added) > MAX_CUMULATIVE_PASSES {
                    return Err(JpxError::Malformed(
                        "cumulative coding passes exceed 3 * Mb - 2 (B.10.6)".into(),
                    ));
                }
                // B.10.7.1: k one-bits grow Lblock before the lengths; the
                // growth is signalled once even for multiple segments
                // (B.10.7.2 NOTE).
                let mut lblock = parse.lblock;
                while reader.read_bit()? == 1 {
                    lblock += 1;
                    if lblock > MAX_LBLOCK {
                        return Err(JpxError::Malformed(
                            "Lblock grew beyond any 32-bit length (B.10.7.1)".into(),
                        ));
                    }
                }
                // B.10.7.2: split the added passes at the Table D.8/D.9
                // termination boundaries; the final pass always closes a
                // segment. A segment is genuinely terminated when its last
                // pass is a boundary or the code-block's very last pass.
                let missing = first_missing.unwrap_or(parse.missing);
                let block_total = total_pass_count(magnitude_bits, missing);
                let mut segments = Vec::new();
                let mut run = 0u32;
                for pass in parse.passes..parse.passes + added {
                    run += 1;
                    let boundary = pass_is_terminated(style, pass);
                    if boundary || pass + 1 == parse.passes + added {
                        let bits = lblock + floor_log2(run);
                        let length = read_length(reader, bits)?;
                        segments.push(SegmentPlan {
                            passes: run,
                            length,
                            terminated: boundary || (block_total > 0 && pass + 1 == block_total),
                        });
                        run = 0;
                    }
                }
                entries.push(PlanEntry {
                    flat,
                    index,
                    first_missing,
                    added_passes: added,
                    lblock,
                    segments,
                });
            }
        }
        Ok(entries)
    }

    /// Anchors the parsed contributions in the tile bit stream (bounds
    /// checked first — nothing is committed for a packet whose body cannot
    /// exist) and advances the cursors.
    fn commit(&mut self, slot: Slot, plan: Vec<PlanEntry>) -> Result<()> {
        let base = if self.packed {
            self.body_pos
        } else {
            self.header_pos
        };
        let mut total = 0usize;
        let mut added_segments = 0u64;
        for entry in &plan {
            for segment in &entry.segments {
                total = total
                    .checked_add(segment.length)
                    .ok_or_else(|| JpxError::Malformed("packet body length overflows".into()))?;
                added_segments += 1;
            }
        }
        let end = base
            .checked_add(total)
            .ok_or_else(|| JpxError::Malformed("packet body length overflows".into()))?;
        if end > self.ctx.bitstream.len() {
            return Err(JpxError::Malformed(
                "packet body overruns the tile bit stream (B.9/B.11)".into(),
            ));
        }
        // Segment records are the one allocation header counts control
        // directly; bound them like decoded output.
        self.segment_count += added_segments;
        let bytes = self
            .segment_count
            .saturating_mul(std::mem::size_of::<CodeBlockSegment>() as u64);
        if bytes > self.limits.max_decoded_bytes {
            return Err(JpxError::LimitExceeded {
                what: "max_decoded_bytes",
                actual: bytes,
                limit: self.limits.max_decoded_bytes,
            });
        }
        let mut cursor = base;
        for entry in plan {
            let output = &mut self.components[slot.comp].bands[entry.flat].blocks[entry.index];
            let state = &mut self.state[slot.comp][entry.flat].blocks[entry.index];
            if let Some(missing) = entry.first_missing {
                output.missing_msbs = missing;
                state.missing = missing;
                state.included = true;
            }
            state.lblock = entry.lblock;
            state.passes += entry.added_passes;
            for segment in entry.segments {
                output.segments.push(CodeBlockSegment {
                    start: cursor,
                    len: segment.length,
                    passes: segment.passes,
                    terminated: segment.terminated,
                });
                cursor += segment.length;
            }
        }
        self.body_pos = cursor;
        if !self.packed {
            self.header_pos = cursor;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Packet header primitives (B.10)
// ---------------------------------------------------------------------

/// Zero-bit-plane counts cannot reach the (E-2)/(E-4) plane budget:
/// Mb = G + eps - 1 <= 7 + 31 - 1 = 37, plus at most 255 from the RGN
/// maxshift (Table A.26), so P <= Mb - 1 <= 291.
const MAX_MISSING_MSBS: u32 = 291;

/// A code-block owns at most 3 * Mb - 2 coding passes (B.10.6 NOTE,
/// extended by the RGN maxshift): 3 * 292 - 2.
const MAX_CUMULATIVE_PASSES: u64 = 874;

/// Lblock (B.10.7.1) starts at 3 and only grows; a byte count needs at
/// most 32 bits (Psot itself is 32-bit), so growth beyond this is hostile.
const MAX_LBLOCK: u32 = 35;

/// Number of coding passes (B.10.6, Table B.4): 0 -> 1; 10 -> 2; 11xx for
/// 3..=5; 1111 + 5 bits for 6..=36; 1111 11111 + 7 bits for 37..=164.
fn read_pass_count(reader: &mut BitReader<'_>) -> Result<u32> {
    if reader.read_bit()? == 0 {
        return Ok(1);
    }
    if reader.read_bit()? == 0 {
        return Ok(2);
    }
    let two = reader.read_bits(2)?;
    if two < 3 {
        return Ok(3 + two);
    }
    let five = reader.read_bits(5)?;
    if five < 31 {
        return Ok(6 + five);
    }
    Ok(37 + reader.read_bits(7)?)
}

/// True when the entropy coder is terminated at the END of absolute coding
/// pass `pass` (D.4/D.6). Pass 0 is the code-block's first cleanup pass;
/// afterwards p % 3 == 1 is significance propagation, 2 is magnitude
/// refinement and 0 is cleanup. With the termination-on-each-pass style
/// bit (Table A.19 bit 2, Table D.8) every pass terminates. In bypass mode
/// (bit 0), Table D.9 terminates the fourth cleanup pass (pass 9) and,
/// from there on, every magnitude refinement and cleanup pass — the raw
/// significance propagation passes flow into the following refinement.
fn pass_is_terminated(style: u8, pass: u32) -> bool {
    if style & 4 != 0 {
        return true;
    }
    if style & 1 == 0 {
        return false;
    }
    pass >= 9 && (pass.is_multiple_of(3) || pass % 3 == 2)
}

/// Segment length field (B-19): `bits` may exceed the minimum (B.10.7.1
/// NOTE 2 allows any width), but the value itself must fit a 32-bit byte
/// count — anything larger is hostile.
fn read_length(reader: &mut BitReader<'_>, bits: u32) -> Result<usize> {
    let mut value = 0u64;
    for _ in 0..bits {
        value = (value << 1) | u64::from(reader.read_bit()?);
        if value > u64::from(u32::MAX) {
            return Err(JpxError::Malformed(
                "codeword segment length exceeds 32 bits (B-19)".into(),
            ));
        }
    }
    Ok(value as usize)
}

/// `floor(log2(n))` for `n >= 1` — the (B-19) width contribution.
fn floor_log2(n: u32) -> u32 {
    31 - n.leading_zeros()
}

/// Total pass count of a code-block (D.2/B.10.6): `3 * (Mb - P) - 2`
/// magnitude bit-planes worth of passes; zero when the missing planes
/// swallow the whole budget.
fn total_pass_count(magnitude_bits: u8, missing: u32) -> u32 {
    let planes = u32::from(magnitude_bits).saturating_sub(missing);
    (3 * planes).saturating_sub(2)
}

// ---------------------------------------------------------------------
// Progression (B.12, A.6.6)
// ---------------------------------------------------------------------

/// One packet slot: a (component, resolution level, precinct) triple.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Slot {
    comp: usize,
    res: usize,
    precinct: usize,
}

/// One progression order volume (B.12.2, Equation (B-21)): the POC ranges
/// (or the full ranges when no POC is in force) plus the order that
/// interleaves the packets inside.
struct Volume {
    comp_start: usize,
    comp_end: usize,
    res_start: usize,
    res_end: usize,
    layer_end: u32,
    order: ProgressionOrder,
}

/// The volume chain for one tile (A.6.6/B.12.3): POC segments replace the
/// COD progression outright when present. Ranges are clamped to what the
/// tile actually has — a POC may describe more volume than exists.
fn plan_volumes(
    components: &[ComponentContext],
    progression: ProgressionOrder,
    layers: u16,
    poc: &[PocSegment],
) -> Vec<Volume> {
    // B.12.1.1: Nmax is the largest decomposition level count of any
    // component in the tile; r beyond a component's own NL yields nothing.
    let nmax = components
        .iter()
        .map(|component| usize::from(component.geometry.levels))
        .max()
        .unwrap_or(0);
    let res_cap = nmax + 1;
    let comp_cap = components.len();
    if poc.is_empty() {
        return vec![Volume {
            comp_start: 0,
            comp_end: comp_cap,
            res_start: 0,
            res_end: res_cap,
            layer_end: u32::from(layers),
            order: progression,
        }];
    }
    poc.iter()
        .map(|segment| Volume {
            comp_start: usize::from(segment.comp_start),
            comp_end: usize::from(segment.comp_end).min(comp_cap),
            res_start: usize::from(segment.res_start),
            res_end: usize::from(segment.res_end).min(res_cap),
            layer_end: u32::from(segment.layer_end).min(u32::from(layers)),
            order: segment.order,
        })
        .collect()
}

/// Charges the iteration budget; a spent budget means the progression
/// description demands more work than the codestream could possibly carry.
fn charge(budget: &mut u64, cost: u64) -> Result<()> {
    if *budget < cost {
        return Err(JpxError::Malformed(
            "progression order volume exceeds the iteration budget (B.12.2)".into(),
        ));
    }
    *budget -= cost;
    Ok(())
}

/// Sort key of one precinct in the B.12.1.3-B.12.1.5 spatial walks: the
/// derived fields compare lexicographically (major, y, x, minor).
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct PositionKey {
    major: u64,
    y: u128,
    x: u128,
    minor: u64,
}

/// The reference-grid coordinate at which precinct `index` (row or column)
/// of resolution level `r` fires its packets in the positional orders.
///
/// B.12.1.3-B.12.1.5 walk (x, y) over the tile's reference-grid rect and
/// admit a component at a coordinate divisible by
/// `XRsiz * 2^(PPx + NL - r)` — the precinct partition line mapped through
/// (B-20) — OR at the tile edge `tx0` when the first precinct's line lies
/// outside the tile (`trx0 * 2^(NL - r)` not divisible by
/// `2^(PPx + NL - r)`, i.e. `trx0` not divisible by `2^PPx`). Equivalently:
/// precinct `index` fires where its partition line lands on the reference
/// grid, except that an unaligned first precinct fires at the tile edge.
///
/// Precinct `index` sits in partition row/column `(tr0 >> pp) + index` of
/// the resolution-level grid (anchored at 0, B.6); its partition line maps
/// back to the reference grid by `XRsiz * 2^(NL - r)` (B-12 with (B-14)),
/// giving `((tr0 >> pp) + index) * sub << (pp + NL - r)`. Every component
/// keys on one common reference-grid scale, so mixed sub-samplings
/// interleave exactly.
fn axis_position(tile_edge: u32, tr0: u32, pp: u8, sub: u8, up: u32, index: u32) -> u128 {
    if index == 0 && tr0 & ((1u32 << pp) - 1) != 0 {
        return u128::from(tile_edge);
    }
    ((u128::from(tr0 >> pp) + u128::from(index)) * u128::from(sub)) << (u32::from(pp) + up)
}

/// Number of precinct slots component `comp` owns at resolution `res`
/// (zero when the component has fewer resolution levels, B.12).
fn resolution_slot_count(components: &[ComponentContext], comp: usize, res: usize) -> usize {
    let geometry = &components[comp].geometry;
    if res > usize::from(geometry.levels) {
        return 0;
    }
    let resolution = &geometry.resolutions[res];
    resolution.precincts_wide as usize * resolution.precincts_high as usize
}

/// Appends every precinct slot of `(comp, res)` in raster order — the
/// B.12.1.1/B.12.1.2 "for each k" loop.
fn push_resolution_slots(
    components: &[ComponentContext],
    comp: usize,
    res: usize,
    slots: &mut Vec<Slot>,
    budget: &mut u64,
) -> Result<()> {
    let count = resolution_slot_count(components, comp, res);
    charge(budget, count as u64)?;
    for precinct in 0..count {
        slots.push(Slot {
            comp,
            res,
            precinct,
        });
    }
    Ok(())
}

/// The spatially interleaved orders (B.12.1.3-B.12.1.5): every precinct
/// becomes one single-slot group, ordered by its firing position with the
/// order-specific role of the resolution and component axes.
fn positional_groups(
    components: &[ComponentContext],
    tile_rect: Rect,
    volume: &Volume,
    budget: &mut u64,
) -> Result<Vec<Group>> {
    let mut events: Vec<(PositionKey, Slot)> = Vec::new();
    for (comp, component) in components
        .iter()
        .enumerate()
        .take(volume.comp_end)
        .skip(volume.comp_start)
    {
        let geometry = &component.geometry;
        let res_end = volume.res_end.min(usize::from(geometry.levels) + 1);
        for res in volume.res_start..res_end {
            let resolution = &geometry.resolutions[res];
            if resolution.precincts_wide == 0 || resolution.precincts_high == 0 {
                continue;
            }
            let up = u32::from(geometry.levels) - res as u32;
            for row in 0..resolution.precincts_high {
                let y = axis_position(
                    tile_rect.y0,
                    resolution.rect.y0,
                    resolution.ppy,
                    component.yrsiz,
                    up,
                    row,
                );
                for column in 0..resolution.precincts_wide {
                    charge(budget, 1)?;
                    let x = axis_position(
                        tile_rect.x0,
                        resolution.rect.x0,
                        resolution.ppx,
                        component.xrsiz,
                        up,
                        column,
                    );
                    // (B-20): the precinct index is its raster position.
                    let precinct =
                        row as usize * resolution.precincts_wide as usize + column as usize;
                    let key = match volume.order {
                        // B.12.1.3: r, then position, then component.
                        ProgressionOrder::Rpcl => PositionKey {
                            major: res as u64,
                            y,
                            x,
                            minor: comp as u64,
                        },
                        // B.12.1.4: position, then component, then r.
                        ProgressionOrder::Pcrl => PositionKey {
                            major: 0,
                            y,
                            x,
                            minor: ((comp as u64) << 8) | res as u64,
                        },
                        // B.12.1.5: component, then position, then r.
                        _ => PositionKey {
                            major: comp as u64,
                            y,
                            x,
                            minor: res as u64,
                        },
                    };
                    events.push((
                        key,
                        Slot {
                            comp,
                            res,
                            precinct,
                        },
                    ));
                }
            }
        }
    }
    events.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(events
        .into_iter()
        .map(|(_, slot)| Group {
            layer_end: volume.layer_end,
            slots: vec![slot],
        })
        .collect())
}

/// Builds the layer-loop groups of one volume in its B.12.1 order.
fn volume_groups(
    components: &[ComponentContext],
    tile_rect: Rect,
    volume: &Volume,
    budget: &mut u64,
) -> Result<Vec<Group>> {
    charge(budget, 1)?;
    match volume.order {
        // B.12.1.1: layers outermost over one (r, c, k) walk.
        ProgressionOrder::Lrcp => {
            let mut slots = Vec::new();
            for res in volume.res_start..volume.res_end {
                for comp in volume.comp_start..volume.comp_end {
                    push_resolution_slots(components, comp, res, &mut slots, budget)?;
                }
            }
            Ok(vec![Group {
                layer_end: volume.layer_end,
                slots,
            }])
        }
        // B.12.1.2: one layer loop per resolution level.
        ProgressionOrder::Rlcp => {
            let mut groups = Vec::new();
            for res in volume.res_start..volume.res_end {
                let mut slots = Vec::new();
                for comp in volume.comp_start..volume.comp_end {
                    push_resolution_slots(components, comp, res, &mut slots, budget)?;
                }
                groups.push(Group {
                    layer_end: volume.layer_end,
                    slots,
                });
            }
            Ok(groups)
        }
        _ => positional_groups(components, tile_rect, volume, budget),
    }
}

/// Emits the packets of one tile in progression order, one at a time, with
/// the A.6.6 "no packet is ever repeated" rule across volumes. Lazy in the
/// layer axis so a hostile layer count cannot force an allocation.
struct PacketSequencer<'a> {
    components: &'a [ComponentContext],
    /// Reference-grid tile rect for the B.12.1.3-5 spatial walks.
    tile_rect: Rect,
    volumes: Vec<Volume>,
    volume_index: usize,
    groups_built: bool,
    groups: Vec<Group>,
    group_index: usize,
    layer: u32,
    slot_index: usize,
    /// Next layer each precinct expects (B.12.2: "the layer always starts
    /// with the next one"), indexed `[comp][res][precinct]`.
    next_layer: Vec<Vec<Vec<u32>>>,
    /// Iteration budget: hostile POC chains can describe far more volume
    /// than the codestream can carry; iteration stops when it is spent.
    budget: u64,
}

/// One run of slots that shares a layer loop: the B.12.1 loop nests all
/// reduce to "for each layer of the group, for each slot of the group".
struct Group {
    layer_end: u32,
    slots: Vec<Slot>,
}

impl<'a> PacketSequencer<'a> {
    fn new(
        components: &'a [ComponentContext],
        tile_rect: Rect,
        volumes: Vec<Volume>,
        budget: u64,
    ) -> Self {
        let next_layer = components
            .iter()
            .map(|component| {
                component
                    .geometry
                    .resolutions
                    .iter()
                    .map(|resolution| {
                        let count =
                            resolution.precincts_wide as usize * resolution.precincts_high as usize;
                        vec![0u32; count]
                    })
                    .collect()
            })
            .collect();
        PacketSequencer {
            components,
            tile_rect,
            volumes,
            volume_index: 0,
            groups_built: false,
            groups: Vec::new(),
            group_index: 0,
            layer: 0,
            slot_index: 0,
            next_layer,
            budget,
        }
    }

    /// The next packet as `(layer, slot)`, or `None` when every volume is
    /// exhausted. Combinations a previous volume already delivered are
    /// skipped (B.12.2: no packet is ever repeated; the layer counter of
    /// each precinct decides).
    fn next_packet(&mut self) -> Result<Option<(u32, Slot)>> {
        loop {
            charge(&mut self.budget, 1)?;
            if self.volume_index >= self.volumes.len() {
                return Ok(None);
            }
            if !self.groups_built {
                self.groups = volume_groups(
                    self.components,
                    self.tile_rect,
                    &self.volumes[self.volume_index],
                    &mut self.budget,
                )?;
                self.groups_built = true;
                self.group_index = 0;
                self.layer = 0;
                self.slot_index = 0;
            }
            let Some(group) = self.groups.get(self.group_index) else {
                self.volume_index += 1;
                self.groups_built = false;
                continue;
            };
            if group.slots.is_empty() || self.layer >= group.layer_end {
                self.group_index += 1;
                self.layer = 0;
                self.slot_index = 0;
                continue;
            }
            let Some(&slot) = group.slots.get(self.slot_index) else {
                self.layer += 1;
                self.slot_index = 0;
                continue;
            };
            self.slot_index += 1;
            let next = &mut self.next_layer[slot.comp][slot.res][slot.precinct];
            if *next == self.layer {
                *next += 1;
                return Ok(Some((self.layer, slot)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::{
        CodingStyle, PrecinctExponents, QuantStep, Quantization, SizComponent, WaveletKind,
    };

    // ---- builders ------------------------------------------------------

    fn rect(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn coding_style(
        levels: u8,
        xcb: u8,
        ycb: u8,
        precincts: &[(u8, u8)],
        block_style: u8,
    ) -> CodingStyle {
        CodingStyle {
            decomposition_levels: levels,
            code_block_width_exp: xcb,
            code_block_height_exp: ycb,
            code_block_style: block_style,
            wavelet: WaveletKind::Reversible53,
            precincts: precincts
                .iter()
                .map(|&(ppx, ppy)| PrecinctExponents { ppx, ppy })
                .collect(),
        }
    }

    /// "No quantization" (Table A.28) with the given guard bits and one
    /// exponent per sub-band in Table A.29 order.
    fn quant_none(guard_bits: u8, exponents: Vec<u8>) -> Quantization {
        Quantization {
            guard_bits,
            style: QuantizationStyle::None { exponents },
        }
    }

    fn component_context(
        tile: Rect,
        style: CodingStyle,
        quant: Quantization,
        roi_shift: Option<u8>,
    ) -> ComponentContext {
        subsampled_component_context(tile, 1, 1, style, quant, roi_shift)
    }

    /// A component sub-sampled by (Table A.11) `xrsiz`/`yrsiz` over the
    /// reference-grid tile rect: geometry lands on the component grid
    /// (B-12), positions on the reference grid.
    fn subsampled_component_context(
        tile: Rect,
        xrsiz: u8,
        yrsiz: u8,
        style: CodingStyle,
        quant: Quantization,
        roi_shift: Option<u8>,
    ) -> ComponentContext {
        let component = SizComponent {
            depth: 8,
            signed: false,
            xrsiz,
            yrsiz,
        };
        let geometry = crate::geometry::tile_component_geometry(tile, &component, &style).unwrap();
        ComponentContext {
            geometry,
            coding: ComponentCoding {
                style,
                quant,
                roi_shift,
            },
            xrsiz,
            yrsiz,
        }
    }

    /// Packs a '0'/'1' string (other characters ignored) into bytes with
    /// the B.10.1 bit-stuffing routine: MSB first, zero padding, and a
    /// stuffed zero MSB in the byte following every emitted 0xFF.
    fn bit_bytes(spec: &str) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        let mut current = 0u8;
        let mut used = 0u8;
        for ch in spec.chars() {
            let bit = match ch {
                '0' => 0,
                '1' => 1,
                _ => continue,
            };
            if used == 0 && bytes.last() == Some(&255) {
                used = 1; // the stuffed zero MSB (B.10.1)
            }
            current = (current << 1) | bit;
            used += 1;
            if used == 8 {
                bytes.push(current);
                current = 0;
                used = 0;
            }
        }
        if used > 0 {
            bytes.push(current << (8 - used));
        }
        bytes
    }

    fn segment_tuples(block: &CodeBlockInput) -> Vec<(usize, usize, u32, bool)> {
        block
            .segments
            .iter()
            .map(|segment| {
                (
                    segment.start,
                    segment.len,
                    segment.passes,
                    segment.terminated,
                )
            })
            .collect()
    }

    fn collect_sequence(
        components: &[ComponentContext],
        tile_rect: Rect,
        order: ProgressionOrder,
        layers: u16,
        poc: Vec<PocSegment>,
    ) -> Vec<(u32, usize, usize, usize)> {
        let volumes = plan_volumes(components, order, layers, &poc);
        let mut sequencer = PacketSequencer::new(components, tile_rect, volumes, 1 << 24);
        let mut sequence = Vec::new();
        while let Some((layer, slot)) = sequencer.next_packet().unwrap() {
            sequence.push((layer, slot.comp, slot.res, slot.precinct));
        }
        sequence
    }

    // ---- B.10.6 pass-count codewords ------------------------------------

    #[test]
    fn pass_count_codewords_match_table_b4() {
        // Table B.4, every branch, hand-encoded and concatenated:
        //   1 -> 0;  2 -> 10;  3 -> 1100;  4 -> 1101;  5 -> 1110;
        //   6 -> 1111 00000 (6 + 0);  36 -> 1111 11110 (6 + 30);
        //   37 -> 1111 11111 0000000 (37 + 0);
        //   164 -> 1111 11111 1111111 (37 + 127).
        let data =
            bit_bytes("0 10 1100 1101 1110 111100000 111111110 1111111110000000 1111111111111111");
        let mut reader = BitReader::new(&data);
        for expected in [1u32, 2, 3, 4, 5, 6, 36, 37, 164] {
            assert_eq!(read_pass_count(&mut reader).unwrap(), expected);
        }
    }

    // ---- D.4/D.6 termination boundaries ----------------------------------

    #[test]
    fn termination_boundaries_follow_tables_d8_and_d9() {
        // Pass indices count from the code-block's first pass: 0 is the
        // first cleanup (bit-plane 1 of Table D.9), then each further plane
        // contributes significance propagation (p % 3 == 1), magnitude
        // refinement (p % 3 == 2) and cleanup (p % 3 == 0).
        // No style: termination only at the end of the block (Table D.8),
        // which the caller adds — no per-pass boundary here.
        for pass in 0..24 {
            assert!(!pass_is_terminated(0, pass), "pass {pass}");
        }
        // Termination on each pass (Table A.19 bit 2): every pass.
        for pass in 0..24 {
            assert!(pass_is_terminated(4, pass), "pass {pass}");
        }
        // Bypass (bit 0), Table D.9: the fourth cleanup (pass 9)
        // terminates, then every magnitude refinement (11, 14, ...) and
        // cleanup (12, 15, ...) terminates while the raw significance
        // propagation passes (10, 13, ...) do not.
        let terminated = [9u32, 11, 12, 14, 15, 17, 18];
        for pass in 0..20 {
            assert_eq!(
                pass_is_terminated(1, pass),
                terminated.contains(&pass),
                "pass {pass}"
            );
        }
    }

    // ---- B.10.8 worked example (Figure B.13 / Table B.5) -----------------

    /// The Figure B.13 precinct: one sub-band holding 3 x 2 code-blocks.
    /// A 48 x 32 tile-component with NL = 0 and 16 x 16 code-blocks makes
    /// the LL band exactly that precinct (single maximal precinct).
    fn figure_b13_component() -> ComponentContext {
        // Mb = G + eps - 1 = 2 + 8 - 1 = 9 (E-2): comfortably above the
        // deepest zero-bit-plane count (7) in Figure B.13.
        component_context(
            rect(0, 0, 48, 32),
            coding_style(0, 4, 4, &[], 0),
            quant_none(2, vec![8]),
            None,
        )
    }

    #[test]
    fn header_walkthrough_reproduces_figure_b13_and_table_b5() {
        // Layer-0 packet header, bit for bit from Table B.5:
        //   1        non-zero length packet
        //   111      CB(0,0) first included (inclusion tag tree: root 0,
        //            level-1 node 0, leaf 0 — three 1 bits fix the path)
        //   000111   CB(0,0) missing 3 bit-planes (zbp tag tree: root
        //            raised 0->3 by three 0 bits then fixed; node and leaf
        //            fixed at the floor with one 1 bit each)
        //   1100     3 coding passes (Table B.4)
        //   0        Lblock unchanged (3)
        //   0100     4 bytes long, 3 + floor(log2 3) = 4 bits (B-19)
        //   1        CB(1,0) first included (path already fixed, leaf 1)
        //   01       CB(1,0) missing 4 planes (leaf floored at 3: 0 raises
        //            to 4, 1 fixes)
        //   10       2 passes
        //   10       Lblock += 1 -> 4
        //   00100    4 bytes long, 4 + floor(log2 2) = 5 bits
        //   0        CB(2,0) not yet included (level-1 node (1,0) raised
        //            0 -> 1: partial tag tree)
        //   0        CB(0,1) not yet included (leaf raised to 1)
        //   0        CB(1,1) not yet included (leaf raised to 1)
        //            CB(2,1): no bits — node (1,0) >= 1 already decides it
        let layer0 = bit_bytes("1 111 000111 1100 0 0100 1 01 10 10 00100 0 0 0");
        assert_eq!(layer0, vec![241, 240, 150, 136, 0]);
        // Layer-1 packet header (second half of Table B.5):
        //   1     non-zero; 1 CB(0,0) included again; 1100 3 passes;
        //   0     Lblock unchanged; 1010 10 bytes (4 bits);
        //   0     CB(1,0) not included; 10 CB(2,0) still not included
        //         (node (1,0) fixed at 1, leaf raised to 2);
        //   0     CB(0,1) not included (leaf raised to 2);
        //   1     CB(1,1) first included; 1 missing 3 planes (root/node
        //         already at 3, leaf fixed); 0 one pass; 0 Lblock;
        //   001   1 byte (3 bits);
        //   1     CB(2,1) first included (leaf fixed at 1);
        //   00011 missing 6 planes (node (1,0) floored at 3, raised to 6
        //         by three 0 bits then fixed; leaf fixed);
        //   0     one pass; 0 Lblock; 010 2 bytes (3 bits).
        let layer1 = bit_bytes("1 1 1100 0 1010 0 10 0 1 1 0 0 001 1 00011 0 0 010");
        assert_eq!(layer1, vec![241, 73, 134, 49, 0]);

        // In-stream layout: header 0..5, bodies 5..13 (4 + 4 bytes),
        // header 13..18, bodies 18..31 (10 + 1 + 2 bytes).
        let mut stream = layer0;
        stream.extend_from_slice(&[101, 102, 103, 104, 111, 112, 113, 114]);
        stream.extend_from_slice(&layer1);
        stream.extend_from_slice(&[0u8; 13]);
        assert_eq!(stream.len(), 31);

        let ctx = TileDecodeContext {
            components: vec![figure_b13_component()],
            tile_rect: rect(0, 0, 48, 32),
            progression: ProgressionOrder::Lrcp,
            layers: 2,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings, Vec::<String>::new());
        assert_eq!(outcome.body_end, 31);
        assert_eq!(outcome.header_end, 31);

        let band = &outcome.packets.components[0].bands[0];
        assert_eq!(band.kind, BandKind::Ll);
        assert_eq!(band.blocks.len(), 6);
        // Raster order: (0,0), (1,0), (2,0), (0,1), (1,1), (2,1). None of
        // the packets end the blocks (Mb = 9: e.g. CB(0,0) has
        // 3 * (9 - 3) - 2 = 16 total passes, only 6 arrived).
        assert_eq!(band.blocks[0].missing_msbs, 3);
        assert_eq!(
            segment_tuples(&band.blocks[0]),
            vec![(5, 4, 3, false), (18, 10, 3, false)]
        );
        assert_eq!(band.blocks[1].missing_msbs, 4);
        assert_eq!(segment_tuples(&band.blocks[1]), vec![(9, 4, 2, false)]);
        assert!(band.blocks[2].segments.is_empty());
        assert!(band.blocks[3].segments.is_empty());
        assert_eq!(band.blocks[4].missing_msbs, 3);
        assert_eq!(segment_tuples(&band.blocks[4]), vec![(28, 1, 1, false)]);
        assert_eq!(band.blocks[5].missing_msbs, 6);
        assert_eq!(segment_tuples(&band.blocks[5]), vec![(29, 2, 1, false)]);
        // Every block carries the (E-2) plane budget and the style bits.
        for block in &band.blocks {
            assert_eq!(block.magnitude_bits, 9);
            assert_eq!(block.style, 0);
        }
    }

    #[test]
    fn packed_headers_split_bits_from_bodies() {
        // Same layer-0 header as the Figure B.13 walkthrough, but relocated
        // to a PPM/PPT stream (A.7.4/A.7.5): header BITS come from the
        // packed stream, bodies stay in the tile bit stream, so the
        // segment offsets count from 0.
        let headers = bit_bytes("1 111 000111 1100 0 0100 1 01 10 10 00100 0 0 0");
        let bodies = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let ctx = TileDecodeContext {
            components: vec![figure_b13_component()],
            tile_rect: rect(0, 0, 48, 32),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &bodies,
            packed_headers: Some(&headers),
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings, Vec::<String>::new());
        assert_eq!(outcome.header_end, 5);
        assert_eq!(outcome.body_end, 8);
        let band = &outcome.packets.components[0].bands[0];
        assert_eq!(segment_tuples(&band.blocks[0]), vec![(0, 4, 3, false)]);
        assert_eq!(segment_tuples(&band.blocks[1]), vec![(4, 4, 2, false)]);
    }

    // ---- B.10.7.2 multiple codeword segments (bypass) ---------------------

    #[test]
    fn bypass_mode_splits_codeword_segments_per_the_b_10_7_2_note() {
        // The B.10.7.2 NOTE: bypass mode, a packet carrying the cleanup
        // pass of bit-plane 4 through the significance propagation pass of
        // bit-plane 6 — absolute passes 9..=13. T = {9, 11, 12} from Table
        // D.9 plus the final pass 13, so K = 4 lengths {6, 75, 134, 192}
        // covering {1, 2, 1, 1} passes.
        //
        // Layer 0 first delivers passes 0..=8 (no boundary before pass 9):
        //   1  non-zero; 1 included (1x1 inclusion tree, value 0);
        //   1  zero missing planes (1x1 zbp tree, value 0);
        //   111100011  9 passes (Table B.4: 6 + 0b00011);
        //   0  Lblock unchanged; 010100  20 bytes in
        //      3 + floor(log2 9) = 6 bits.
        let layer0 = bit_bytes("1 1 1 111100011 0 010100");
        assert_eq!(layer0, vec![254, 50, 128]);
        // Layer 1 is the NOTE verbatim:
        //   1 non-zero; 1 included again; 1110  5 passes;
        //   111110  Lblock 3 + 5 = 8;
        //   00000110   6   (8 + floor(log2 1) = 8 bits)
        //   001001011  75  (8 + floor(log2 2) = 9 bits)
        //   10000110   134 (8 bits)
        //   11000000   192 (8 bits)
        let layer1 = bit_bytes("1 1 1110 111110 00000110 001001011 10000110 11000000");
        assert_eq!(layer1, vec![251, 224, 98, 92, 54, 0]);

        let mut stream = layer0;
        stream.extend_from_slice(&[0u8; 20]);
        stream.extend_from_slice(&layer1);
        stream.extend_from_slice(&[0u8; 407]);
        assert_eq!(stream.len(), 436);

        // One 16 x 16 code-block; bypass style (Table A.19 bit 0);
        // Mb = 2 + 9 - 1 = 10 so the block owns 3 * 10 - 2 = 28 passes and
        // pass 13 is NOT its last.
        let component = component_context(
            rect(0, 0, 16, 16),
            coding_style(0, 4, 4, &[], 1),
            quant_none(2, vec![9]),
            None,
        );
        let ctx = TileDecodeContext {
            components: vec![component],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 2,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings, Vec::<String>::new());
        assert_eq!(outcome.body_end, 436);
        let block = &outcome.packets.components[0].bands[0].blocks[0];
        assert_eq!(
            segment_tuples(block),
            vec![
                // Layer 0: single segment, passes 0..=8, nothing terminated.
                (3, 20, 9, false),
                // Layer 1: pass 9 (4th cleanup, terminated), passes 10-11
                // (raw sig-prop + terminated raw mag-ref), pass 12 (cleanup,
                // terminated), pass 13 (raw sig-prop, not terminated).
                (29, 6, 1, true),
                (35, 75, 2, true),
                (110, 134, 1, true),
                (244, 192, 1, false),
            ]
        );
    }

    // ---- B.10.3 zero-length packets --------------------------------------

    #[test]
    fn zero_length_packets_defer_inclusion_to_later_layers() {
        // Layer 0 is an empty packet: a single 0 bit padded to one byte
        // (B.10.3). Layer 1 then includes the block for the first time, so
        // its inclusion tag-tree value is 1 (the first contributing layer,
        // B.10.3 NOTE):
        //   1   non-zero packet
        //   01  inclusion tag tree: 0 raises the 1x1 node to 1, 1 fixes it
        //       (value 1 < threshold 2 -> included now)
        //   001 two missing bit-planes (0 raises to 1, 0 to 2, 1 fixes)
        //   0   one pass; 0 Lblock unchanged; 101  5 bytes in 3 bits.
        let layer1 = bit_bytes("1 01 001 0 0 101");
        assert_eq!(layer1, vec![164, 160]);
        let mut stream = vec![0u8];
        stream.extend_from_slice(&layer1);
        stream.extend_from_slice(&[9, 9, 9, 9, 9]);

        let component = component_context(
            rect(0, 0, 16, 16),
            coding_style(0, 4, 4, &[], 0),
            quant_none(2, vec![8]),
            None,
        );
        let ctx = TileDecodeContext {
            components: vec![component],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 2,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings, Vec::<String>::new());
        assert_eq!(outcome.body_end, 8);
        let block = &outcome.packets.components[0].bands[0].blocks[0];
        assert_eq!(block.missing_msbs, 2);
        assert_eq!(segment_tuples(block), vec![(3, 5, 1, false)]);
    }

    // ---- A.8.1/A.8.2 SOP and EPH markers ----------------------------------

    fn sop_eph_component() -> ComponentContext {
        component_context(
            rect(0, 0, 16, 16),
            coding_style(0, 4, 4, &[], 0),
            quant_none(2, vec![8]),
            None,
        )
    }

    #[test]
    fn sop_and_eph_markers_wrap_the_packet() {
        // SOP marker segment (A.8.1): 0xFF91, Lsop = 4, Nsop = 0; then the
        // packet header:
        //   1 non-zero; 1 included (value 0); 1 zero missing planes;
        //   0 one pass; 0 Lblock; 010  2 bytes in 3 bits
        // then the EPH marker (A.8.2, 0xFF92) and the 2-byte body.
        let header = bit_bytes("1 1 1 0 0 010");
        assert_eq!(header, vec![226]);
        let mut stream = vec![255, 145, 0, 4, 0, 0];
        stream.extend_from_slice(&header);
        stream.extend_from_slice(&[255, 146]);
        stream.extend_from_slice(&[7, 7]);
        let ctx = TileDecodeContext {
            components: vec![sop_eph_component()],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: true,
            eph_markers: true,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings, Vec::<String>::new());
        assert_eq!(outcome.body_end, 11);
        assert_eq!(outcome.header_end, 11);
        let block = &outcome.packets.components[0].bands[0].blocks[0];
        assert_eq!(segment_tuples(block), vec![(9, 2, 1, false)]);
    }

    #[test]
    fn sop_sequence_mismatch_warns_and_resynchronizes() {
        // Nsop = 5 where 0 is expected: the packet still decodes (the
        // count is a resynchronization aid), with one warning (A.8.1).
        let header = bit_bytes("1 1 1 0 0 010");
        let mut stream = vec![255, 145, 0, 4, 0, 5];
        stream.extend_from_slice(&header);
        stream.extend_from_slice(&[7, 7]);
        let ctx = TileDecodeContext {
            components: vec![sop_eph_component()],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: true,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings.len(), 1);
        assert!(outcome.packets.warnings[0].contains("SOP"));
        let block = &outcome.packets.components[0].bands[0].blocks[0];
        assert_eq!(segment_tuples(block), vec![(7, 2, 1, false)]);
    }

    // ---- leniency doctrine -------------------------------------------------

    #[test]
    fn corruption_in_the_first_packet_is_a_hard_error() {
        let ctx = TileDecodeContext {
            components: vec![sop_eph_component()],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &[],
            packed_headers: None,
        };
        assert!(matches!(
            decode_tile_packets(&ctx, &DecodeLimits::default()),
            Err(JpxError::Malformed(_))
        ));
    }

    #[test]
    fn first_packet_body_overrun_is_a_hard_error() {
        // Header: 1 non-zero; 1 included; 1 zero planes; 0 one pass;
        // 111110 Lblock 3 + 5 = 8; 11001000 claims 200 body bytes — but
        // only 5 exist. The very first packet is corrupt -> Malformed.
        let header = bit_bytes("1 1 1 0 111110 11001000");
        assert_eq!(header, vec![239, 178, 0]);
        let mut stream = header;
        stream.extend_from_slice(&[0u8; 5]);
        let ctx = TileDecodeContext {
            components: vec![sop_eph_component()],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        match decode_tile_packets(&ctx, &DecodeLimits::default()) {
            Err(JpxError::Malformed(message)) => assert!(message.contains("overrun")),
            other => panic!("expected a hard body-overrun error, got {other:?}"),
        }
    }

    #[test]
    fn corruption_after_the_first_packet_degrades_to_a_warning() {
        // The Figure B.13 layer-0 packet parses, then the layer-1 header is
        // truncated after one byte: the tile keeps the layer-0 segments and
        // reports one warning (leniency doctrine).
        let mut stream = bit_bytes("1 111 000111 1100 0 0100 1 01 10 10 00100 0 0 0");
        stream.extend_from_slice(&[101, 102, 103, 104, 111, 112, 113, 114]);
        stream.push(241);
        let ctx = TileDecodeContext {
            components: vec![figure_b13_component()],
            tile_rect: rect(0, 0, 48, 32),
            progression: ProgressionOrder::Lrcp,
            layers: 2,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        assert_eq!(outcome.packets.warnings.len(), 1);
        assert!(outcome.packets.warnings[0].contains("layer 1"));
        let band = &outcome.packets.components[0].bands[0];
        assert_eq!(segment_tuples(&band.blocks[0]), vec![(5, 4, 3, false)]);
        assert_eq!(segment_tuples(&band.blocks[1]), vec![(9, 4, 2, false)]);
        assert_eq!(outcome.body_end, 13);
    }

    #[test]
    fn segment_allocation_is_bounded_by_decode_limits() {
        let header = bit_bytes("1 1 1 0 0 010");
        let mut stream = header;
        stream.extend_from_slice(&[7, 7]);
        let limits = DecodeLimits {
            max_decoded_bytes: std::mem::size_of::<CodeBlockSegment>() as u64 - 1,
            ..DecodeLimits::default()
        };
        let ctx = TileDecodeContext {
            components: vec![sop_eph_component()],
            tile_rect: rect(0, 0, 16, 16),
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &stream,
            packed_headers: None,
        };
        assert!(matches!(
            decode_tile_packets(&ctx, &limits),
            Err(JpxError::LimitExceeded {
                what: "max_decoded_bytes",
                ..
            })
        ));
    }

    // ---- E.1 magnitude bit-plane budgets -----------------------------------

    #[test]
    fn magnitude_bits_follow_e2_and_e5() {
        // NL = 2: band order LL(nb 2), r1 HL/LH/HH (nb 2), r2 HL/LH/HH
        // (nb 1). Scalar derived (Table A.28) resolves every band from the
        // NL-LL pair via (E-5): eps_b = eps_0 - NL + nb.
        // eps_0 = 8, G = 2: LL and r1 get Mb = 2 + 8 - 1 = 9; r2 gets
        // eps = 8 - 2 + 1 = 7 -> Mb = 8 (E-2).
        let style = coding_style(2, 6, 6, &[], 0);
        let quant = Quantization {
            guard_bits: 2,
            style: QuantizationStyle::ScalarDerived {
                exponent: 8,
                mantissa: 0,
            },
        };
        let component = component_context(rect(0, 0, 64, 64), style, quant, None);
        let ctx = TileDecodeContext {
            components: vec![component],
            tile_rect: rect(0, 0, 64, 64),
            progression: ProgressionOrder::Lrcp,
            layers: 0,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &[],
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        let bands = &outcome.packets.components[0].bands;
        assert_eq!(bands.len(), 7);
        let bits: Vec<u8> = bands
            .iter()
            .map(|band| band.blocks[0].magnitude_bits)
            .collect();
        assert_eq!(bits, vec![9, 9, 9, 9, 8, 8, 8]);
        // The band metadata mirrors the geometry (B.9 order).
        let kinds: Vec<BandKind> = bands.iter().map(|band| band.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BandKind::Ll,
                BandKind::Hl,
                BandKind::Lh,
                BandKind::Hh,
                BandKind::Hl,
                BandKind::Lh,
                BandKind::Hh,
            ]
        );
        let levels: Vec<u8> = bands.iter().map(|band| band.level).collect();
        assert_eq!(levels, vec![2, 2, 2, 2, 1, 1, 1]);
    }

    #[test]
    fn magnitude_bits_add_the_rgn_maxshift_and_read_expounded_steps() {
        // Scalar expounded (Table A.28): one (E-3) pair per band in the
        // Table A.29 order; with G = 1, Mb = 1 + eps - 1 = eps (E-2). The
        // RGN maxshift raises every band by Srgn (A.6.3, H.2).
        let exponents = [9u8, 8, 7, 6, 5, 4, 3];
        let style = coding_style(2, 6, 6, &[], 0);
        let quant = Quantization {
            guard_bits: 1,
            style: QuantizationStyle::ScalarExpounded {
                steps: exponents
                    .iter()
                    .map(|&exponent| QuantStep {
                        exponent,
                        mantissa: 0,
                    })
                    .collect(),
            },
        };
        let component = component_context(rect(0, 0, 64, 64), style, quant, Some(4));
        let ctx = TileDecodeContext {
            components: vec![component],
            tile_rect: rect(0, 0, 64, 64),
            progression: ProgressionOrder::Lrcp,
            layers: 0,
            poc: Vec::new(),
            sop_markers: false,
            eph_markers: false,
            bitstream: &[],
            packed_headers: None,
        };
        let outcome = decode_tile_packets(&ctx, &DecodeLimits::default()).unwrap();
        let bits: Vec<u8> = outcome.packets.components[0]
            .bands
            .iter()
            .map(|band| band.blocks[0].magnitude_bits)
            .collect();
        assert_eq!(bits, vec![13, 12, 11, 10, 9, 8, 7]);
    }

    // ---- B.12.1 progression orders ------------------------------------------

    /// Two components sharing one 32 x 32 tile:
    /// c0: NL = 1, precincts (PPx, PPy) = (3,3) then (4,4):
    ///     r0 rect [0,16)^2 -> 2 x 2 precincts, r1 [0,32)^2 -> 2 x 2.
    /// c1: sub-sampled by 2 (component rect [0,16)^2 per B-12), NL = 2,
    ///     precincts (3,3), (3,3), (4,4): every level a single precinct.
    fn progression_components() -> Vec<ComponentContext> {
        vec![
            component_context(
                rect(0, 0, 32, 32),
                coding_style(1, 3, 3, &[(3, 3), (4, 4)], 0),
                quant_none(2, vec![8]),
                None,
            ),
            subsampled_component_context(
                rect(0, 0, 32, 32),
                2,
                2,
                coding_style(2, 3, 3, &[(3, 3), (3, 3), (4, 4)], 0),
                quant_none(2, vec![8]),
                None,
            ),
        ]
    }

    #[test]
    fn lrcp_and_rlcp_orders_follow_the_b_12_1_loops() {
        let components = progression_components();
        // Sanity: the precinct grids the hand-enumeration assumes.
        assert_eq!(components[0].geometry.resolutions[0].precincts_wide, 2);
        assert_eq!(components[0].geometry.resolutions[1].precincts_wide, 2);
        assert_eq!(components[1].geometry.resolutions[0].precincts_wide, 1);

        // B.12.1.1: for l { for r { for c { for k } } }; Nmax = 2 so r = 2
        // only carries c1 packets.
        let lrcp = collect_sequence(&components, rect(0, 0, 32, 32), ProgressionOrder::Lrcp, 2, Vec::new());
        let expected_layer0 = [
            (0, 0, 0, 0),
            (0, 0, 0, 1),
            (0, 0, 0, 2),
            (0, 0, 0, 3),
            (0, 1, 0, 0),
            (0, 0, 1, 0),
            (0, 0, 1, 1),
            (0, 0, 1, 2),
            (0, 0, 1, 3),
            (0, 1, 1, 0),
            (0, 1, 2, 0),
        ];
        assert_eq!(&lrcp[..11], &expected_layer0);
        let expected_layer1: Vec<(u32, usize, usize, usize)> = expected_layer0
            .iter()
            .map(|&(_, c, r, k)| (1, c, r, k))
            .collect();
        assert_eq!(&lrcp[11..], &expected_layer1[..]);

        // B.12.1.2: for r { for l { for c { for k } } }.
        let rlcp = collect_sequence(&components, rect(0, 0, 32, 32), ProgressionOrder::Rlcp, 2, Vec::new());
        let expected = [
            (0, 0, 0, 0),
            (0, 0, 0, 1),
            (0, 0, 0, 2),
            (0, 0, 0, 3),
            (0, 1, 0, 0),
            (1, 0, 0, 0),
            (1, 0, 0, 1),
            (1, 0, 0, 2),
            (1, 0, 0, 3),
            (1, 1, 0, 0),
            (0, 0, 1, 0),
            (0, 0, 1, 1),
            (0, 0, 1, 2),
            (0, 0, 1, 3),
            (0, 1, 1, 0),
            (1, 0, 1, 0),
            (1, 0, 1, 1),
            (1, 0, 1, 2),
            (1, 0, 1, 3),
            (1, 1, 1, 0),
            (0, 1, 2, 0),
            (1, 1, 2, 0),
        ];
        assert_eq!(rlcp, expected);
    }

    #[test]
    fn positional_orders_follow_b_12_1_3_to_b_12_1_5() {
        // Hand-derived reference-grid positions: every precinct fires
        // where its partition line meets the tile (B.12.1.3
        // divisibility): c0 r0 rows/columns at 0 and 8 << 1 = 16; c0 r1
        // at 0 and 16; c1 (sub-sampled by 2, all levels aligned at 0)
        // fires only at (0, 0).
        let components = progression_components();

        // B.12.1.3 RPCL: for r { for y { for x { for c } } }, layers last.
        let rpcl = collect_sequence(&components, rect(0, 0, 32, 32), ProgressionOrder::Rpcl, 2, Vec::new());
        let expected_rpcl = [
            // r = 0: (0,0) c0 k0 then c1 k0; (0,16) k1; (16,0) k2; (16,16) k3.
            (0, 0, 0, 0),
            (1, 0, 0, 0),
            (0, 1, 0, 0),
            (1, 1, 0, 0),
            (0, 0, 0, 1),
            (1, 0, 0, 1),
            (0, 0, 0, 2),
            (1, 0, 0, 2),
            (0, 0, 0, 3),
            (1, 0, 0, 3),
            // r = 1: same spatial walk.
            (0, 0, 1, 0),
            (1, 0, 1, 0),
            (0, 1, 1, 0),
            (1, 1, 1, 0),
            (0, 0, 1, 1),
            (1, 0, 1, 1),
            (0, 0, 1, 2),
            (1, 0, 1, 2),
            (0, 0, 1, 3),
            (1, 0, 1, 3),
            // r = 2: only c1 has a third resolution level.
            (0, 1, 2, 0),
            (1, 1, 2, 0),
        ];
        assert_eq!(rpcl, expected_rpcl);

        // B.12.1.4 PCRL: for y { for x { for c { for r } } }.
        let pcrl = collect_sequence(&components, rect(0, 0, 32, 32), ProgressionOrder::Pcrl, 2, Vec::new());
        let expected_pcrl = [
            (0, 0, 0, 0),
            (1, 0, 0, 0),
            (0, 0, 1, 0),
            (1, 0, 1, 0),
            (0, 1, 0, 0),
            (1, 1, 0, 0),
            (0, 1, 1, 0),
            (1, 1, 1, 0),
            (0, 1, 2, 0),
            (1, 1, 2, 0),
            (0, 0, 0, 1),
            (1, 0, 0, 1),
            (0, 0, 1, 1),
            (1, 0, 1, 1),
            (0, 0, 0, 2),
            (1, 0, 0, 2),
            (0, 0, 1, 2),
            (1, 0, 1, 2),
            (0, 0, 0, 3),
            (1, 0, 0, 3),
            (0, 0, 1, 3),
            (1, 0, 1, 3),
        ];
        assert_eq!(pcrl, expected_pcrl);

        // B.12.1.5 CPRL: for c { for y { for x { for r } } } — all of c0's
        // spatial walk first, then c1's three resolutions at (0, 0).
        let cprl = collect_sequence(&components, rect(0, 0, 32, 32), ProgressionOrder::Cprl, 2, Vec::new());
        let expected_cprl = [
            (0, 0, 0, 0),
            (1, 0, 0, 0),
            (0, 0, 1, 0),
            (1, 0, 1, 0),
            (0, 0, 0, 1),
            (1, 0, 0, 1),
            (0, 0, 1, 1),
            (1, 0, 1, 1),
            (0, 0, 0, 2),
            (1, 0, 0, 2),
            (0, 0, 1, 2),
            (1, 0, 1, 2),
            (0, 0, 0, 3),
            (1, 0, 0, 3),
            (0, 0, 1, 3),
            (1, 0, 1, 3),
            (0, 1, 0, 0),
            (1, 1, 0, 0),
            (0, 1, 1, 0),
            (1, 1, 1, 0),
            (0, 1, 2, 0),
            (1, 1, 2, 0),
        ];
        assert_eq!(cprl, expected_cprl);
    }

    #[test]
    fn unaligned_tile_origins_fire_their_first_precinct_at_the_tile_edge() {
        // B.12.1.4's second condition arm: at y = ty0, a precinct whose
        // partition line lies above the tile still fires, because try0 is
        // not divisible by 2^PPy (and likewise for x). Tile [5,21)^2:
        // c0: XRsiz = 1, NL = 0, (PPx, PPy) = (3,3): component rect
        //     [5,21)^2 -> 3 x 3 precincts firing at {5, 8, 16} per axis
        //     (partition lines 0 -> tile edge 5, 8, 16).
        // c1: sub-sampled by 2, component rect [3,11)^2 (B-12), NL = 0,
        //     (3,3) -> 2 x 2 precincts; partition line 0 is unaligned
        //     (trx0 = 3) and fires at the tile edge 5, line 8 maps to
        //     reference-grid 8 * 2 = 16.
        let components = vec![
            component_context(
                rect(5, 5, 21, 21),
                coding_style(0, 2, 2, &[(3, 3)], 0),
                quant_none(2, vec![8]),
                None,
            ),
            subsampled_component_context(
                rect(5, 5, 21, 21),
                2,
                2,
                coding_style(0, 2, 2, &[(3, 3)], 0),
                quant_none(2, vec![8]),
                None,
            ),
        ];
        assert_eq!(components[0].geometry.resolutions[0].precincts_wide, 3);
        assert_eq!(components[1].geometry.rect, rect(3, 3, 11, 11));
        assert_eq!(components[1].geometry.resolutions[0].precincts_wide, 2);
        // PCRL interleave by (y, x, c): rows fire at y = 5 (both), 8 (c0
        // only) and 16 (both) — the sub-sampled component's second
        // precinct row waits for reference-grid row 16, NOT its
        // component-grid row 8 (the pre-seam approximation).
        let pcrl = collect_sequence(
            &components,
            rect(5, 5, 21, 21),
            ProgressionOrder::Pcrl,
            1,
            Vec::new(),
        );
        let expected = [
            (0, 0, 0, 0),
            (0, 1, 0, 0),
            (0, 0, 0, 1),
            (0, 0, 0, 2),
            (0, 1, 0, 1),
            (0, 0, 0, 3),
            (0, 0, 0, 4),
            (0, 0, 0, 5),
            (0, 0, 0, 6),
            (0, 1, 0, 2),
            (0, 0, 0, 7),
            (0, 0, 0, 8),
            (0, 1, 0, 3),
        ];
        assert_eq!(pcrl, expected);
    }

    #[test]
    fn poc_volumes_chain_without_repeating_packets() {
        // A.6.6/B.12.2: volume 1 sends r0 layers {0, 1} in LRCP; volume 2
        // spans r0..2 layers 0..3 in RLCP but must skip the packets already
        // sent — each precinct "starts with the next layer".
        let component = component_context(
            rect(0, 0, 16, 16),
            coding_style(1, 3, 3, &[], 0),
            quant_none(2, vec![8]),
            None,
        );
        let poc = vec![
            PocSegment {
                res_start: 0,
                comp_start: 0,
                layer_end: 2,
                res_end: 1,
                comp_end: 1,
                order: ProgressionOrder::Lrcp,
            },
            PocSegment {
                res_start: 0,
                comp_start: 0,
                layer_end: 3,
                res_end: 2,
                comp_end: 1,
                order: ProgressionOrder::Rlcp,
            },
        ];
        let sequence = collect_sequence(
            &[component],
            rect(0, 0, 16, 16),
            ProgressionOrder::Lrcp,
            3,
            poc,
        );
        let expected = [
            (0, 0, 0, 0),
            (1, 0, 0, 0),
            (2, 0, 0, 0),
            (0, 0, 1, 0),
            (1, 0, 1, 0),
            (2, 0, 1, 0),
        ];
        assert_eq!(sequence, expected);
    }

    // ---- zoo fixtures end-to-end --------------------------------------------

    fn zoo_stream(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read(path).unwrap()
    }

    /// Mirrors the `decode()` plumbing up to the Tier-2 seam for every tile
    /// of a fixture and returns `(packets, header_end, body_end, len)`.
    fn parse_zoo_tiles(name: &str) -> Vec<(TilePackets, usize, usize, usize)> {
        let data = zoo_stream(name);
        let limits = DecodeLimits::default();
        let container = crate::boxes::scan(&data).unwrap();
        let cs = crate::markers::parse_codestream(container.codestream, &limits).unwrap();
        assert!(
            cs.main.ppm.is_empty(),
            "{name}: fixtures use in-stream headers"
        );
        let siz = &cs.main.siz;
        let (tiles_wide, tiles_high) = crate::geometry::tile_grid(siz).unwrap();
        let tile_total = tiles_wide as usize * tiles_high as usize;
        let mut tiles: Vec<Vec<&crate::markers::TilePart<'_>>> =
            (0..tile_total).map(|_| Vec::new()).collect();
        for part in &cs.tile_parts {
            tiles[usize::from(part.sot.tile_index)].push(part);
        }
        let mut outcomes = Vec::new();
        for (tile_index, parts) in tiles.iter().enumerate() {
            assert!(!parts.is_empty(), "{name}: tile {tile_index} has no parts");
            let overrides = crate::markers::merge_tile_overrides(parts).unwrap();
            assert!(overrides.ppt.is_empty());
            let tile_coding = crate::markers::resolve_tile_coding(&cs.main, &overrides).unwrap();
            let p = (tile_index % tiles_wide as usize) as u32;
            let q = (tile_index / tiles_wide as usize) as u32;
            let tile_rect = crate::geometry::tile_rect(siz, p, q);
            let mut components = Vec::new();
            for (index, component) in siz.components.iter().enumerate() {
                let coding =
                    crate::markers::resolve_component_coding(&cs.main, &overrides, index as u16)
                        .unwrap();
                let geometry =
                    crate::geometry::tile_component_geometry(tile_rect, component, &coding.style)
                        .unwrap();
                components.push(ComponentContext {
                    geometry,
                    coding,
                    xrsiz: component.xrsiz,
                    yrsiz: component.yrsiz,
                });
            }
            let bitstream: Vec<u8> = parts
                .iter()
                .flat_map(|part| part.body.iter().copied())
                .collect();
            let ctx = TileDecodeContext {
                components,
                tile_rect,
                progression: tile_coding.progression,
                layers: tile_coding.layers,
                poc: tile_coding.poc.clone(),
                sop_markers: tile_coding.sop_markers,
                eph_markers: tile_coding.eph_markers,
                bitstream: &bitstream,
                packed_headers: None,
            };
            let outcome = decode_tile_packets(&ctx, &limits).unwrap();
            outcomes.push((
                outcome.packets,
                outcome.header_end,
                outcome.body_end,
                bitstream.len(),
            ));
        }
        outcomes
    }

    /// Every codeword segment must address bytes inside the tile body;
    /// returns the segment count.
    fn assert_segments_within(name: &str, packets: &TilePackets, len: usize) -> u64 {
        let mut count = 0u64;
        for component in &packets.components {
            for band in &component.bands {
                for block in &band.blocks {
                    for segment in &block.segments {
                        assert!(
                            segment.start + segment.len <= len,
                            "{name}: segment {segment:?} beyond the {len}-byte body"
                        );
                        count += 1;
                    }
                }
            }
        }
        count
    }

    #[test]
    fn zoo_progression_fixtures_parse_to_the_exact_stream_end() {
        // All five B.12.1 orders over the same source image: a progression
        // iterator error desynchronizes the tag trees and cannot land on
        // the exact stream end.
        for name in [
            "rgb-prog-lrcp.jp2",
            "rgb-prog-rlcp.jp2",
            "rgb-prog-rpcl.jp2",
            "rgb-prog-pcrl.jp2",
            "rgb-prog-cprl.jp2",
        ] {
            for (packets, header_end, body_end, len) in parse_zoo_tiles(name) {
                assert_eq!(packets.warnings, Vec::<String>::new(), "{name}");
                assert_eq!(body_end, len, "{name}");
                assert_eq!(header_end, len, "{name}");
                assert!(assert_segments_within(name, &packets, len) > 0, "{name}");
            }
        }
    }

    #[test]
    fn zoo_layers_fixture_accumulates_segments_across_three_layers() {
        let outcomes = parse_zoo_tiles("rgb-layers.jp2");
        for (packets, header_end, body_end, len) in &outcomes {
            assert_eq!(packets.warnings, Vec::<String>::new());
            assert_eq!(*body_end, *len);
            assert_eq!(*header_end, *len);
        }
        // Three quality layers: at least one code-block must have picked
        // up contributions from more than one packet, and none from more
        // than three (no bypass/per-pass termination in this fixture).
        let mut max_segments = 0usize;
        for (packets, ..) in &outcomes {
            for component in &packets.components {
                for band in &component.bands {
                    for block in &band.blocks {
                        assert!(block.segments.len() <= 3);
                        max_segments = max_segments.max(block.segments.len());
                    }
                }
            }
        }
        assert!(max_segments >= 2, "layers never split: {max_segments}");
    }

    #[test]
    fn zoo_structured_fixtures_parse_every_tile() {
        // Custom precincts (with per-band empty precincts at the right
        // edge), a 5 x 3 tile grid, a reduced resolution count and 16 x 16
        // code-blocks.
        for name in [
            "rgb-precinct.jp2",
            "rgb-tiled.jp2",
            "rgb-res3.jp2",
            "rgb-cb16.jp2",
        ] {
            for (packets, header_end, body_end, len) in parse_zoo_tiles(name) {
                assert_eq!(packets.warnings, Vec::<String>::new(), "{name}");
                assert_eq!(body_end, len, "{name}");
                assert_eq!(header_end, len, "{name}");
                assert_segments_within(name, &packets, len);
            }
        }
    }
}
