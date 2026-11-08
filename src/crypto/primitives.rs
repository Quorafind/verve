//! AES-GCM and AES-CBC on bytes, for formats an embedder keeps compatible with Chromium's: the
//! ciphertexts of Electron's `safeStorage`. The ciphers are `crypto.rs`'s, the ones `crypto.subtle`
//! runs; this is the way in that has no JavaScript value in between. A child module of `crypto`, so
//! it reaches the ciphers without making them visible to the rest of the crate.

use super::{Aes, cbc_blocks, gcm_open, gcm_seal, pad, unpad};

/// AES-GCM with a 16-byte tag after the ciphertext; the key is 16, 24 or 32 bytes.
pub fn aes_gcm_seal(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    gcm_seal(key, iv, &[], data, 16)
}

/// The other way; `None` when the key or the tag is not the data's.
pub fn aes_gcm_open(key: &[u8], iv: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    gcm_open(key, iv, &[], sealed, 16)
}

/// AES-CBC with PKCS#7 padding.
pub fn aes_cbc_encrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(key)?;
    let mut padded = data.to_vec();
    pad(&mut padded);
    Some(cbc_blocks(&aes, Some(&mut { *iv }), &padded, true))
}

/// The other way; `None` when the key or the padding is not the data's.
pub fn aes_cbc_decrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(key)?;
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }
    unpad(cbc_blocks(&aes, Some(&mut { *iv }), data, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex digits"))
            .collect()
    }

    #[test]
    fn cbc_matches_nist_sp_800_38a_and_round_trips() {
        // F.2.1 (CBC-AES128), first block; PKCS#7 adds a block after it.
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .expect("16 bytes");
        let plain = hex("6bc1bee22e409f96e93d7e117393172a");
        let sealed = aes_cbc_encrypt(&key, &iv, &plain).expect("a 16-byte key");
        assert_eq!(
            crate::encoding::hex(&sealed[..16]),
            "7649abac8119b246cee98e9b12e9197d"
        );
        assert_eq!(
            aes_cbc_decrypt(&key, &iv, &sealed).as_deref(),
            Some(&plain[..])
        );
        assert!(aes_cbc_decrypt(&key, &iv, &sealed[..15]).is_none());
    }

    #[test]
    fn gcm_round_trips_and_refuses_a_changed_byte() {
        let key = [3u8; 32];
        let sealed = aes_gcm_seal(&key, &[9; 12], b"hello").expect("a 32-byte key");
        assert_eq!(sealed.len(), 5 + 16);
        assert_eq!(
            aes_gcm_open(&key, &[9; 12], &sealed).as_deref(),
            Some(&b"hello"[..])
        );
        let mut changed = sealed;
        changed[0] ^= 1;
        assert!(aes_gcm_open(&key, &[9; 12], &changed).is_none());
    }
}
