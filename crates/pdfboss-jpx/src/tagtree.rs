//! Packet-header bit reading (ITU-T T.800 B.10.1) and tag-tree decoding
//! (B.10.2) — shared by the Tier-2 packet reader.

use crate::error::{JpxError, Result};

/// MSB-first bit reader over packet-header bytes implementing the B.10.1
/// bit-stuffing rule: a byte following an emitted 0xFF carries only seven
/// payload bits (its MSB is a stuffed zero). Running out of bytes is a
/// `Malformed` error — packet headers never legitimately end early.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next unread byte.
    pos: usize,
    /// Bits still unread in `current` (0..=8).
    remaining: u8,
    /// The byte bits are currently drawn from, MSB first.
    current: u8,
    /// The previously completed byte was 0xFF, so the next byte loaded is
    /// stuffed (B.10.1).
    last_was_ff: bool,
}

impl<'a> BitReader<'a> {
    /// Starts reading at the first byte of `data`.
    pub(crate) fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            remaining: 0,
            current: 0,
            last_was_ff: false,
        }
    }

    /// Reads a single bit (0 or 1).
    pub(crate) fn read_bit(&mut self) -> Result<u32> {
        if self.remaining == 0 {
            let Some(&byte) = self.data.get(self.pos) else {
                return Err(JpxError::Malformed("packet header truncated".into()));
            };
            self.pos += 1;
            if self.last_was_ff {
                // B.10.1: the byte following 0xFF carries seven payload
                // bits; its MSB is the stuffed zero.
                self.current = byte << 1;
                self.remaining = 7;
            } else {
                self.current = byte;
                self.remaining = 8;
            }
            self.last_was_ff = byte == 255;
        }
        let bit = u32::from(self.current >> 7);
        self.current <<= 1;
        self.remaining -= 1;
        Ok(bit)
    }

    /// Reads `count` bits (0..=32) MSB first, as the packet header's fixed-
    /// width fields require (e.g. the B-19 length fields).
    pub(crate) fn read_bits(&mut self, count: u32) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    /// Ends the packet header (B.10.1): discards the unread bits of the
    /// current byte and, when the last fully consumed byte was 0xFF, also
    /// consumes the following stuffed byte — the stuffed zero bit is part
    /// of the header even when 0xFF would otherwise have been its last
    /// byte.
    pub(crate) fn align(&mut self) -> Result<()> {
        self.remaining = 0;
        self.current = 0;
        if self.last_was_ff {
            // The stuffed byte after a trailing 0xFF belongs to this
            // header even when nothing of it is read (B.10.1).
            if self.pos < self.data.len() {
                self.pos += 1;
            }
            self.last_was_ff = false;
        }
        Ok(())
    }

    /// Index of the next unread byte — after [`Self::align`] this is where
    /// the packet body begins.
    pub(crate) fn byte_position(&self) -> usize {
        self.pos
    }
}

/// Decoding state of one tag-tree node (B.10.2): the "current value" the
/// node has been raised to so far, and whether a 1 bit has fixed it there.
#[derive(Clone)]
struct NodeState {
    /// The node's value is known to be at least this ("current value").
    low: u32,
    /// A 1 bit fixed the value at exactly `low`.
    known: bool,
}

/// Tag-tree decoder (B.10.2): a 2-D array of non-negative integers coded
/// through a quad-tree of running minima. Shared by code-block inclusion
/// (B.10.4) and zero-bit-plane signalling (B.10.5); state persists across
/// packets of the same precinct (the causality rule of B.10.2).
// Internal node state is the tag-tree stage's to design; only `new` and
// `decode` are the frozen seam.
pub(crate) struct TagTree {
    /// Leaf columns (code-blocks spanned horizontally).
    width: u32,
    /// Leaf rows.
    height: u32,
    /// Every node's state, level 0 (the single root) first, each level
    /// row-major. Empty for a zero-sized leaf grid.
    nodes: Vec<NodeState>,
    /// Per level, root first: index of the level's first node in `nodes`
    /// and the level's width in nodes. Level `levels.len() - 1` is the
    /// leaf grid; each higher level halves both dimensions (rounding up)
    /// until the 1x1 root (B.10.2, Figure B.12).
    levels: Vec<(usize, u32)>,
}

