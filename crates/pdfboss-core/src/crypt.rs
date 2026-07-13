//! Decryption for the Standard security handler (ISO 32000 §7.6), RC4 variants
//! only, opened with the empty user password.
//!
//! Handles `/V` 1–2 with `/R` 2–3 (40- to 128-bit RC4). AES handlers (`/V` 4–5)
//! and non-empty passwords are not supported: those documents are reported as
//! encrypted-and-unsupported by the caller. MD5 and RC4 are implemented here
//! from their published specifications so the crate needs no cryptographic
//! dependency.

use crate::object::{Dict, Object};

/// Password padding string (ISO 32000 §7.6.3.3, Algorithm 2, step (a)).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// A configured RC4 Standard-handler decryptor for a document opened with the
/// empty user password.
pub(crate) struct Decryptor {
    /// The `n`-byte file encryption key.
    key: Vec<u8>,
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
        if !matches!(v, 1 | 2) || !matches!(r, 2 | 3) {
            return None; // AES (V4/V5) and other revisions are out of scope.
        }
        let o = enc.get("O").and_then(Object::as_str_bytes)?;
        if o.len() < 32 {
            return None;
        }
        let p = enc.get_int("P")?;
        let length_bits = if v == 1 {
            40
        } else {
            enc.get_int("Length").unwrap_or(40)
        };
        let n = (length_bits / 8).clamp(5, 16) as usize;

        // Algorithm 2: derive the file key from the (empty) user password.
        let mut input = Vec::with_capacity(32 + 32 + 4 + id0.len());
        input.extend_from_slice(&PAD); // padded empty password == the pad itself
        input.extend_from_slice(&o[..32]); // /O entry
        input.extend_from_slice(&(p as i32 as u32).to_le_bytes()); // /P, low 32 bits, LE
        input.extend_from_slice(id0); // first /ID element
                                      // (R>=4's /EncryptMetadata step is not reachable here: only R2/R3.)
        let mut digest = md5(&input);
        if r >= 3 {
            for _ in 0..50 {
                digest = md5(&digest[..n]);
            }
        }
        let key = digest[..n].to_vec();

        // Verify the empty user password against /U (Algorithm 4 for R2,
        // Algorithm 5 for R3). A mismatch means a real password is required.
        let u = enc.get("U").and_then(Object::as_str_bytes)?;
        if !verify_user_password(&key, r, id0, u) {
            return None;
        }
        Some(Decryptor { key })
    }

    /// RC4-decrypts one indirect object's strings and stream data in place with
    /// its per-object key (Algorithm 1). Objects extracted from object streams
    /// are already plaintext and must not be passed here.
    pub(crate) fn decrypt_object(&self, obj: &mut Object, num: u32, gen: u16) {
        let key = self.object_key(num, gen);
        decrypt_in_place(obj, &key);
    }

    /// Per-object key: `MD5(filekey ++ num[0..3] ++ gen[0..2])` truncated to
    /// `min(n + 5, 16)` bytes (ISO 32000 §7.6.2, Algorithm 1).
    fn object_key(&self, num: u32, gen: u16) -> Vec<u8> {
        let mut input = Vec::with_capacity(self.key.len() + 5);
        input.extend_from_slice(&self.key);
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        let digest = md5(&input);
        let n = (self.key.len() + 5).min(16);
        digest[..n].to_vec()
    }
}

/// Recursively RC4-decrypts every string and stream body reachable from `obj`
/// with the given per-object `key`.
fn decrypt_in_place(obj: &mut Object, key: &[u8]) {
    match obj {
        Object::String(bytes) => *bytes = rc4(key, bytes),
        Object::Array(items) => items.iter_mut().for_each(|it| decrypt_in_place(it, key)),
        Object::Dict(dict) => dict.values_mut().for_each(|v| decrypt_in_place(v, key)),
        Object::Stream(stream) => {
            stream
                .dict
                .values_mut()
                .for_each(|v| decrypt_in_place(v, key));
            stream.data = rc4(key, &stream.data);
        }
        _ => {}
    }
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
        // AES (V4/R4) is out of scope: from_standard returns None so the caller
        // reports the file as encrypted-and-unsupported.
        let mut enc = Dict::new();
        enc.insert(Name("Filter".into()), Object::Name(Name("Standard".into())));
        enc.insert(Name("V".into()), Object::Int(4));
        enc.insert(Name("R".into()), Object::Int(4));
        enc.insert(Name("O".into()), Object::String(vec![0; 32]));
        enc.insert(Name("U".into()), Object::String(vec![0; 32]));
        enc.insert(Name("P".into()), Object::Int(-4));
        assert!(Decryptor::from_standard(&enc, ID0).is_none());
    }
}
