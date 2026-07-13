//! Decryption for the Standard security handler (ISO 32000 §7.6) opened with
//! the empty user password.
//!
//! Handles RC4 (`/V` 1–2, `/R` 2–3, 40–128-bit), AESV2 (`/V` 4, 128-bit
//! AES-CBC) and AESV3 (`/V` 5, `/R` 5–6, 256-bit AES-CBC). Documents that need
//! a real password are reported as encrypted-and-unsupported by the caller. The
//! primitives — MD5, RC4, AES and the SHA-2 family — are implemented here from
//! their published specifications so the crate needs no cryptographic
//! dependency.

use crate::object::{Dict, Object};

/// Password padding string (ISO 32000 §7.6.3.3, Algorithm 2, step (a)).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Which cipher a configured [`Decryptor`] applies to strings and streams.
#[derive(Clone, Copy, PartialEq)]
enum Cipher {
    /// RC4 stream cipher, per-object key (V1/V2, and V4 with `/CFM /V2`).
    Rc4,
    /// AES-128-CBC, per-object key with the `sAlT` suffix (V4, `/CFM /AESV2`).
    Aesv2,
    /// AES-256-CBC, the file key applied directly (V5, `/CFM /AESV3`).
    Aesv3,
}

/// A configured Standard-handler decryptor for a document opened with the empty
/// user password.
pub(crate) struct Decryptor {
    /// The file key (`n` bytes for RC4/AESV2, 32 for AESV3).
    key: Vec<u8>,
    cipher: Cipher,
}

impl Decryptor {
    /// Builds a decryptor from the resolved `/Encrypt` dictionary and the first
    /// `/ID` element, assuming the empty user password. Returns `None` when the
    /// handler or its parameters are unsupported, or the empty password does
    /// not open the file (so the caller can report it as unsupported).
    pub(crate) fn from_standard(enc: &Dict, id0: &[u8]) -> Option<Decryptor> {
        if enc.get_name("Filter").map(|n| n.0.as_str()) != Some("Standard") {
            return None;
        }
        let v = enc.get_int("V").unwrap_or(0);
        let r = enc.get_int("R").unwrap_or(0);
        match (v, r) {
            // RC4: V1 (40-bit) and V2 (up to 128-bit).
            (1 | 2, 2 | 3) => {
                let n = if v == 1 {
                    5
                } else {
                    (enc.get_int("Length").unwrap_or(40) / 8).clamp(5, 16) as usize
                };
                let key = md5_file_key(enc, id0, r, n)?;
                let u = enc.get("U").and_then(Object::as_str_bytes)?;
                verify_user_password(&key, r, id0, u).then_some(Decryptor {
                    key,
                    cipher: Cipher::Rc4,
                })
            }
            // V4: 128-bit key, cipher chosen by the standard crypt filter.
            (4, 4) => {
                let key = md5_file_key(enc, id0, r, 16)?;
                let u = enc.get("U").and_then(Object::as_str_bytes)?;
                if !verify_user_password(&key, r, id0, u) {
                    return None;
                }
                let cipher = match crypt_filter_method(enc)?.as_str() {
                    "AESV2" => Cipher::Aesv2,
                    "V2" => Cipher::Rc4,
                    _ => return None, // Identity or unknown
                };
                Some(Decryptor { key, cipher })
            }
            // V5: AES-256 with SHA-2-based key derivation.
            (5, 5 | 6) => aesv3_key(enc, r).map(|key| Decryptor {
                key,
                cipher: Cipher::Aesv3,
            }),
            _ => None,
        }
    }

    /// Decrypts one indirect object's strings and stream data in place. Objects
    /// extracted from object streams are already plaintext and must not be
    /// passed here.
    pub(crate) fn decrypt_object(&self, obj: &mut Object, num: u32, gen: u16) {
        let key = match self.cipher {
            Cipher::Aesv3 => self.key.clone(), // one file key for every object
            Cipher::Rc4 | Cipher::Aesv2 => self.object_key(num, gen),
        };
        decrypt_in_place(obj, &key, self.cipher);
    }

