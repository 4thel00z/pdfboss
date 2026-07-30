//! Encrypted-fixture support: a one-page document protected by the Standard
//! security handler (RC4, `/V 2 /R 3`, 128-bit) under the **empty** user
//! password, for tests that need a file both document APIs must transparently
//! decrypt.
//!
//! The MD5 (RFC 1321) and RC4 primitives are implemented here rather than
//! borrowed from `pdfboss-core` because this crate deliberately has no
//! dependencies: it must stay a pure fixture builder every other crate can
//! use without cycles. Both are public algorithms; RC4 is symmetric, so the
//! same routine that decrypts in the reader encrypts here.

use crate::PdfBuilder;

/// The padding string of ISO 32000-1 7.6.3.3, Algorithm 2.
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// 128-bit file key.
const KEY_LEN: usize = 16;
/// Permissions word baked into the fixture.
const P: i32 = -44;
/// The first `/ID` element baked into the fixture.
const ID0: &[u8] = b"0123456789abcdef";

/// Per-round left-rotation amounts (RFC 1321).
#[rustfmt::skip]
const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Per-round additive constants `floor(2^32 * abs(sin(i + 1)))` (RFC 1321).
#[rustfmt::skip]
const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// One-shot MD5 (RFC 1321), sufficient for key derivation.
fn md5(input: &[u8]) -> [u8; 16] {
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    let mut msg = input.to_vec();
    let bitlen = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_le_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (word, bytes) in m.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(MD5_K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(MD5_S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// RC4 stream cipher (symmetric: the same call encrypts and decrypts).
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[s[i as usize].wrapping_add(s[j as usize]) as usize];
        out.push(byte ^ k);
    }
    out
}

/// `/O` for empty owner and user passwords (ISO 32000-1 Algorithm 3, R3).
fn owner_entry() -> Vec<u8> {
    let mut d = md5(&PAD);
    for _ in 0..50 {
        d = md5(&d[..KEY_LEN]);
    }
    let rc4key = d[..KEY_LEN].to_vec();
    let mut o = rc4(&rc4key, &PAD);
    for i in 1u8..=19 {
        let k: Vec<u8> = rc4key.iter().map(|b| b ^ i).collect();
        o = rc4(&k, &o);
    }
    o
}

/// File key from `/O` for the empty user password (Algorithm 2, R3).
fn file_key(o: &[u8]) -> Vec<u8> {
    let mut input = Vec::new();
    input.extend_from_slice(&PAD);
    input.extend_from_slice(o);
    input.extend_from_slice(&(P as u32).to_le_bytes());
    input.extend_from_slice(ID0);
    let mut d = md5(&input);
    for _ in 0..50 {
        d = md5(&d[..KEY_LEN]);
    }
    d[..KEY_LEN].to_vec()
}

/// `/U` for the empty user password (Algorithm 5, R3).
fn user_entry(key: &[u8]) -> Vec<u8> {
    let mut input = Vec::new();
    input.extend_from_slice(&PAD);
    input.extend_from_slice(ID0);
    let mut x = md5(&input).to_vec();
    x = rc4(key, &x);
    for i in 1u8..=19 {
        let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        x = rc4(&k, &x);
    }
    x.resize(32, 0); // trailing padding is arbitrary
    x
}

/// Per-object key (Algorithm 1).
fn obj_key(key: &[u8], num: u32, gen: u16) -> Vec<u8> {
    let mut input = key.to_vec();
    input.extend_from_slice(&num.to_le_bytes()[..3]);
    input.extend_from_slice(&gen.to_le_bytes()[..2]);
    md5(&input)[..(key.len() + 5).min(16)].to_vec()
}

fn hexstr(b: &[u8]) -> String {
    let mut s = String::from("<");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s.push('>');
    s
}

/// One-call fixture: a single page showing `text` in 12pt Helvetica, with
/// the content stream and an extra `/Msg` string (object 6, value `text`)
/// RC4-encrypted under the Standard handler and the empty user password.
/// A reader that decrypts correctly extracts `text`; one that does not gets
/// keystream garbage.
pub fn encrypted_rc4_doc(text: &str) -> Vec<u8> {
    let o = owner_entry();
    let key = file_key(&o);
    let u = user_entry(&key);

    let content = crate::show_text_content(text);
    let enc_content = rc4(&obj_key(&key, 5, 0), content.as_bytes());
    let enc_msg = rc4(&obj_key(&key, 6, 0), text.as_bytes());

    let mut b = PdfBuilder::new();
    b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.object(2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>");
    b.object(
        3,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    b.object(
        4,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>",
    );
    b.stream(5, "", &enc_content);
    b.object(6, &format!("<< /Msg {} >>", hexstr(&enc_msg)));
    b.object(
        9,
        &format!(
            "<< /Filter /Standard /V 2 /R 3 /Length 128 /P {P} /O {} /U {} >>",
            hexstr(&o),
            hexstr(&u)
        ),
    );
    let trailer = format!("/Encrypt 9 0 R /ID [{}{}]", hexstr(ID0), hexstr(ID0));
    b.trailer_extra(&trailer).build(1)
}
