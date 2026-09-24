//! Decryption for PDFs that open with an empty user password: the standard security handler,
//! revisions 2 to 4 (RC4 40 to 128 bit, and AES-128 "AESV2"). Revisions 5 and 6 (AES-256) are
//! not supported; such files stay "unknown". Only streams are decrypted: that's all the
//! classifier reads (content streams, form XObjects, object streams).
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
use md5::{Digest, Md5};

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

pub struct Crypt { key: Vec<u8>, aes: bool }

impl Crypt {
    /// Algorithm 2 (ISO 32000-1, 7.6.3.3) with the empty user password.
    /// `o` is the /O string, `p` the /P permissions, `id0` the first /ID string.
    pub fn new(r: u32, length_bits: u32, o: &[u8], p: i32, id0: &[u8], aes: bool, encrypt_metadata: bool) -> Option<Crypt> {
        if !(2..=4).contains(&r) || o.len() < 32 { return None; }
        let n = if r == 2 { 5 } else { (length_bits / 8).clamp(5, 16) as usize };
        let mut h = Md5::new();
        h.update(PAD);
        h.update(&o[..32]);
        h.update((p as u32).to_le_bytes());
        h.update(id0);
        if r >= 4 && !encrypt_metadata { h.update([0xff, 0xff, 0xff, 0xff]); }
        let mut key = h.finalize().to_vec();
        if r >= 3 {
            for _ in 0..50 { key = Md5::digest(&key[..n]).to_vec(); }
        }
        key.truncate(n);
        Some(Crypt { key, aes })
    }

    /// Decrypt one stream of object `num` (generation `gen`).
    pub fn decrypt(&self, num: u32, gen: u16, data: &[u8]) -> Option<Vec<u8>> {
        let mut h = Md5::new();
        h.update(&self.key);
        h.update(&num.to_le_bytes()[..3]);
        h.update(gen.to_le_bytes());
        if self.aes { h.update(b"sAlT"); }
        let digest = h.finalize();
        let k = &digest[..(self.key.len() + 5).min(16)];
        if self.aes { aes128_cbc(k, data) } else { Some(rc4(k, data)) }
    }
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = core::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter().map(|&b| {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
    }).collect()
}

/// AES-128-CBC with the IV in the first 16 bytes, PKCS#7 padding removed if present.
fn aes128_cbc(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    // The stream slice can carry the end-of-line before "endstream"; keep whole blocks only.
    let data = &data[..data.len() - data.len() % 16];
    if key.len() != 16 || data.len() < 32 { return None; }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let mut prev: [u8; 16] = data[..16].try_into().ok()?;
    let mut out = Vec::with_capacity(data.len() - 16);
    for chunk in data[16..].chunks_exact(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for (b, p) in block.iter_mut().zip(prev.iter()) { *b ^= p; }
        out.extend_from_slice(&block);
        prev.copy_from_slice(chunk);
    }
    if let Some(&pad) = out.last() {
        let pad = pad as usize;
        if (1..=16).contains(&pad) && out.len() >= pad && out[out.len() - pad..].iter().all(|&b| b as usize == pad) {
            out.truncate(out.len() - pad);
        }
    }
    Some(out)
}