    /// Per-object key: `MD5(filekey ++ num[0..3] ++ gen[0..2] [++ "sAlT"])`
    /// truncated to `min(n + 5, 16)` bytes (ISO 32000 §7.6.2, Algorithm 1). The
    /// `sAlT` suffix is added for AES crypt filters.
    fn object_key(&self, num: u32, gen: u16) -> Vec<u8> {
        let mut input = Vec::with_capacity(self.key.len() + 9);
        input.extend_from_slice(&self.key);
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        if self.cipher == Cipher::Aesv2 {
            input.extend_from_slice(b"sAlT");
        }
        let digest = md5(&input);
        let n = (self.key.len() + 5).min(16);
        digest[..n].to_vec()
    }
}

/// Recursively decrypts every string and stream body reachable from `obj` with
/// the per-object `key` under `cipher`.
fn decrypt_in_place(obj: &mut Object, key: &[u8], cipher: Cipher) {
    match obj {
        Object::String(bytes) => *bytes = decrypt_bytes(cipher, key, bytes),
        Object::Array(items) => items
            .iter_mut()
            .for_each(|it| decrypt_in_place(it, key, cipher)),
        Object::Dict(dict) => dict
            .values_mut()
            .for_each(|v| decrypt_in_place(v, key, cipher)),
        Object::Stream(stream) => {
            stream
                .dict
                .values_mut()
                .for_each(|v| decrypt_in_place(v, key, cipher));
            stream.data = decrypt_bytes(cipher, key, &stream.data);
        }
        _ => {}
    }
}

/// Applies `cipher` to one string or stream body with the given `key`.
fn decrypt_bytes(cipher: Cipher, key: &[u8], data: &[u8]) -> Vec<u8> {
    match cipher {
        Cipher::Rc4 => rc4(key, data),
        Cipher::Aesv2 | Cipher::Aesv3 => aes_cbc_decrypt(key, data),
    }
}

/// The Standard stream crypt filter's method (`/CF` → `/StmF` → `/CFM`):
/// `V2`, `AESV2`, or `Identity`.
fn crypt_filter_method(enc: &Dict) -> Option<String> {
    let stmf = enc
        .get_name("StmF")
        .map(|n| n.0.as_str())
        .unwrap_or("StdCF");
    let filter = enc.get_dict("CF")?.get_dict(stmf)?;
    Some(filter.get_name("CFM")?.0.clone())
}