impl TagTree {
    /// A tree over a `width x height` leaf grid, every node's current value
    /// initialized to zero (B.10.2). A zero-sized grid is legal (empty
    /// precinct ∩ band): `decode` is then never called for it.
    pub(crate) fn new(width: u32, height: u32) -> Self {
        // Leaf level upwards: halve (rounding up) until the 1x1 root, then
        // flip so the root is level 0 as in B.10.2.
        let mut dims = Vec::new();
        if width > 0 && height > 0 {
            let (mut w, mut h) = (width, height);
            dims.push((w, h));
            while w > 1 || h > 1 {
                w = w.div_ceil(2);
                h = h.div_ceil(2);
                dims.push((w, h));
            }
            dims.reverse();
        }
        let mut levels = Vec::with_capacity(dims.len());
        let mut total = 0usize;
        for &(w, h) in &dims {
            levels.push((total, w));
            total += w as usize * h as usize;
        }
        let nodes = vec![
            NodeState {
                low: 0,
                known: false,
            };
            total
        ];
        TagTree {
            width,
            height,
            nodes,
            levels,
        }
    }

    /// Advances the knowledge about leaf `(x, y)` until either its value is
    /// fully determined and `< threshold` (returns `Ok(true)`) or it is
    /// known to be `>= threshold` (returns `Ok(false)`), consuming exactly
    /// the bits B.10.2 prescribes (querying the path from the level-0 root
    /// downwards; information already fixed by earlier calls is not read
    /// again).
    ///
    /// Callers: inclusion asks `decode(.., layer + 1)` (B.10.4); the
    /// zero-bit-plane count raises `threshold` until the call returns true,
    /// the value then being `threshold - 1` (B.10.5).
    pub(crate) fn decode(
        &mut self,
        reader: &mut BitReader<'_>,
        x: u32,
        y: u32,
        threshold: u32,
    ) -> Result<bool> {
        if x >= self.width || y >= self.height {
            return Err(JpxError::Malformed(format!(
                "tag tree leaf ({x}, {y}) outside the {}x{} grid",
                self.width, self.height
            )));
        }
        let TagTree { levels, nodes, .. } = self;
        let leaf_level = levels.len() - 1;
        // Every node records the minimum of its children (B.10.2), so once
        // an ancestor is fixed its value floors every descendant's current
        // value — the NOTE's q2(1, 0) needs a single 1 bit exactly because
        // it starts at its parent's value, not at zero.
        let mut floor = 0u32;
        for (level, &(offset, level_width)) in levels.iter().enumerate() {
            let shift = leaf_level - level;
            let nx = (u64::from(x) >> shift) as usize;
            let ny = (u64::from(y) >> shift) as usize;
            let node = &mut nodes[offset + ny * level_width as usize + nx];
            if node.low < floor {
                node.low = floor;
            }
            // B.10.2: a 0 bit means the value exceeds the current value
            // (raise it by one); a 1 bit means it equals it (fix it).
            // Decoding halts as soon as the query is answered, and bits
            // implied by earlier queries are never in the stream.
            while !node.known && node.low < threshold {
                if reader.read_bit()? == 1 {
                    node.known = true;
                } else {
                    node.low += 1;
                }
            }
            if node.low >= threshold {
                // Fixed at, or bounded below by, `threshold` or more: the
                // leaf cannot be smaller (minima nest), so the answer is
                // known without descending further.
                return Ok(false);
            }
            floor = node.low;
        }
        // The leaf itself was fixed at a value below the threshold.
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_reads_msb_first() {
        // 0b1010_0000 = 160: the first three bits are 1, 0, 1.
        let mut reader = BitReader::new(&[160]);
        assert_eq!(reader.read_bit().unwrap(), 1);
        assert_eq!(reader.read_bit().unwrap(), 0);
        assert_eq!(reader.read_bit().unwrap(), 1);
        // read_bits packs MSB first: remaining bits 0_0000 == 0.
        assert_eq!(reader.read_bits(5).unwrap(), 0);
    }

    #[test]
    fn bit_reader_unstuffs_after_ff() {
        // B.10.1: after an 0xFF byte the next byte carries 7 bits, its MSB
        // being a stuffed zero. Bytes: 255, then 0b0110_0000 = 96. Reading
        // 8 bits yields 255; the following SEVEN bits yield 110_0000 = 96.
        let mut reader = BitReader::new(&[255, 96]);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        assert_eq!(reader.read_bits(7).unwrap(), 96);
        // Both bytes fully consumed.
        assert_eq!(reader.byte_position(), 2);
    }

    #[test]
    fn align_discards_partial_bytes() {
        // Read 1 bit of 0b1000_0000 = 128, then align: the next read must
        // start at byte 1 (value 77).
        let mut reader = BitReader::new(&[128, 77]);
        assert_eq!(reader.read_bit().unwrap(), 1);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 1);
        assert_eq!(reader.read_bits(8).unwrap(), 77);
    }