/// Algorithm 2: derive the RC4/AESV2 file key from the empty user password.
fn md5_file_key(enc: &Dict, id0: &[u8], r: i64, n: usize) -> Option<Vec<u8>> {
    let o = enc.get("O").and_then(Object::as_str_bytes)?;
    if o.len() < 32 {
        return None;
    }
    let p = enc.get_int("P")?;
    let mut input = Vec::with_capacity(32 + 32 + 4 + id0.len() + 4);
    input.extend_from_slice(&PAD); // padded empty password == the pad itself
    input.extend_from_slice(&o[..32]);
    input.extend_from_slice(&(p as i32 as u32).to_le_bytes()); // /P low 32 bits, LE
    input.extend_from_slice(id0);
    // Revision 4 with /EncryptMetadata false hashes an extra 0xFFFFFFFF.
    if r >= 4 && enc.get("EncryptMetadata").and_then(Object::as_bool) == Some(false) {
        input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let mut digest = md5(&input);
    if r >= 3 {
        for _ in 0..50 {
            digest = md5(&digest[..n]);
        }
    }
    Some(digest[..n].to_vec())
}

/// Checks the empty user password by recomputing `/U` and comparing.
fn verify_user_password(key: &[u8], r: i64, id0: &[u8], u: &[u8]) -> bool {
    if r == 2 {
        // Algorithm 4: U = RC4(key, PAD).
        let computed = rc4(key, &PAD);
        u.len() >= 32 && computed == u[..32]
    } else {
        // Algorithm 5: U = MD5(PAD ++ ID[0]) encrypted with 20 keyed RC4 passes.
        let mut input = Vec::with_capacity(32 + id0.len());
        input.extend_from_slice(&PAD);
        input.extend_from_slice(id0);
        let mut x = md5(&input).to_vec();
        x = rc4(key, &x);
        for i in 1u8..=19 {
            let keyed: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            x = rc4(&keyed, &x);
        }
        // Only the first 16 bytes are defined; the rest of /U is arbitrary padding.
        u.len() >= 16 && x[..16] == u[..16]
    }
}

/// RC4 stream cipher (symmetric: the same call encrypts and decrypts).
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    debug_assert!(!key.is_empty());
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
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

// --- AES (FIPS-197) and CBC mode -----------------------------------------

/// AES substitution box.
#[rustfmt::skip]
const AES_SBOX: [u8; 256] = [
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
    0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
    0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
    0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
    0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
    0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
    0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
    0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
    0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
];

/// Round constants for key expansion (`RCON[j]` used when `i % Nk == 0`).
const AES_RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

/// The inverse S-box, derived once from `AES_SBOX`.
fn aes_inv_sbox() -> [u8; 256] {
    let mut inv = [0u8; 256];
    for (i, &s) in AES_SBOX.iter().enumerate() {
        inv[s as usize] = i as u8;
    }
    inv
}

/// Multiplies two elements of GF(2^8) with the AES reduction polynomial.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// Expands a 16- or 32-byte key into `Nr + 1` round keys (state is stored
/// column-major, so byte `r + 4c` is row `r`, column `c`).
fn aes_expand_key(key: &[u8]) -> Vec<[u8; 16]> {
    let nk = key.len() / 4; // 4 (AES-128) or 8 (AES-256)
    let nr = nk + 6;
    let total = 4 * (nr + 1);
    let mut w: Vec<[u8; 4]> = Vec::with_capacity(total);
    for i in 0..nk {
        w.push([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    for i in nk..total {
        let mut t = w[i - 1];
        if i.is_multiple_of(nk) {
            t = [t[1], t[2], t[3], t[0]]; // RotWord
            for b in &mut t {
                *b = AES_SBOX[*b as usize]; // SubWord
            }
            t[0] ^= AES_RCON[i / nk];
        } else if nk > 6 && i % nk == 4 {
            for b in &mut t {
                *b = AES_SBOX[*b as usize];
            }
        }
        let prev = w[i - nk];
        w.push([
            prev[0] ^ t[0],
            prev[1] ^ t[1],
            prev[2] ^ t[2],
            prev[3] ^ t[3],
        ]);
    }
    (0..=nr)
        .map(|round| {
            let mut rk = [0u8; 16];
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * round + c]);
            }
            rk
        })
        .collect()
}

fn add_round_key(s: &mut [u8; 16], rk: &[u8; 16]) {
    for (b, k) in s.iter_mut().zip(rk) {
        *b ^= k;
    }
}

fn shift_rows(s: &mut [u8; 16]) {
    let o = *s;
    for r in 1..4 {
        for c in 0..4 {
            s[r + 4 * c] = o[r + 4 * ((c + r) % 4)];
        }
    }
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    let o = *s;
    for r in 1..4 {
        for c in 0..4 {
            s[r + 4 * c] = o[r + 4 * ((c + 4 - r) % 4)];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let i = 4 * c;
        let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
        s[i] = gmul(a0, 2) ^ gmul(a1, 3) ^ a2 ^ a3;
        s[i + 1] = a0 ^ gmul(a1, 2) ^ gmul(a2, 3) ^ a3;
        s[i + 2] = a0 ^ a1 ^ gmul(a2, 2) ^ gmul(a3, 3);
        s[i + 3] = gmul(a0, 3) ^ a1 ^ a2 ^ gmul(a3, 2);
    }
}

fn inv_mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let i = 4 * c;
        let (a0, a1, a2, a3) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
        s[i] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
        s[i + 1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
        s[i + 2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
        s[i + 3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
    }
}

fn aes_encrypt_block(s: &mut [u8; 16], rks: &[[u8; 16]]) {
    let nr = rks.len() - 1;
    add_round_key(s, &rks[0]);
    for rk in &rks[1..nr] {
        s.iter_mut().for_each(|b| *b = AES_SBOX[*b as usize]);
        shift_rows(s);
        mix_columns(s);
        add_round_key(s, rk);
    }
    s.iter_mut().for_each(|b| *b = AES_SBOX[*b as usize]);
    shift_rows(s);
    add_round_key(s, &rks[nr]);
}

fn aes_decrypt_block(s: &mut [u8; 16], rks: &[[u8; 16]], inv_sbox: &[u8; 256]) {
    let nr = rks.len() - 1;
    add_round_key(s, &rks[nr]);
    for rk in rks[1..nr].iter().rev() {
        inv_shift_rows(s);
        s.iter_mut().for_each(|b| *b = inv_sbox[*b as usize]);
        add_round_key(s, rk);
        inv_mix_columns(s);
    }
    inv_shift_rows(s);
    s.iter_mut().for_each(|b| *b = inv_sbox[*b as usize]);
    add_round_key(s, &rks[0]);
}

/// AES-CBC decryption of whole blocks (no IV prefix, no padding removal).
/// Returns an empty vector when the input is not a positive multiple of 16.
fn aes_cbc_decrypt_blocks(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    if data.is_empty() || !data.len().is_multiple_of(16) || iv.len() < 16 {
        return Vec::new();
    }
    let rks = aes_expand_key(key);
    let inv_sbox = aes_inv_sbox();
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..16]);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        let cipher = block;
        aes_decrypt_block(&mut block, &rks, &inv_sbox);
        for (b, p) in block.iter_mut().zip(&prev) {
            *b ^= p;
        }
        out.extend_from_slice(&block);
        prev = cipher;
    }
    out
}

/// AES-CBC encryption of whole blocks (no IV prefix, no padding). Used only by
/// the R6 key-derivation hash.
fn aes_cbc_encrypt_blocks(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    let rks = aes_expand_key(key);
    let mut prev = [0u8; 16];
    prev.copy_from_slice(&iv[..16]);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = [0u8; 16];
        for ((b, c), p) in block.iter_mut().zip(chunk).zip(&prev) {
            *b = c ^ p;
        }
        aes_encrypt_block(&mut block, &rks);
        out.extend_from_slice(&block);
        prev = block;
    }
    out
}

/// Decrypts a PDF AES value: the first 16 bytes are the IV, the rest is
/// CBC-encrypted with PKCS#7 padding. Malformed input yields empty output
/// rather than garbage.
fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    if data.len() < 16 {
        return Vec::new();
    }
    let (iv, ct) = data.split_at(16);
    let mut out = aes_cbc_decrypt_blocks(key, iv, ct);
    strip_pkcs7(&mut out);
    out
}

/// Removes PKCS#7 padding in place if present and well-formed.
fn strip_pkcs7(data: &mut Vec<u8>) {
    let Some(&pad) = data.last() else {
        return;
    };
    let pad = pad as usize;
    if (1..=16).contains(&pad) && pad <= data.len() {
        let start = data.len() - pad;
        if data[start..].iter().all(|&b| b as usize == pad) {
            data.truncate(start);
        }
    }
}