    #[test]
    fn align_consumes_the_stuffed_byte_after_a_trailing_ff() {
        // B.10.1: "the single zero bit stuffed after a byte with 0xFF must
        // be included even if the 0xFF would otherwise have been the last
        // byte" — so a header ending in 0xFF is followed by one stuffed
        // byte that align() must swallow. Bytes: 255, 127, 77.
        let mut reader = BitReader::new(&[255, 127, 77]);
        assert_eq!(reader.read_bits(8).unwrap(), 255);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 2);
        assert_eq!(reader.read_bits(8).unwrap(), 77);
    }

    #[test]
    fn align_is_a_no_op_on_a_byte_boundary() {
        let mut reader = BitReader::new(&[10, 20]);
        assert_eq!(reader.read_bits(8).unwrap(), 10);
        reader.align().unwrap();
        assert_eq!(reader.byte_position(), 1);
        assert_eq!(reader.read_bits(8).unwrap(), 20);
    }

    #[test]
    fn bit_reader_errors_on_exhaustion() {
        let mut reader = BitReader::new(&[7]);
        assert_eq!(reader.read_bits(8).unwrap(), 7);
        assert!(
            matches!(reader.read_bit(), Err(JpxError::Malformed(msg)) if msg.contains("header"))
        );
    }

    /// Reads leaf `(x, y)`'s value the way zero-bit-plane counts are read
    /// (B.10.5): raise the threshold until `decode` answers true; the value
    /// is then `threshold - 1`.
    fn decode_value(tree: &mut TagTree, reader: &mut BitReader<'_>, x: u32, y: u32) -> u32 {
        for threshold in 1..64 {
            if tree.decode(reader, x, y, threshold).unwrap() {
                return threshold - 1;
            }
        }
        panic!("tag-tree value did not resolve below 63");
    }

    #[test]
    fn tag_tree_reproduces_the_worked_note_of_b_10_2() {
        // The NOTE to B.10.2 walks the first three leaves of Figure B.12a
        // (a 6x3 array) and gives their exact codewords:
        //   q3(0,0) = 1 codes 01111
        //   q3(1,0) = 3 codes 001
        //   q3(2,0) = 2 codes 101
        // Concatenated that is 01111 001 101 = 11 bits. The sentinel 10101
        // is appended so the test proves EXACTLY 11 bits were consumed:
        //   0111 1001 1011 0101 -> bytes 121, 181.
        let data = [121u8, 181];
        let mut reader = BitReader::new(&data);
        let mut tree = TagTree::new(6, 3);
        assert_eq!(decode_value(&mut tree, &mut reader, 0, 0), 1);
        assert_eq!(decode_value(&mut tree, &mut reader, 1, 0), 3);
        assert_eq!(decode_value(&mut tree, &mut reader, 2, 0), 2);
        // 0b10101 = 21: the sentinel is intact right after bit 11.
        assert_eq!(reader.read_bits(5).unwrap(), 21);
    }

    #[test]
    fn tag_tree_decodes_the_full_figure_b_12_array() {
        // Figure B.12: leaves (level 3, 6x3) with ancestor minima
        // level 2 = [1 1 2 / 2 2 1], level 1 = [1 1], level 0 = [1].
        // Hand-coding every leaf in raster order per B.10.2 (a node's
        // running value starts at its parent's fixed value; each 0 bit
        // raises it by one, a 1 bit fixes it; nothing known is re-coded):
        //   (0,0)=1: 01 1 1 1   root 0->1 fixed, q1/q2/leaf each "1"
        //   (1,0)=3: 001        leaf 1->2->3
        //   (2,0)=2: 1 01       q2(1,0) fixed 1, leaf 1->2
        //   (3,0)=3: 001        leaf 1->2->3
        //   (4,0)=2: 1 01 1     q1(1,0) fixed 1, q2(2,0) 1->2, leaf 2
        //   (5,0)=3: 01         leaf 2->3
        //   (0,1)=2: 01         leaf 1->2
        //   (1,1)=2: 01         leaf 1->2
        //   (2,1)=1: 1          leaf fixed at 1
        //   (3,1)=4: 0001       leaf 1->2->3->4
        //   (4,1)=3: 01         leaf 2->3
        //   (5,1)=2: 1          leaf fixed at 2
        //   (0,2)=2: 01 1       q2(0,1) 1->2 fixed, leaf 2
        //   (1,2)=2: 1          leaf fixed at 2
        //   (2,2)=2: 01 1       q2(1,1) 1->2 fixed, leaf 2
        //   (3,2)=2: 1          leaf fixed at 2
        //   (4,2)=1: 1 1        q2(2,1) fixed 1, leaf 1
        //   (5,2)=2: 01         leaf 1->2
        // 44 bits; with the sentinel 1010 appended:
        //   01111001 10100110 11010101 10001011 01110111 11011010
        // = bytes 121, 166, 213, 139, 119, 218.
        let data = [121u8, 166, 213, 139, 119, 218];
        let mut reader = BitReader::new(&data);
        let mut tree = TagTree::new(6, 3);
        let expected = [
            [1u32, 3, 2, 3, 2, 3],
            [2, 2, 1, 4, 3, 2],
            [2, 2, 2, 2, 1, 2],
        ];
        for (y, row) in expected.iter().enumerate() {
            for (x, &value) in row.iter().enumerate() {
                assert_eq!(
                    decode_value(&mut tree, &mut reader, x as u32, y as u32),
                    value,
                    "leaf ({x}, {y})"
                );
            }
        }
        // 0b1010 = 10: the sentinel begins exactly at bit 45.
        assert_eq!(reader.read_bits(4).unwrap(), 10);
        assert_eq!(reader.byte_position(), 6);
    }

    #[test]
    fn tag_tree_state_survives_between_queries() {
        // Inclusion-style use (B.10.4): each layer's packet asks
        // decode(.., layer + 1). A 1x1 tree over the value 2 codes 0 0 1
        // — one bit per query, and per B.10.2 causality nothing is ever
        // re-read. Bits 001 + 00000 padding = byte 0b00100000 = 32.
        let data = [32u8];
        let mut reader = BitReader::new(&data);
        let mut tree = TagTree::new(1, 1);
        assert!(!tree.decode(&mut reader, 0, 0, 1).unwrap()); // reads "0"
        assert!(!tree.decode(&mut reader, 0, 0, 2).unwrap()); // reads "0"
        assert!(tree.decode(&mut reader, 0, 0, 3).unwrap()); // reads "1"

        // The value is fixed at 2 now: both answers come from retained
        // state alone — a stray read would eat the zero padding and flip
        // the threshold-3 answer.
        assert!(tree.decode(&mut reader, 0, 0, 3).unwrap());
        assert!(!tree.decode(&mut reader, 0, 0, 2).unwrap());
        assert_eq!(reader.read_bits(5).unwrap(), 0);
    }

    #[test]
    fn tag_tree_decode_reads_across_a_stuffed_ff_boundary() {
        // A 129x1 grid halves to levels sized 129, 65, 33, 17, 9, 5, 3,
        // 2, 1 — a nine-node path from root to leaf (0, 0). Coding leaf
        // value 0 emits a "1" at every node: nine 1 bits. The first eight
        // assemble the byte 0xFF, so per B.10.1 the following byte stuffs
        // a zero into its MSB; the ninth 1 lands after it and the byte is
        // padded out: 11111111, 0(stuffed)1000000 -> bytes 255, 64.
        let data = [255u8, 64];
        let mut reader = BitReader::new(&data);
        let mut tree = TagTree::new(129, 1);
        assert!(tree.decode(&mut reader, 0, 0, 1).unwrap());
        // Nine payload bits consumed; the six padding zeros remain.
        assert_eq!(reader.read_bits(6).unwrap(), 0);
        assert_eq!(reader.byte_position(), 2);
    }

    #[test]
    fn tag_tree_threshold_zero_reads_nothing() {
        // Tag trees code non-negative integers (B.10.2), so every value
        // is >= 0 and a zero threshold is decided without any bits.
        let mut reader = BitReader::new(&[]);
        let mut tree = TagTree::new(2, 2);
        assert!(!tree.decode(&mut reader, 1, 1, 0).unwrap());
    }

    #[test]
    fn tag_tree_rejects_out_of_range_leaves() {
        let mut reader = BitReader::new(&[255]);
        let mut tree = TagTree::new(2, 2);
        assert!(matches!(
            tree.decode(&mut reader, 2, 0, 1),
            Err(JpxError::Malformed(msg)) if msg.contains("tag tree")
        ));
    }

    #[test]
    fn tag_tree_propagates_reader_exhaustion() {
        let mut reader = BitReader::new(&[]);
        let mut tree = TagTree::new(1, 1);
        assert!(tree.decode(&mut reader, 0, 0, 1).is_err());
    }
}