// --- SHA-2 (FIPS 180-4): 256, 512 and 384 --------------------------------

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(input: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = input.to_vec();
    let bitlen = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (word, bytes) in w.iter_mut().zip(chunk.chunks_exact(4)) {
            *word = u32::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in SHA256_K.iter().zip(&w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *hv = hv.wrapping_add(v);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[rustfmt::skip]
const SHA512_K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_core(input: &[u8], mut h: [u64; 8]) -> [u64; 8] {
    let mut msg = input.to_vec();
    let bitlen = (input.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());

    for chunk in msg.chunks_exact(128) {
        let mut w = [0u64; 80];
        for (word, bytes) in w.iter_mut().zip(chunk.chunks_exact(8)) {
            *word = u64::from_be_bytes(bytes.try_into().unwrap());
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (k, wi) in SHA512_K.iter().zip(&w) {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*wi);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *hv = hv.wrapping_add(v);
        }
    }
    h
}

fn sha512(input: &[u8]) -> Vec<u8> {
    let h = sha512_core(
        input,
        [
            0x6a09e667f3bcc908,
            0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,
            0xa54ff53a5f1d36f1,
            0x510e527fade682d1,
            0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,
            0x5be0cd19137e2179,
        ],
    );
    h.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn sha384(input: &[u8]) -> Vec<u8> {
    let h = sha512_core(
        input,
        [
            0xcbbb9d5dc1059ed8,
            0x629a292a367cd507,
            0x9159015a3070dd17,
            0x152fecd8f70e5939,
            0x67332667ffc00b31,
            0x8eb44a8768581511,
            0xdb0c2e0d64f98fa7,
            0x47b5481dbefa4fa4,
        ],
    );
    h.iter().take(6).flat_map(|w| w.to_be_bytes()).collect()
}

/// Recovers the AES-256 file key for the empty user password (ISO 32000-2
/// §7.6.4.3.3, Algorithm 2.A) for revisions 5 and 6.
fn aesv3_key(enc: &Dict, r: i64) -> Option<Vec<u8>> {
    let u = enc.get("U").and_then(Object::as_str_bytes)?;
    let ue = enc.get("UE").and_then(Object::as_str_bytes)?;
    if u.len() < 48 || ue.len() < 32 {
        return None;
    }
    let validation_salt = &u[32..40];
    let key_salt = &u[40..48];
    let pw: &[u8] = b""; // empty user password
    if hash_2b(r, pw, validation_salt, &[])[..32] != u[..32] {
        return None; // empty password does not open the file
    }
    let intermediate = hash_2b(r, pw, key_salt, &[]);
    let file_key = aes_cbc_decrypt_blocks(&intermediate, &[0u8; 16], &ue[..32]);
    (file_key.len() == 32).then_some(file_key)
}

/// The revision-6 password hash (ISO 32000-2, Algorithm 2.B); a plain SHA-256
/// for revision 5.
fn hash_2b(r: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut seed = Vec::with_capacity(password.len() + salt.len() + udata.len());
    seed.extend_from_slice(password);
    seed.extend_from_slice(salt);
    seed.extend_from_slice(udata);
    let mut k = sha256(&seed).to_vec();
    if r < 6 {
        return k; // revision 5: a single SHA-256
    }
    let mut round = 0usize;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }
        let e = aes_cbc_encrypt_blocks(&k[..16], &k[16..32], &k1);
        let modulus = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
        k = match modulus {
            0 => sha256(&e).to_vec(),
            1 => sha384(&e),
            _ => sha512(&e),
        };
        round += 1;
        if round >= 64 && usize::from(*e.last().unwrap()) <= round - 32 {
            break;
        }
    }
    k.truncate(32);
    k
}

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

/// One-shot MD5 (RFC 1321). Sufficient for the small key-derivation inputs; not
/// a streaming API.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn md5_known_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn md5_spans_block_boundary() {
        // 56 bytes forces a second padded block.
        let input = [b'a'; 56];
        assert_eq!(hex(&md5(&input)), "3b0c8ac703f828b04c6c197006d17218");
    }

    #[test]
    fn rc4_known_vector() {
        // Classic RC4 test vector: key "Key", plaintext "Plaintext".
        let ct = rc4(b"Key", b"Plaintext");
        assert_eq!(hex(&ct), "bbf316e8d940af0ad3");
        // Symmetric: decrypting the ciphertext returns the plaintext.
        assert_eq!(rc4(b"Key", &ct), b"Plaintext");
    }

    // --- End-to-end fixture: build a V2/R3 (128-bit RC4) file encrypted under
    // the empty password, then confirm the loader transparently decrypts it. ---

    const N: usize = 16; // 128-bit key
    const P: i32 = -44;
    const ID0: &[u8] = b"0123456789abcdef";

    /// `/O` for empty owner and user passwords (Algorithm 3, R3).
    fn owner_entry() -> Vec<u8> {
        let mut d = md5(&PAD);
        for _ in 0..50 {
            d = md5(&d[..N]);
        }
        let rc4key = d[..N].to_vec();
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
            d = md5(&d[..N]);
        }
        d[..N].to_vec()
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

    fn encrypted_fixture(u_override: Option<Vec<u8>>) -> Vec<u8> {
        use pdfboss_testkit::PdfBuilder;
        let o = owner_entry();
        let key = file_key(&o);
        let u = u_override.unwrap_or_else(|| user_entry(&key));

        let msg = rc4(&obj_key(&key, 3, 0), b"Top secret message");
        let stream = rc4(&obj_key(&key, 4, 0), b"decrypted stream body");

        let mut b = PdfBuilder::new().version(1, 4);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(3, &format!("<< /Msg {} >>", hexstr(&msg)));
        b.stream(4, "", &stream);
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

    #[test]
    fn document_load_decrypts_standard_rc4() {
        use crate::object::ObjRef;
        use crate::Document;

        let doc = Document::load(encrypted_fixture(None)).expect("empty password opens the file");

        let obj3 = doc.get(ObjRef { num: 3, gen: 0 }).unwrap();
        let msg = obj3
            .as_dict()
            .unwrap()
            .get("Msg")
            .unwrap()
            .as_str_bytes()
            .unwrap();
        assert_eq!(msg, b"Top secret message", "string decrypted");

        let obj4 = doc.get(ObjRef { num: 4, gen: 0 }).unwrap();
        let data = doc.stream_data(obj4.as_stream().unwrap()).unwrap();
        assert_eq!(data, b"decrypted stream body", "stream decrypted");
    }

    #[test]
    fn document_load_rejects_when_password_does_not_verify() {
        use crate::error::Error;
        use crate::Document;

        // A `/U` that will not verify under the empty password stands in for a
        // real password-protected file: the loader must decline, not decrypt.
        let bad_u = vec![0u8; 32];
        let err = Document::load(encrypted_fixture(Some(bad_u)));
        assert!(matches!(err, Err(Error::Encrypted)));
    }

    #[test]
    fn unsupported_handler_is_declined() {
        // A future/unknown handler version is declined so the caller reports the
        // file as encrypted-and-unsupported.
        let mut enc = Dict::new();
        enc.insert(Name("Filter".into()), Object::Name(Name("Standard".into())));
        enc.insert(Name("V".into()), Object::Int(6));
        enc.insert(Name("R".into()), Object::Int(7));
        enc.insert(Name("O".into()), Object::String(vec![0; 48]));
        enc.insert(Name("U".into()), Object::String(vec![0; 48]));
        enc.insert(Name("P".into()), Object::Int(-4));
        assert!(Decryptor::from_standard(&enc, ID0).is_none());
    }

    // --- AES / SHA-2 known-answer vectors ---

    #[test]
    fn aes_fips197_block_vectors() {
        // FIPS-197 Appendix C.1 (AES-128) and C.3 (AES-256), same plaintext.
        let pt: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let key128: Vec<u8> = (0u8..16).collect();
        let rks = aes_expand_key(&key128);
        let mut b = pt;
        aes_encrypt_block(&mut b, &rks);
        assert_eq!(hex(&b), "69c4e0d86a7b0430d8cdb78070b4c55a");
        aes_decrypt_block(&mut b, &rks, &aes_inv_sbox());
        assert_eq!(b, pt, "AES-128 decrypt inverts encrypt");

        let key256: Vec<u8> = (0u8..32).collect();
        let rks = aes_expand_key(&key256);
        let mut b = pt;
        aes_encrypt_block(&mut b, &rks);
        assert_eq!(hex(&b), "8ea2b7ca516745bfeafc49904b496089");
    }

    #[test]
    fn sha2_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha512(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed\
             8086072ba1e7cc2358baeca134c825a7"
        );
    }

    fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
        let pad = 16 - (data.len() % 16); // 1..=16 (a full block when aligned)
        let mut v = data.to_vec();
        v.resize(data.len() + pad, pad as u8);
        v
    }

    #[test]
    fn aes_cbc_roundtrip() {
        let key: Vec<u8> = (0u8..16).collect();
        let iv = [0x24u8; 16];
        let pt = b"a message spanning several AES blocks exactly?!!";
        let ct = aes_cbc_encrypt_blocks(&key, &iv, &pkcs7_pad(pt));
        let mut val = iv.to_vec(); // PDF format: IV followed by ciphertext
        val.extend_from_slice(&ct);
        assert_eq!(aes_cbc_decrypt(&key, &val), pt);
    }

    // --- AESV2 (V4/R4) end-to-end fixture ---

    fn obj_key_aes(key: &[u8], num: u32, gen: u16) -> Vec<u8> {
        let mut input = key.to_vec();
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        input.extend_from_slice(b"sAlT");
        md5(&input)[..(key.len() + 5).min(16)].to_vec()
    }

    /// Encrypts as a PDF AES value: a 16-byte IV followed by CBC ciphertext of
    /// the PKCS#7-padded plaintext.
    fn aes_encrypt_pdf(key: &[u8], pt: &[u8], iv: &[u8; 16]) -> Vec<u8> {
        let mut out = iv.to_vec();
        out.extend_from_slice(&aes_cbc_encrypt_blocks(key, iv, &pkcs7_pad(pt)));
        out
    }

    fn encrypted_fixture_aesv2() -> Vec<u8> {
        use pdfboss_testkit::PdfBuilder;
        let o = owner_entry();
        let key = file_key(&o); // R4 derivation matches R3 (EncryptMetadata true)
        let u = user_entry(&key);
        let iv = [0x11u8; 16];
        let msg = aes_encrypt_pdf(&obj_key_aes(&key, 3, 0), b"Top secret message", &iv);
        let stream = aes_encrypt_pdf(&obj_key_aes(&key, 4, 0), b"decrypted stream body", &iv);

        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(3, &format!("<< /Msg {} >>", hexstr(&msg)));
        b.stream(4, "", &stream);
        b.object(
            9,
            &format!(
                "<< /Filter /Standard /V 4 /R 4 /Length 128 /P {P} /O {} /U {} \
                 /CF << /StdCF << /CFM /AESV2 /Length 16 >> >> /StmF /StdCF /StrF /StdCF >>",
                hexstr(&o),
                hexstr(&u)
            ),
        );
        let trailer = format!("/Encrypt 9 0 R /ID [{}{}]", hexstr(ID0), hexstr(ID0));
        b.trailer_extra(&trailer).build(1)
    }

    #[test]
    fn document_load_decrypts_aesv2() {
        use crate::object::ObjRef;
        use crate::Document;
        let doc = Document::load(encrypted_fixture_aesv2()).expect("AESV2 empty password opens");
        let obj3 = doc.get(ObjRef { num: 3, gen: 0 }).unwrap();
        let msg = obj3
            .as_dict()
            .unwrap()
            .get("Msg")
            .unwrap()
            .as_str_bytes()
            .unwrap();
        assert_eq!(msg, b"Top secret message");
        let obj4 = doc.get(ObjRef { num: 4, gen: 0 }).unwrap();
        assert_eq!(
            doc.stream_data(obj4.as_stream().unwrap()).unwrap(),
            b"decrypted stream body"
        );
    }

    // --- AESV3 (V5/R5 and R6) end-to-end fixture ---

    fn encrypted_fixture_aesv3(r: i64) -> Vec<u8> {
        use pdfboss_testkit::PdfBuilder;
        let key: Vec<u8> = (0u8..32).map(|i| i ^ 0x5a).collect(); // arbitrary 256-bit file key
        let vsalt: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let ksalt: [u8; 8] = [9, 10, 11, 12, 13, 14, 15, 16];
        let mut u = hash_2b(r, b"", &vsalt, &[]); // 32-byte validation hash
        u.extend_from_slice(&vsalt);
        u.extend_from_slice(&ksalt);
        let intermediate = hash_2b(r, b"", &ksalt, &[]);
        let ue = aes_cbc_encrypt_blocks(&intermediate, &[0u8; 16], &key);
        let iv = [0x22u8; 16];
        let msg = aes_encrypt_pdf(&key, b"AES-256 secret", &iv);
        let stream = aes_encrypt_pdf(&key, b"AES-256 stream body", &iv);

        let mut b = PdfBuilder::new().version(1, 7);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.object(3, &format!("<< /Msg {} >>", hexstr(&msg)));
        b.stream(4, "", &stream);
        b.object(
            9,
            &format!(
                "<< /Filter /Standard /V 5 /R {r} /Length 256 /P {P} /U {} /UE {} \
                 /O {} /OE {} \
                 /CF << /StdCF << /CFM /AESV3 /Length 32 >> >> /StmF /StdCF /StrF /StdCF >>",
                hexstr(&u),
                hexstr(&ue),
                hexstr(&[0u8; 48]),
                hexstr(&[0u8; 32])
            ),
        );
        let trailer = format!("/Encrypt 9 0 R /ID [{}{}]", hexstr(ID0), hexstr(ID0));
        b.trailer_extra(&trailer).build(1)
    }

    fn assert_aesv3_decrypts(r: i64) {
        use crate::object::ObjRef;
        use crate::Document;
        let doc = Document::load(encrypted_fixture_aesv3(r)).expect("AESV3 empty password opens");
        let obj3 = doc.get(ObjRef { num: 3, gen: 0 }).unwrap();
        let msg = obj3
            .as_dict()
            .unwrap()
            .get("Msg")
            .unwrap()
            .as_str_bytes()
            .unwrap();
        assert_eq!(msg, b"AES-256 secret", "R{r} string");
        let obj4 = doc.get(ObjRef { num: 4, gen: 0 }).unwrap();
        assert_eq!(
            doc.stream_data(obj4.as_stream().unwrap()).unwrap(),
            b"AES-256 stream body",
            "R{r} stream"
        );
    }

    #[test]
    fn document_load_decrypts_aesv3_r5() {
        assert_aesv3_decrypts(5);
    }

    #[test]
    fn document_load_decrypts_aesv3_r6() {
        assert_aesv3_decrypts(6); // exercises the iterated Algorithm 2.B hash
    }
}
