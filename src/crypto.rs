//! Cryptography: the web's `crypto` (`getRandomValues`, `randomUUID`, `crypto.subtle`) and
//! Node's `crypto` module, both on RustCrypto's crates — digests, HMAC, PBKDF2, HKDF, scrypt and
//! AES — and the system's randomness.
//!
//! What plugins do with it is mostly three things: hash text (`createHash('sha256')`, `md5` for
//! a cache key), make random ids, and encrypt notes for sync — AES-GCM through `crypto.subtle`
//! with a key derived from a passphrase by PBKDF2 (Meld Encrypt, Self-hosted LiveSync, Remotely
//! Save). Those are here whole. Of AES's modes, GCM, CBC, CTR, ECB and the key wrap are written
//! here over the block cipher itself, so each can be fed a piece at a time as Node's ciphers
//! are, and GCM takes any length of IV and of tag.
//!
//! Public-key cryptography — RSA, elliptic curves, Ed25519 — is not here: asking for it answers
//! a `NotSupportedError` (web) or throws `ERR_FEATURE_UNAVAILABLE_ON_PLATFORM` (Node).

use std::cell::RefCell;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    Trace, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArray, JsUint8Array},
};
use ghash::universal_hash::UniversalHash;
use sha2::digest::DynDigest;

use crate::native::{
    arg, array, closure, coded_error, function, get, hidden, js_text, methods, object, text_of,
    type_error,
};

// ─── Randomness ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn random_bytes(count: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; count];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

fn random_uuid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = to_hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Fills the bytes of a typed array (or `DataView`, or `ArrayBuffer`) in place with random ones.
fn fill_random(
    target: &JsValue,
    offset: Option<usize>,
    size: Option<usize>,
    context: &mut Context,
) -> JsResult<()> {
    let object = target.as_object().ok_or_else(|| {
        type_error("The \"buf\" argument must be an ArrayBuffer or ArrayBufferView")
    })?;
    let (buffer, start, length) = if let Ok(buffer) =
        boa_engine::object::builtins::JsArrayBuffer::from_object(object.clone())
    {
        let length = buffer.data().map_or(0, |d| d.len());
        (JsValue::from(object.clone()), 0, length)
    } else {
        let buffer = object.get(js_string!("buffer"), context)?;
        let start = object
            .get(js_string!("byteOffset"), context)?
            .to_number(context)? as usize;
        let length = object
            .get(js_string!("byteLength"), context)?
            .to_number(context)? as usize;
        (buffer, start, length)
    };
    let from = offset.unwrap_or(0).min(length);
    let count = size.unwrap_or(length - from).min(length - from);
    let view = context
        .intrinsics()
        .constructors()
        .typed_uint8_array()
        .constructor()
        .construct(
            &[buffer, (start + from).into(), count.into()],
            None,
            context,
        )?;
    let view = JsUint8Array::from_object(view)?;
    for (at, byte) in random_bytes(count).into_iter().enumerate() {
        view.set(at, byte, true, context)?;
    }
    Ok(())
}

// ─── Text and bytes ─────────────────────────────────────────────────────────────────────────

fn to_hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map_while(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// A string as bytes in one of Node's encodings.
fn encode_text(text: &str, encoding: &str) -> JsResult<Vec<u8>> {
    Ok(match encoding.to_ascii_lowercase().as_str() {
        "" | "utf8" | "utf-8" | "buffer" => text.as_bytes().to_vec(),
        "hex" => from_hex(text),
        "base64" | "base64url" => crate::encoding::unbase64(text).unwrap_or_default(),
        "latin1" | "binary" | "ascii" => text.chars().map(|c| c as u32 as u8).collect(),
        "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => {
            text.encode_utf16().flat_map(u16::to_le_bytes).collect()
        }
        other => return Err(type_error(format!("Unknown encoding: {other}"))),
    })
}

/// What `data` is as bytes: a string in `encoding` (UTF-8 by default), or a buffer, typed array,
/// `DataView` or `ArrayBuffer` as it is.
fn data_bytes(data: &JsValue, encoding: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if data.is_string() {
        let text = text_of(data, context)?;
        let encoding = if encoding.is_string() {
            text_of(encoding, context)?
        } else {
            String::new()
        };
        return encode_text(&text, &encoding);
    }
    if let Some(bytes) = crate::encoding::bytes_of(data, context)? {
        return Ok(bytes);
    }
    // A `KeyObject`.
    if let Some(bytes) = secret_of(data) {
        return Ok(bytes);
    }
    Err(coded_error(
        context,
        &format!(
            "The \"data\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received {}",
            data.type_of()
        ),
        "ERR_INVALID_ARG_TYPE",
    ))
}

/// Bytes as Node answers them: a `Buffer`, or a string in `encoding`.
fn bytes_out(bytes: Vec<u8>, encoding: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    if !encoding.is_string() {
        return crate::encoding::make_buffer(context, bytes);
    }
    let encoding = text_of(encoding, context)?.to_ascii_lowercase();
    let text = match encoding.as_str() {
        "buffer" => return crate::encoding::make_buffer(context, bytes),
        "hex" => to_hex(&bytes),
        "base64" => crate::encoding::base64(&bytes, false),
        "base64url" => crate::encoding::base64(&bytes, true),
        "latin1" | "binary" => bytes.iter().map(|b| char::from(*b)).collect(),
        "ascii" => bytes.iter().map(|b| char::from(b & 0x7f)).collect(),
        "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(&bytes).into_owned(),
    };
    Ok(js_text(&text))
}

fn array_buffer(bytes: Vec<u8>, context: &mut Context) -> JsResult<JsValue> {
    JsUint8Array::from_iter(bytes, context)?.buffer(context)
}

// ─── Digests and HMAC ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HashAlg {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    Sha512_224,
    Sha512_256,
}

/// Runs `$body` with `$H` the RustCrypto type of the hash `$alg`.
macro_rules! with_hash {
    ($alg:expr, $H:ident => $body:expr) => {
        match $alg {
            HashAlg::Md5 => {
                type $H = md5::Md5;
                $body
            }
            HashAlg::Sha1 => {
                type $H = sha1::Sha1;
                $body
            }
            HashAlg::Sha224 => {
                type $H = sha2::Sha224;
                $body
            }
            HashAlg::Sha256 => {
                type $H = sha2::Sha256;
                $body
            }
            HashAlg::Sha384 => {
                type $H = sha2::Sha384;
                $body
            }
            HashAlg::Sha512 => {
                type $H = sha2::Sha512;
                $body
            }
            HashAlg::Sha512_224 => {
                type $H = sha2::Sha512_224;
                $body
            }
            HashAlg::Sha512_256 => {
                type $H = sha2::Sha512_256;
                $body
            }
        }
    };
}

impl HashAlg {
    /// A hash by any of the names Node and the web call it: `sha256`, `SHA-256`, `RSA-SHA256`,
    /// `sha-256`.
    fn named(name: &str) -> Option<HashAlg> {
        let lower = name.to_ascii_lowercase();
        let lower = lower.strip_prefix("rsa-").unwrap_or(&lower);
        Some(match lower.replace(['-', '_'], "").as_str() {
            "md5" => HashAlg::Md5,
            "sha1" => HashAlg::Sha1,
            "sha224" => HashAlg::Sha224,
            "sha256" => HashAlg::Sha256,
            "sha384" => HashAlg::Sha384,
            "sha512" => HashAlg::Sha512,
            "sha512/224" | "sha512224" => HashAlg::Sha512_224,
            "sha512/256" | "sha512256" => HashAlg::Sha512_256,
            _ => return None,
        })
    }

    /// The name the web gives it (`SHA-256`), for a key's `algorithm.hash.name`.
    fn web_name(self) -> &'static str {
        match self {
            HashAlg::Md5 => "MD5",
            HashAlg::Sha1 => "SHA-1",
            HashAlg::Sha224 => "SHA-224",
            HashAlg::Sha256 => "SHA-256",
            HashAlg::Sha384 => "SHA-384",
            HashAlg::Sha512 => "SHA-512",
            HashAlg::Sha512_224 => "SHA-512/224",
            HashAlg::Sha512_256 => "SHA-512/256",
        }
    }

    fn block_size(self) -> usize {
        match self {
            HashAlg::Md5 | HashAlg::Sha1 | HashAlg::Sha224 | HashAlg::Sha256 => 64,
            _ => 128,
        }
    }

    fn hasher(self) -> Box<dyn DynDigest> {
        with_hash!(self, H => Box::new(H::default()))
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        let mut hasher = self.hasher();
        hasher.update(data);
        hasher.finalize().into_vec()
    }
}

/// HMAC over any of the hashes, kept as its two running digests so it can be fed a piece at a
/// time.
struct Mac {
    inner: Box<dyn DynDigest>,
    outer: Box<dyn DynDigest>,
}

impl Mac {
    fn new(alg: HashAlg, key: &[u8]) -> Mac {
        let block = alg.block_size();
        let mut key = if key.len() > block {
            alg.digest(key)
        } else {
            key.to_vec()
        };
        key.resize(block, 0);
        let mut inner = alg.hasher();
        let mut outer = alg.hasher();
        inner.update(&key.iter().map(|b| b ^ 0x36).collect::<Vec<_>>());
        outer.update(&key.iter().map(|b| b ^ 0x5c).collect::<Vec<_>>());
        Mac { inner, outer }
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finish(self) -> Vec<u8> {
        let Mac { inner, mut outer } = self;
        outer.update(&inner.finalize());
        outer.finalize().into_vec()
    }

    fn copy(&self) -> Mac {
        Mac {
            inner: self.inner.box_clone(),
            outer: self.outer.box_clone(),
        }
    }

    fn once(alg: HashAlg, key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = Mac::new(alg, key);
        mac.update(data);
        mac.finish()
    }
}

fn pbkdf2(alg: HashAlg, password: &[u8], salt: &[u8], iterations: u32, length: usize) -> Vec<u8> {
    let mut out = vec![0u8; length];
    with_hash!(alg, H => pbkdf2::pbkdf2_hmac::<H>(password, salt, iterations, &mut out));
    out
}

fn hkdf(alg: HashAlg, key: &[u8], salt: &[u8], info: &[u8], length: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; length];
    let ok =
        with_hash!(alg, H => hkdf::Hkdf::<H>::new(Some(salt), key).expand(info, &mut out).is_ok());
    ok.then_some(out)
}

/// Whether two byte strings are the same, taking as long whichever byte differs.
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ─── AES and its modes ──────────────────────────────────────────────────────────────────────

/// The AES block cipher at whichever of its three key sizes the key is.
#[derive(Clone)]
enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

type Block = [u8; 16];

impl Aes {
    fn new(key: &[u8]) -> Option<Aes> {
        Some(match key.len() {
            16 => Aes::A128(aes::Aes128::new(GenericArray::from_slice(key))),
            24 => Aes::A192(aes::Aes192::new(GenericArray::from_slice(key))),
            32 => Aes::A256(aes::Aes256::new(GenericArray::from_slice(key))),
            _ => return None,
        })
    }

    fn encrypt(&self, block: &mut Block) {
        let block = GenericArray::from_mut_slice(block);
        match self {
            Aes::A128(c) => c.encrypt_block(block),
            Aes::A192(c) => c.encrypt_block(block),
            Aes::A256(c) => c.encrypt_block(block),
        }
    }

    fn decrypt(&self, block: &mut Block) {
        let block = GenericArray::from_mut_slice(block);
        match self {
            Aes::A128(c) => c.decrypt_block(block),
            Aes::A192(c) => c.decrypt_block(block),
            Aes::A256(c) => c.decrypt_block(block),
        }
    }
}

/// Adds one to the rightmost `bits` bits of a counter block, wrapping within them — AES-CTR's
/// `length`, and GCM's 32.
fn increment(counter: &mut Block, bits: u32) {
    let mut carry = true;
    let mut left = bits;
    for byte in counter.iter_mut().rev() {
        if !carry || left == 0 {
            break;
        }
        let width = left.min(8);
        let mask: u16 = (1u16 << width) - 1;
        let low = u16::from(*byte) & mask;
        let next = low + 1;
        *byte = (*byte & !(mask as u8)) | ((next & mask) as u8);
        carry = next > mask;
        left -= width;
    }
}

/// A keystream from a counter: CTR mode, and the encryption half of GCM.
#[derive(Clone)]
struct Ctr {
    aes: Aes,
    counter: Block,
    bits: u32,
    stream: Block,
    used: usize,
    /// Whether the counter is incremented before its first block (GCM) or after (CTR).
    pre_increment: bool,
}

impl Ctr {
    fn new(aes: Aes, counter: Block, bits: u32, pre_increment: bool) -> Ctr {
        Ctr {
            aes,
            counter,
            bits,
            stream: [0; 16],
            used: 16,
            pre_increment,
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            if self.used == 16 {
                if self.pre_increment {
                    increment(&mut self.counter, self.bits);
                }
                self.stream = self.counter;
                self.aes.encrypt(&mut self.stream);
                if !self.pre_increment {
                    increment(&mut self.counter, self.bits);
                }
                self.used = 0;
            }
            *byte ^= self.stream[self.used];
            self.used += 1;
        }
    }
}

/// GCM: CTR from the IV's counter block, and GHASH over the additional data and the ciphertext,
/// fed a piece at a time.
struct Gcm {
    aes: Aes,
    ctr: Ctr,
    j0: Block,
    ghash: ghash::GHash,
    aad: Vec<u8>,
    aad_done: bool,
    aad_len: u64,
    text_len: u64,
    pending: Vec<u8>,
    encrypt: bool,
}

impl Gcm {
    fn new(aes: Aes, iv: &[u8], encrypt: bool) -> Gcm {
        let mut h = [0u8; 16];
        aes.encrypt(&mut h);
        let ghash_key = GenericArray::from(h);
        let j0: Block = if iv.len() == 12 {
            let mut j0 = [0u8; 16];
            j0[..12].copy_from_slice(iv);
            j0[15] = 1;
            j0
        } else {
            let mut g = <ghash::GHash as KeyInit>::new(&ghash_key);
            g.update_padded(iv);
            let mut lengths = [0u8; 16];
            lengths[8..].copy_from_slice(&((iv.len() as u64) * 8).to_be_bytes());
            g.update(&[GenericArray::from(lengths)]);
            g.finalize().into()
        };
        Gcm {
            ctr: Ctr::new(aes.clone(), j0, 32, true),
            aes,
            j0,
            ghash: <ghash::GHash as KeyInit>::new(&ghash_key),
            aad: Vec::new(),
            aad_done: false,
            aad_len: 0,
            text_len: 0,
            pending: Vec::new(),
            encrypt,
        }
    }

    fn end_aad(&mut self) {
        if !self.aad_done {
            self.ghash.update_padded(&self.aad);
            self.aad_len = self.aad.len() as u64;
            self.aad_done = true;
        }
    }

    fn hash_text(&mut self, text: &[u8]) {
        self.pending.extend_from_slice(text);
        let whole = self.pending.len() / 16 * 16;
        if whole > 0 {
            let blocks: Vec<_> = self.pending[..whole]
                .chunks_exact(16)
                .map(GenericArray::clone_from_slice)
                .collect();
            self.ghash.update(&blocks);
            self.pending.drain(..whole);
        }
    }

    fn update(&mut self, data: &[u8]) -> Vec<u8> {
        self.end_aad();
        let mut out = data.to_vec();
        if !self.encrypt {
            self.hash_text(data);
        }
        self.ctr.apply(&mut out);
        if self.encrypt {
            let copy = out.clone();
            self.hash_text(&copy);
        }
        self.text_len += data.len() as u64;
        out
    }

    /// The full sixteen-byte tag; a shorter one is its first bytes.
    fn tag(mut self) -> Block {
        self.end_aad();
        let pending = std::mem::take(&mut self.pending);
        self.ghash.update_padded(&pending);
        let mut lengths = [0u8; 16];
        lengths[..8].copy_from_slice(&(self.aad_len * 8).to_be_bytes());
        lengths[8..].copy_from_slice(&(self.text_len * 8).to_be_bytes());
        self.ghash.update(&[GenericArray::from(lengths)]);
        let s: Block = self.ghash.finalize().into();
        let mut e = self.j0;
        self.aes.encrypt(&mut e);
        let mut tag = [0u8; 16];
        for i in 0..16 {
            tag[i] = s[i] ^ e[i];
        }
        tag
    }
}

/// AES-GCM all at once: the ciphertext with the tag after it, as the web answers it.
fn gcm_seal(key: &[u8], iv: &[u8], aad: &[u8], data: &[u8], tag_len: usize) -> Option<Vec<u8>> {
    let mut gcm = Gcm::new(Aes::new(key)?, iv, true);
    gcm.aad = aad.to_vec();
    let mut out = gcm.update(data);
    out.extend_from_slice(&gcm.tag()[..tag_len]);
    Some(out)
}

/// The other way: `None` when the tag is not the data's.
fn gcm_open(key: &[u8], iv: &[u8], aad: &[u8], sealed: &[u8], tag_len: usize) -> Option<Vec<u8>> {
    if sealed.len() < tag_len {
        return None;
    }
    let (data, tag) = sealed.split_at(sealed.len() - tag_len);
    let mut gcm = Gcm::new(Aes::new(key)?, iv, false);
    gcm.aad = aad.to_vec();
    let out = gcm.update(data);
    same_bytes(&gcm.tag()[..tag_len], tag).then_some(out)
}

fn pad(data: &mut Vec<u8>) {
    let fill = 16 - data.len() % 16;
    data.extend(std::iter::repeat_n(fill as u8, fill));
}

fn unpad(mut data: Vec<u8>) -> Option<Vec<u8>> {
    let fill = *data.last()? as usize;
    if fill == 0
        || fill > 16
        || fill > data.len()
        || !data[data.len() - fill..]
            .iter()
            .all(|b| *b as usize == fill)
    {
        return None;
    }
    data.truncate(data.len() - fill);
    Some(data)
}

/// CBC (or, with no IV, ECB) over whole blocks.
fn cbc_blocks(aes: &Aes, iv: Option<&mut Block>, data: &[u8], encrypt: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut chain = iv;
    for chunk in data.chunks_exact(16) {
        let mut block: Block = chunk.try_into().expect("a whole block");
        if encrypt {
            if let Some(prev) = chain.as_deref() {
                for i in 0..16 {
                    block[i] ^= prev[i];
                }
            }
            aes.encrypt(&mut block);
            if let Some(prev) = chain.as_deref_mut() {
                *prev = block;
            }
        } else {
            let cipher = block;
            aes.decrypt(&mut block);
            if let Some(prev) = chain.as_deref_mut() {
                for i in 0..16 {
                    block[i] ^= prev[i];
                }
                *prev = cipher;
            }
        }
        out.extend_from_slice(&block);
    }
    out
}

/// AES key wrap (RFC 3394), which `wrapKey` with `AES-KW` uses.
fn key_wrap(kek: &[u8], plain: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(kek)?;
    if !plain.len().is_multiple_of(8) || plain.len() < 16 {
        return None;
    }
    let n = plain.len() / 8;
    let mut a = [0xa6u8; 8];
    let mut r: Vec<[u8; 8]> = plain
        .chunks_exact(8)
        .map(|c| c.try_into().expect("eight bytes"))
        .collect();
    for j in 0..6 {
        for (i, half) in r.iter_mut().enumerate() {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(half);
            aes.encrypt(&mut b);
            let t = (n * j + i + 1) as u64;
            a.copy_from_slice(&b[..8]);
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            half.copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    for block in r {
        out.extend_from_slice(&block);
    }
    Some(out)
}

fn key_unwrap(kek: &[u8], wrapped: &[u8]) -> Option<Vec<u8>> {
    let aes = Aes::new(kek)?;
    if !wrapped.len().is_multiple_of(8) || wrapped.len() < 24 {
        return None;
    }
    let n = wrapped.len() / 8 - 1;
    let mut a: [u8; 8] = wrapped[..8].try_into().ok()?;
    let mut r: Vec<[u8; 8]> = wrapped[8..]
        .chunks_exact(8)
        .map(|c| c.try_into().expect("eight bytes"))
        .collect();
    for j in (0..6).rev() {
        for i in (0..n).rev() {
            let t = (n * j + i + 1) as u64;
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            aes.decrypt(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    if a != [0xa6; 8] {
        return None;
    }
    Some(r.concat())
}

// ─── The web's crypto.subtle ────────────────────────────────────────────────────────────────

/// A `DOMException` named `name` when the realm has one, an `Error` so named otherwise.
fn dom_error(context: &mut Context, name: &str, message: &str) -> JsError {
    if let Ok(class) = crate::native::global(context, "DOMException")
        && let Some(class) = class.as_object().filter(|c| c.is_constructor())
        && let Ok(error) = class.construct(&[js_text(message), js_text(name)], None, context)
    {
        return JsError::from_opaque(error.into());
    }
    let error = JsNativeError::error()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = error.set(js_string!("name"), js_text(name), false, context);
    JsError::from_opaque(error.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyAlg {
    AesGcm,
    AesCbc,
    AesCtr,
    AesKw,
    Hmac(HashAlg),
    Pbkdf2,
    Hkdf,
}

impl KeyAlg {
    fn name(self) -> &'static str {
        match self {
            KeyAlg::AesGcm => "AES-GCM",
            KeyAlg::AesCbc => "AES-CBC",
            KeyAlg::AesCtr => "AES-CTR",
            KeyAlg::AesKw => "AES-KW",
            KeyAlg::Hmac(_) => "HMAC",
            KeyAlg::Pbkdf2 => "PBKDF2",
            KeyAlg::Hkdf => "HKDF",
        }
    }

    fn is_aes(self) -> bool {
        matches!(
            self,
            KeyAlg::AesGcm | KeyAlg::AesCbc | KeyAlg::AesCtr | KeyAlg::AesKw
        )
    }
}

/// A `CryptoKey`'s secret, kept natively where script cannot read it.
#[derive(Trace, Finalize, JsData)]
struct KeyData {
    #[unsafe_ignore_trace]
    alg: KeyAlg,
    #[unsafe_ignore_trace]
    bytes: Vec<u8>,
    extractable: bool,
    #[unsafe_ignore_trace]
    usages: Vec<String>,
}

/// An algorithm as the web names it: a string, or an object with a `name`.
fn algorithm_name(algorithm: &JsValue, context: &mut Context) -> JsResult<String> {
    let name = match algorithm.as_object() {
        Some(o) if !algorithm.is_string() => o.get(js_string!("name"), context)?,
        _ => algorithm.clone(),
    };
    if name.is_undefined() {
        return Err(type_error("Algorithm: name is missing"));
    }
    Ok(text_of(&name, context)?.to_ascii_uppercase())
}

/// The `hash` member of an algorithm: `"SHA-256"` or `{ name: "SHA-256" }`.
fn hash_member(algorithm: &JsValue, context: &mut Context) -> JsResult<HashAlg> {
    let hash = get(algorithm, "hash", context)?;
    if hash.is_undefined() {
        return Err(type_error("Algorithm: hash is missing"));
    }
    let name = algorithm_name(&hash, context)?;
    match HashAlg::named(&name) {
        Some(alg) if alg != HashAlg::Md5 => Ok(alg),
        _ => Err(dom_error(
            context,
            "NotSupportedError",
            &format!("Unrecognized hash: {name}"),
        )),
    }
}

fn buffer_member(
    algorithm: &JsValue,
    key: &str,
    context: &mut Context,
) -> JsResult<Option<Vec<u8>>> {
    let value = get(algorithm, key, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    crate::encoding::bytes_of(&value, context)?
        .map(Some)
        .ok_or_else(|| type_error(format!("Algorithm: {key} is not a BufferSource")))
}

fn number_member(algorithm: &JsValue, key: &str, context: &mut Context) -> JsResult<Option<f64>> {
    let value = get(algorithm, key, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    Ok(Some(value.to_number(context)?))
}

fn not_supported(context: &mut Context, name: &str) -> JsError {
    dom_error(
        context,
        "NotSupportedError",
        &format!("Algorithm not supported: {name}"),
    )
}

/// The key algorithm a name and its parameters ask for, when it is one there are keys of here.
fn key_alg(algorithm: &JsValue, context: &mut Context) -> JsResult<KeyAlg> {
    let name = algorithm_name(algorithm, context)?;
    Ok(match name.as_str() {
        "AES-GCM" => KeyAlg::AesGcm,
        "AES-CBC" => KeyAlg::AesCbc,
        "AES-CTR" => KeyAlg::AesCtr,
        "AES-KW" => KeyAlg::AesKw,
        "HMAC" => KeyAlg::Hmac(hash_member(algorithm, context)?),
        "PBKDF2" => KeyAlg::Pbkdf2,
        "HKDF" => KeyAlg::Hkdf,
        other => return Err(not_supported(context, other)),
    })
}

fn usages_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    crate::native::list_of(value, context)?
        .iter()
        .map(|u| text_of(u, context))
        .collect()
}

/// A `CryptoKey`: its secret natively, and `type`, `extractable`, `algorithm` and `usages` for
/// script to read.
fn make_key(
    alg: KeyAlg,
    bytes: Vec<u8>,
    extractable: bool,
    usages: Vec<String>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let prototype = crate::native::global(context, "CryptoKey")
        .and_then(|class| get(&class, "prototype", context))
        .ok()
        .and_then(|p| p.as_object());
    let algorithm = object(context);
    algorithm.set(js_string!("name"), js_text(alg.name()), false, context)?;
    match alg {
        KeyAlg::Hmac(hash) => {
            let hash_object = object(context);
            hash_object.set(js_string!("name"), js_text(hash.web_name()), false, context)?;
            algorithm.set(js_string!("hash"), hash_object, false, context)?;
            algorithm.set(js_string!("length"), bytes.len() * 8, false, context)?;
        }
        alg if alg.is_aes() => {
            algorithm.set(js_string!("length"), bytes.len() * 8, false, context)?;
        }
        _ => {}
    }
    let usage_values: Vec<JsValue> = usages.iter().map(|u| js_text(u)).collect();
    let usage_list = array(context, usage_values);
    let key = JsObject::from_proto_and_data(
        prototype,
        KeyData {
            alg,
            bytes,
            extractable,
            usages,
        },
    );
    key.set(js_string!("type"), js_text("secret"), false, context)?;
    key.set(js_string!("extractable"), extractable, false, context)?;
    key.set(js_string!("algorithm"), algorithm, false, context)?;
    key.set(js_string!("usages"), usage_list, false, context)?;
    Ok(key.into())
}

/// The key `value` is, checked to be of `allowed` algorithms and to allow `usage`.
fn key_for(value: &JsValue, usage: &str, context: &mut Context) -> JsResult<(KeyAlg, Vec<u8>)> {
    let object = value
        .as_object()
        .ok_or_else(|| type_error("The key is not a CryptoKey"))?;
    let data = object
        .downcast_ref::<KeyData>()
        .ok_or_else(|| type_error("The key is not a CryptoKey"))?;
    if !data.usages.iter().any(|u| u == usage) {
        let message = format!("The key does not allow '{usage}'");
        drop(data);
        return Err(dom_error(context, "InvalidAccessError", &message));
    }
    Ok((data.alg, data.bytes.clone()))
}

/// Checks that the operation's algorithm is the key's own.
fn same_alg(algorithm: &str, key: KeyAlg, context: &mut Context) -> JsResult<()> {
    if algorithm != key.name() {
        return Err(dom_error(
            context,
            "InvalidAccessError",
            &format!("The key is for {}, not {algorithm}", key.name()),
        ));
    }
    Ok(())
}

fn check_aes_length(bytes: &[u8], context: &mut Context) -> JsResult<()> {
    if ![16, 24, 32].contains(&bytes.len()) {
        return Err(dom_error(
            context,
            "DataError",
            "AES keys must be 128, 192 or 256 bits long",
        ));
    }
    Ok(())
}

/// The JWK `alg` of a key: `A256GCM`, `HS256`.
fn jwk_alg(alg: KeyAlg, length: usize) -> Option<String> {
    let bits = length * 8;
    Some(match alg {
        KeyAlg::AesGcm => format!("A{bits}GCM"),
        KeyAlg::AesCbc => format!("A{bits}CBC"),
        KeyAlg::AesCtr => format!("A{bits}CTR"),
        KeyAlg::AesKw => format!("A{bits}KW"),
        KeyAlg::Hmac(HashAlg::Sha1) => "HS1".into(),
        KeyAlg::Hmac(HashAlg::Sha256) => "HS256".into(),
        KeyAlg::Hmac(HashAlg::Sha384) => "HS384".into(),
        KeyAlg::Hmac(HashAlg::Sha512) => "HS512".into(),
        _ => return None,
    })
}

/// `importKey(format, keyData, algorithm, extractable, usages)`, done now.
fn import_key(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = text_of(&arg(args, 0), context)?;
    let alg = key_alg(&arg(args, 2), context)?;
    let extractable = arg(args, 3).to_boolean();
    let usages = usages_of(&arg(args, 4), context)?;
    let bytes = match format.as_str() {
        "raw" => crate::encoding::bytes_of(&arg(args, 1), context)?
            .ok_or_else(|| type_error("importKey: the key data is not a BufferSource"))?,
        "jwk" => {
            let jwk = arg(args, 1);
            let kty = get(&jwk, "kty", context)?;
            if crate::print(&kty) != "oct" {
                return Err(dom_error(
                    context,
                    "DataError",
                    "importKey: only 'oct' JSON Web Keys are supported",
                ));
            }
            let k = text_of(&get(&jwk, "k", context)?, context)?;
            crate::encoding::unbase64(&k)
                .ok_or_else(|| dom_error(context, "DataError", "importKey: 'k' is not base64url"))?
        }
        other => {
            return Err(dom_error(
                context,
                "NotSupportedError",
                &format!("importKey: the '{other}' format is not supported"),
            ));
        }
    };
    if alg.is_aes() {
        check_aes_length(&bytes, context)?;
    }
    if matches!(alg, KeyAlg::Pbkdf2 | KeyAlg::Hkdf) && extractable {
        return Err(dom_error(
            context,
            "SyntaxError",
            "importKey: PBKDF2 and HKDF keys cannot be extractable",
        ));
    }
    make_key(alg, bytes, extractable, usages, context)
}

fn export_key(format: &str, key: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let object = key
        .as_object()
        .ok_or_else(|| type_error("exportKey: not a CryptoKey"))?;
    let (alg, bytes, extractable, usages) = {
        let data = object
            .downcast_ref::<KeyData>()
            .ok_or_else(|| type_error("exportKey: not a CryptoKey"))?;
        (
            data.alg,
            data.bytes.clone(),
            data.extractable,
            data.usages.clone(),
        )
    };
    if !extractable {
        return Err(dom_error(
            context,
            "InvalidAccessError",
            "exportKey: the key is not extractable",
        ));
    }
    match format {
        "raw" => array_buffer(bytes, context),
        "jwk" => {
            let jwk = object_with_jwk(alg, &bytes, &usages, context)?;
            Ok(jwk.into())
        }
        other => Err(dom_error(
            context,
            "NotSupportedError",
            &format!("exportKey: the '{other}' format is not supported"),
        )),
    }
}

fn object_with_jwk(
    alg: KeyAlg,
    bytes: &[u8],
    usages: &[String],
    context: &mut Context,
) -> JsResult<JsObject> {
    let jwk = object(context);
    jwk.set(js_string!("kty"), js_text("oct"), false, context)?;
    jwk.set(
        js_string!("k"),
        js_text(&crate::encoding::base64(bytes, true)),
        false,
        context,
    )?;
    if let Some(name) = jwk_alg(alg, bytes.len()) {
        jwk.set(js_string!("alg"), js_text(&name), false, context)?;
    }
    jwk.set(js_string!("ext"), true, false, context)?;
    let ops: Vec<JsValue> = usages.iter().map(|u| js_text(u)).collect();
    let ops = array(context, ops);
    jwk.set(js_string!("key_ops"), ops, false, context)?;
    Ok(jwk)
}

/// How long a key of `algorithm` is, in bytes, when made by `generateKey` or `deriveKey`.
fn key_length(algorithm: &JsValue, alg: KeyAlg, context: &mut Context) -> JsResult<usize> {
    let length = number_member(algorithm, "length", context)?;
    match alg {
        KeyAlg::Hmac(hash) => Ok(length.map_or(hash.block_size(), |bits| bits as usize / 8)),
        alg if alg.is_aes() => match length {
            Some(bits) if [128., 192., 256.].contains(&bits) => Ok(bits as usize / 8),
            _ => Err(dom_error(
                context,
                "OperationError",
                "AES key length must be 128, 192 or 256 bits",
            )),
        },
        other => Err(not_supported(context, other.name())),
    }
}

fn derive_bits(
    algorithm: &JsValue,
    key: &JsValue,
    bits: usize,
    usage: &str,
    context: &mut Context,
) -> JsResult<Vec<u8>> {
    let name = algorithm_name(algorithm, context)?;
    let (alg, secret) = key_for(key, usage, context)?;
    same_alg(&name, alg, context)?;
    if !bits.is_multiple_of(8) {
        return Err(dom_error(
            context,
            "OperationError",
            "deriveBits: the length must be a multiple of 8",
        ));
    }
    let hash = hash_member(algorithm, context)?;
    let salt = buffer_member(algorithm, "salt", context)?.unwrap_or_default();
    match alg {
        KeyAlg::Pbkdf2 => {
            let iterations = number_member(algorithm, "iterations", context)?.unwrap_or(0.);
            if iterations < 1. {
                return Err(dom_error(
                    context,
                    "OperationError",
                    "PBKDF2: iterations must be at least 1",
                ));
            }
            Ok(pbkdf2(hash, &secret, &salt, iterations as u32, bits / 8))
        }
        KeyAlg::Hkdf => {
            let info = buffer_member(algorithm, "info", context)?.unwrap_or_default();
            hkdf(hash, &secret, &salt, &info, bits / 8).ok_or_else(|| {
                dom_error(
                    context,
                    "OperationError",
                    "HKDF: the length asked for is too long",
                )
            })
        }
        other => Err(not_supported(context, other.name())),
    }
}

/// `encrypt` and `decrypt` with an AES key.
fn aes_crypt(
    algorithm: &JsValue,
    key: &JsValue,
    data: &[u8],
    encrypt: bool,
    context: &mut Context,
) -> JsResult<Vec<u8>> {
    let name = algorithm_name(algorithm, context)?;
    let (alg, secret) = key_for(key, if encrypt { "encrypt" } else { "decrypt" }, context)?;
    same_alg(&name, alg, context)?;
    let aes = Aes::new(&secret).ok_or_else(|| dom_error(context, "DataError", "Bad AES key"))?;
    match alg {
        KeyAlg::AesGcm => {
            let iv = buffer_member(algorithm, "iv", context)?
                .ok_or_else(|| type_error("AES-GCM: iv is missing"))?;
            if iv.is_empty() {
                return Err(dom_error(
                    context,
                    "OperationError",
                    "AES-GCM: the iv is empty",
                ));
            }
            let aad = buffer_member(algorithm, "additionalData", context)?.unwrap_or_default();
            let tag_bits = number_member(algorithm, "tagLength", context)?.unwrap_or(128.);
            if ![32., 64., 96., 104., 112., 120., 128.].contains(&tag_bits) {
                return Err(dom_error(
                    context,
                    "OperationError",
                    "AES-GCM: bad tagLength",
                ));
            }
            let tag = tag_bits as usize / 8;
            if encrypt {
                Ok(gcm_seal(&secret, &iv, &aad, data, tag).expect("an AES key"))
            } else {
                gcm_open(&secret, &iv, &aad, data, tag).ok_or_else(|| {
                    dom_error(
                        context,
                        "OperationError",
                        "The operation failed for an operation-specific reason",
                    )
                })
            }
        }
        KeyAlg::AesCbc => {
            let iv = buffer_member(algorithm, "iv", context)?
                .ok_or_else(|| type_error("AES-CBC: iv is missing"))?;
            let mut iv: Block = iv.try_into().map_err(|_| {
                dom_error(
                    context,
                    "OperationError",
                    "AES-CBC: the iv must be 16 bytes",
                )
            })?;
            if encrypt {
                let mut padded = data.to_vec();
                pad(&mut padded);
                Ok(cbc_blocks(&aes, Some(&mut iv), &padded, true))
            } else {
                if data.is_empty() || !data.len().is_multiple_of(16) {
                    return Err(dom_error(
                        context,
                        "OperationError",
                        "AES-CBC: the data is not whole blocks",
                    ));
                }
                unpad(cbc_blocks(&aes, Some(&mut iv), data, false))
                    .ok_or_else(|| dom_error(context, "OperationError", "AES-CBC: bad padding"))
            }
        }
        KeyAlg::AesCtr => {
            let counter = buffer_member(algorithm, "counter", context)?
                .ok_or_else(|| type_error("AES-CTR: counter is missing"))?;
            let counter: Block = counter.try_into().map_err(|_| {
                dom_error(
                    context,
                    "OperationError",
                    "AES-CTR: the counter must be 16 bytes",
                )
            })?;
            let bits = number_member(algorithm, "length", context)?.unwrap_or(0.);
            if !(1. ..=128.).contains(&bits) {
                return Err(dom_error(
                    context,
                    "OperationError",
                    "AES-CTR: length must be between 1 and 128",
                ));
            }
            let mut out = data.to_vec();
            Ctr::new(aes, counter, bits as u32, false).apply(&mut out);
            Ok(out)
        }
        other => Err(not_supported(context, other.name())),
    }
}

/// Each `crypto.subtle` method answers a promise; this one settles it with what `op` gives.
macro_rules! subtle_method {
    ($name:literal, $length:literal, $op:expr) => {
        ($name, $length, |_, args, cx| {
            let op: fn(&[JsValue], &mut Context) -> JsResult<JsValue> = $op;
            let outcome = op(args, cx);
            crate::native::settled(outcome, cx)
        })
    };
}

const SUBTLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    subtle_method!("digest", 2, |args, cx| {
        let name = algorithm_name(&arg(args, 0), cx)?;
        let alg = match HashAlg::named(&name) {
            Some(alg) if alg != HashAlg::Md5 => alg,
            _ => return Err(not_supported(cx, &name)),
        };
        let data = crate::encoding::bytes_of(&arg(args, 1), cx)?
            .ok_or_else(|| type_error("digest: the data is not an ArrayBuffer or a view of one"))?;
        array_buffer(alg.digest(&data), cx)
    }),
    subtle_method!("importKey", 5, import_key),
    subtle_method!("exportKey", 2, |args, cx| {
        let format = text_of(&arg(args, 0), cx)?;
        export_key(&format, &arg(args, 1), cx)
    }),
    subtle_method!("generateKey", 3, |args, cx| {
        let algorithm = arg(args, 0);
        let alg = key_alg(&algorithm, cx)?;
        let length = key_length(&algorithm, alg, cx)?;
        let usages = usages_of(&arg(args, 2), cx)?;
        make_key(
            alg,
            random_bytes(length),
            arg(args, 1).to_boolean(),
            usages,
            cx,
        )
    }),
    subtle_method!("deriveBits", 3, |args, cx| {
        let bits = arg(args, 2).to_number(cx)? as usize;
        let bytes = derive_bits(&arg(args, 0), &arg(args, 1), bits, "deriveBits", cx)?;
        array_buffer(bytes, cx)
    }),
    subtle_method!("deriveKey", 5, |args, cx| {
        let target = arg(args, 2);
        let alg = key_alg(&target, cx)?;
        let length = key_length(&target, alg, cx)?;
        let bytes = derive_bits(&arg(args, 0), &arg(args, 1), length * 8, "deriveKey", cx)?;
        let usages = usages_of(&arg(args, 4), cx)?;
        make_key(alg, bytes, arg(args, 3).to_boolean(), usages, cx)
    }),
    subtle_method!("encrypt", 3, |args, cx| {
        let data = crate::encoding::bytes_of(&arg(args, 2), cx)?
            .ok_or_else(|| type_error("encrypt: the data is not a BufferSource"))?;
        let out = aes_crypt(&arg(args, 0), &arg(args, 1), &data, true, cx)?;
        array_buffer(out, cx)
    }),
    subtle_method!("decrypt", 3, |args, cx| {
        let data = crate::encoding::bytes_of(&arg(args, 2), cx)?
            .ok_or_else(|| type_error("decrypt: the data is not a BufferSource"))?;
        let out = aes_crypt(&arg(args, 0), &arg(args, 1), &data, false, cx)?;
        array_buffer(out, cx)
    }),
    subtle_method!("sign", 3, |args, cx| {
        let name = algorithm_name(&arg(args, 0), cx)?;
        let (alg, secret) = key_for(&arg(args, 1), "sign", cx)?;
        same_alg(&name, alg, cx)?;
        let KeyAlg::Hmac(hash) = alg else {
            return Err(not_supported(cx, &name));
        };
        let data = crate::encoding::bytes_of(&arg(args, 2), cx)?
            .ok_or_else(|| type_error("sign: the data is not a BufferSource"))?;
        array_buffer(Mac::once(hash, &secret, &data), cx)
    }),
    subtle_method!("verify", 4, |args, cx| {
        let name = algorithm_name(&arg(args, 0), cx)?;
        let (alg, secret) = key_for(&arg(args, 1), "verify", cx)?;
        same_alg(&name, alg, cx)?;
        let KeyAlg::Hmac(hash) = alg else {
            return Err(not_supported(cx, &name));
        };
        let signature = crate::encoding::bytes_of(&arg(args, 2), cx)?
            .ok_or_else(|| type_error("verify: the signature is not a BufferSource"))?;
        let data = crate::encoding::bytes_of(&arg(args, 3), cx)?
            .ok_or_else(|| type_error("verify: the data is not a BufferSource"))?;
        Ok(same_bytes(&Mac::once(hash, &secret, &data), &signature).into())
    }),
    subtle_method!("wrapKey", 4, |args, cx| {
        let format = text_of(&arg(args, 0), cx)?;
        let exported = export_key(&format, &arg(args, 1), cx)?;
        let bytes = if format == "jwk" {
            let json = cx.intrinsics().objects().json();
            let stringify = json.get(js_string!("stringify"), cx)?;
            let text = crate::call(cx, &stringify, &json.into(), &[exported])?;
            text_of(&text, cx)?.into_bytes()
        } else {
            crate::encoding::bytes_of(&exported, cx)?.unwrap_or_default()
        };
        let algorithm = arg(args, 3);
        let name = algorithm_name(&algorithm, cx)?;
        let out = if name == "AES-KW" {
            let (alg, kek) = key_for(&arg(args, 2), "wrapKey", cx)?;
            same_alg(&name, alg, cx)?;
            key_wrap(&kek, &bytes).ok_or_else(|| {
                dom_error(
                    cx,
                    "OperationError",
                    "AES-KW: the key must be a multiple of 8 bytes, at least 16",
                )
            })?
        } else {
            wrap_with_cipher(&algorithm, &arg(args, 2), &bytes, true, cx)?
        };
        array_buffer(out, cx)
    }),
    subtle_method!("unwrapKey", 7, |args, cx| {
        let format = text_of(&arg(args, 0), cx)?;
        let wrapped = crate::encoding::bytes_of(&arg(args, 1), cx)?
            .ok_or_else(|| type_error("unwrapKey: the data is not a BufferSource"))?;
        let algorithm = arg(args, 3);
        let name = algorithm_name(&algorithm, cx)?;
        let bytes = if name == "AES-KW" {
            let (alg, kek) = key_for(&arg(args, 2), "unwrapKey", cx)?;
            same_alg(&name, alg, cx)?;
            key_unwrap(&kek, &wrapped).ok_or_else(|| {
                dom_error(
                    cx,
                    "OperationError",
                    "AES-KW: the wrapped key does not unwrap",
                )
            })?
        } else {
            wrap_with_cipher(&algorithm, &arg(args, 2), &wrapped, false, cx)?
        };
        let key_data: JsValue = if format == "jwk" {
            let json = cx.intrinsics().objects().json();
            let parse = json.get(js_string!("parse"), cx)?;
            crate::call(
                cx,
                &parse,
                &json.into(),
                &[js_text(&String::from_utf8_lossy(&bytes))],
            )?
        } else {
            JsUint8Array::from_iter(bytes, cx)?.into()
        };
        import_key(
            &[
                js_text(&format),
                key_data,
                arg(args, 4),
                arg(args, 5),
                arg(args, 6),
            ],
            cx,
        )
    }),
];

/// `wrapKey`/`unwrapKey` with an encryption algorithm (AES-GCM, AES-CBC, AES-CTR): the key's
/// bytes encrypted under the wrapping key, which must allow `wrapKey`/`unwrapKey`.
fn wrap_with_cipher(
    algorithm: &JsValue,
    key: &JsValue,
    data: &[u8],
    wrap: bool,
    context: &mut Context,
) -> JsResult<Vec<u8>> {
    let usage = if wrap { "wrapKey" } else { "unwrapKey" };
    let (alg, secret) = key_for(key, usage, context)?;
    // The same key, allowed to encrypt for this once.
    let usages = vec![if wrap {
        "encrypt".to_owned()
    } else {
        "decrypt".to_owned()
    }];
    let stand_in = make_key(alg, secret, false, usages, context)?;
    aes_crypt(algorithm, &stand_in, data, wrap, context)
}

/// The web's `crypto`: `getRandomValues`, `randomUUID`, `subtle`.
fn web_crypto(context: &mut Context) -> JsResult<JsObject> {
    let crypto = object(context);
    if let Ok(prototype) = class_prototype(context, "Crypto") {
        crypto.set_prototype(Some(prototype));
    }
    let get_random_values = function(context, "getRandomValues", 1, |_, args, cx| {
        // Fills the typed array it is given and answers it.
        let array = arg(args, 0);
        let length = get(&array, "byteLength", cx)?;
        if !array.is_object() || length.is_undefined() {
            return Err(type_error("getRandomValues: not a typed array"));
        }
        if length.to_number(cx)? > 65536. {
            return Err(dom_error(
                cx,
                "QuotaExceededError",
                "getRandomValues: more than 65536 bytes asked for",
            ));
        }
        fill_random(&array, None, None, cx)?;
        Ok(array)
    });
    hidden(
        &crypto,
        js_string!("getRandomValues"),
        get_random_values,
        context,
    )?;
    let uuid = function(context, "randomUUID", 0, |_, _, _| {
        Ok(js_text(&random_uuid()))
    });
    hidden(&crypto, js_string!("randomUUID"), uuid, context)?;
    let subtle = object(context);
    if let Ok(prototype) = class_prototype(context, "SubtleCrypto") {
        subtle.set_prototype(Some(prototype));
    }
    methods(context, &subtle, SUBTLE_METHODS)?;
    crypto.set(js_string!("subtle"), subtle, false, context)?;
    Ok(crypto)
}

fn class_prototype(context: &mut Context, name: &str) -> JsResult<JsObject> {
    let class = crate::native::global(context, name)?;
    get(&class, "prototype", context)?
        .as_object()
        .ok_or_else(|| type_error(format!("{name} is not a class")))
}

/// The web's classes that cannot be made from script — `Crypto`, `SubtleCrypto`, `CryptoKey` —
/// for `instanceof`.
fn illegal_class(context: &mut Context, name: &str) -> JsResult<()> {
    let (class, _) = crate::native::class(
        context,
        crate::native::ClassSpec {
            name,
            init: |_, _, _| Err(type_error("Illegal constructor")),
            ..Default::default()
        },
    )?;
    crate::native::define_global(context, name, class)
}

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    for name in ["Crypto", "SubtleCrypto", "CryptoKey"] {
        illegal_class(context, name)?;
    }
    let crypto = web_crypto(context)?;
    crate::native::define_global(context, "crypto", crypto)
}

// ─── Node's crypto: hashes, HMACs, ciphers ──────────────────────────────────────────────────

/// What a `Hash`, `Hmac`, `Cipher` or `Decipher` is, natively. Each is an `EventEmitter` too,
/// and has `write`/`end`/`read` as the stream Node's is would, gathering what it made for `read`.
#[derive(Trace, Finalize, JsData)]
struct NodeCrypto {
    #[unsafe_ignore_trace]
    state: RefCell<State>,
    #[unsafe_ignore_trace]
    output: RefCell<Vec<u8>>,
}

enum State {
    Hash(Box<dyn DynDigest>),
    Hmac(Mac),
    Cipher(Box<Cipher>),
    Done,
}

fn with_state<R>(this: &JsValue, f: impl FnOnce(&mut State, &mut Vec<u8>) -> R) -> JsResult<R> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("not a crypto object"))?;
    let data = object
        .downcast_ref::<NodeCrypto>()
        .ok_or_else(|| type_error("not a crypto object"))?;
    let mut state = data.state.borrow_mut();
    let mut output = data.output.borrow_mut();
    Ok(f(&mut state, &mut output))
}

fn finalized(context: &mut Context) -> JsError {
    coded_error(
        context,
        "Digest already called",
        "ERR_CRYPTO_HASH_FINALIZED",
    )
}

/// An object on `prototype` holding `state`.
fn node_object(prototype: &JsObject, state: State) -> JsValue {
    JsObject::from_proto_and_data(
        Some(prototype.clone()),
        NodeCrypto {
            state: RefCell::new(state),
            output: RefCell::new(Vec::new()),
        },
    )
    .into()
}

fn hash_update(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data = data_bytes(&arg(args, 0), &arg(args, 1), context)?;
    let ok = with_state(this, |state, _| match state {
        State::Hash(hasher) => {
            hasher.update(&data);
            true
        }
        State::Hmac(mac) => {
            mac.update(&data);
            true
        }
        _ => false,
    })?;
    if !ok {
        return Err(finalized(context));
    }
    Ok(this.clone())
}

fn hash_finish(this: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let state = with_state(this, |state, _| std::mem::replace(state, State::Done))?;
    match state {
        State::Hash(hasher) => Ok(hasher.finalize().into_vec()),
        State::Hmac(mac) => Ok(mac.finish()),
        _ => Err(finalized(context)),
    }
}

const HASH_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("update", 2, hash_update),
    ("digest", 1, |this, args, cx| {
        let bytes = hash_finish(this, cx)?;
        bytes_out(bytes, &arg(args, 0), cx)
    }),
    ("copy", 1, |this, _, cx| {
        let copy = with_state(this, |state, _| match state {
            State::Hash(hasher) => Some(State::Hash(hasher.box_clone())),
            State::Hmac(mac) => Some(State::Hmac(mac.copy())),
            _ => None,
        })?;
        let copy = copy.ok_or_else(|| finalized(cx))?;
        let prototype = this.as_object().and_then(|o| o.prototype());
        Ok(JsObject::from_proto_and_data(
            prototype,
            NodeCrypto {
                state: RefCell::new(copy),
                output: RefCell::new(Vec::new()),
            },
        )
        .into())
    }),
    ("write", 3, |this, args, cx| {
        hash_update(this, args, cx)?;
        callback_soon(args, cx);
        Ok(true.into())
    }),
    ("end", 3, |this, args, cx| {
        let first = arg(args, 0);
        if !first.is_undefined() && !first.is_callable() {
            hash_update(this, args, cx)?;
        }
        let bytes = hash_finish(this, cx)?;
        with_state(this, |_, output| output.extend(bytes))?;
        callback_soon(args, cx);
        Ok(this.clone())
    }),
    ("read", 1, read_output),
    ("setEncoding", 1, |this, _, _| Ok(this.clone())),
];

/// The callback given last to `write` or `end`, called once this turn is over.
fn callback_soon(args: &[JsValue], context: &mut Context) {
    if let Some(callback) = args.iter().rev().find(|a| a.is_callable()) {
        crate::microtask(context, callback.clone(), Vec::new());
    }
}

fn read_output(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = with_state(this, |_, output| std::mem::take(output))?;
    if bytes.is_empty() {
        return Ok(JsValue::null());
    }
    crate::encoding::make_buffer(context, bytes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Gcm,
    Cbc,
    Ctr,
    Ecb,
}

/// A cipher as Node runs it: fed a piece at a time, answering what it can at once.
struct Cipher {
    mode: Mode,
    encrypt: bool,
    aes: Aes,
    padding: bool,
    /// CBC's chaining block.
    chain: Block,
    /// Bytes waiting for a whole block (CBC, ECB).
    pending: Vec<u8>,
    ctr: Option<Ctr>,
    gcm: Option<Gcm>,
    tag_len: usize,
    /// GCM: the tag made (encrypting) or to be checked (decrypting).
    tag: Option<Vec<u8>>,
}

/// `aes-256-gcm` and the rest: the key length in bytes and the mode.
fn cipher_named(name: &str) -> Option<(usize, Mode)> {
    let lower = name.to_ascii_lowercase();
    let lower = match lower.as_str() {
        "aes128" => "aes-128-cbc",
        "aes192" => "aes-192-cbc",
        "aes256" => "aes-256-cbc",
        "id-aes128-gcm" => "aes-128-gcm",
        "id-aes192-gcm" => "aes-192-gcm",
        "id-aes256-gcm" => "aes-256-gcm",
        other => other,
    };
    let mut parts = lower.split('-');
    if parts.next()? != "aes" {
        return None;
    }
    let size = match parts.next()? {
        "128" => 16,
        "192" => 24,
        "256" => 32,
        _ => return None,
    };
    let mode = match parts.next()? {
        "gcm" => Mode::Gcm,
        "cbc" => Mode::Cbc,
        "ctr" => Mode::Ctr,
        "ecb" => Mode::Ecb,
        _ => return None,
    };
    parts.next().is_none().then_some((size, mode))
}

const CIPHERS: &[&str] = &[
    "aes-128-cbc",
    "aes-128-ctr",
    "aes-128-ecb",
    "aes-128-gcm",
    "aes-192-cbc",
    "aes-192-ctr",
    "aes-192-ecb",
    "aes-192-gcm",
    "aes-256-cbc",
    "aes-256-ctr",
    "aes-256-ecb",
    "aes-256-gcm",
    "aes128",
    "aes192",
    "aes256",
    "id-aes128-gcm",
    "id-aes192-gcm",
    "id-aes256-gcm",
];

const HASHES: &[&str] = &[
    "RSA-MD5",
    "RSA-SHA1",
    "RSA-SHA224",
    "RSA-SHA256",
    "RSA-SHA384",
    "RSA-SHA512",
    "md5",
    "sha1",
    "sha224",
    "sha256",
    "sha384",
    "sha512",
    "sha512-224",
    "sha512-256",
];

impl Cipher {
    fn update(&mut self, data: &[u8]) -> Vec<u8> {
        match self.mode {
            Mode::Gcm => self.gcm.as_mut().expect("a GCM cipher").update(data),
            Mode::Ctr => {
                let mut out = data.to_vec();
                self.ctr.as_mut().expect("a CTR cipher").apply(&mut out);
                out
            }
            Mode::Cbc | Mode::Ecb => {
                self.pending.extend_from_slice(data);
                let mut whole = self.pending.len() / 16 * 16;
                // Decrypting with padding, the last block waits for `final`: it holds the padding.
                if !self.encrypt && self.padding && whole == self.pending.len() && whole > 0 {
                    whole -= 16;
                }
                let blocks: Vec<u8> = self.pending.drain(..whole).collect();
                let chain = (self.mode == Mode::Cbc).then_some(&mut self.chain);
                cbc_blocks(&self.aes, chain, &blocks, self.encrypt)
            }
        }
    }

    fn finish(&mut self) -> Result<Vec<u8>, &'static str> {
        match self.mode {
            Mode::Gcm => {
                let gcm = self.gcm.take().ok_or("Unsupported state")?;
                let tag = gcm.tag();
                if self.encrypt {
                    self.tag = Some(tag[..self.tag_len].to_vec());
                    Ok(Vec::new())
                } else {
                    match &self.tag {
                        Some(expected) if same_bytes(&tag[..expected.len()], expected) => {
                            Ok(Vec::new())
                        }
                        _ => Err("Unsupported state or unable to authenticate data"),
                    }
                }
            }
            Mode::Ctr => Ok(Vec::new()),
            Mode::Cbc | Mode::Ecb => {
                let mut rest = std::mem::take(&mut self.pending);
                if self.encrypt {
                    if self.padding {
                        pad(&mut rest);
                    } else if !rest.len().is_multiple_of(16) {
                        return Err("wrong final block length");
                    }
                } else if !rest.len().is_multiple_of(16) || (self.padding && rest.is_empty()) {
                    return Err("wrong final block length");
                }
                let chain = (self.mode == Mode::Cbc).then_some(&mut self.chain);
                let out = cbc_blocks(&self.aes, chain, &rest, self.encrypt);
                if !self.encrypt && self.padding {
                    return unpad(out).ok_or("bad decrypt");
                }
                Ok(out)
            }
        }
    }
}

fn key_bytes(value: &JsValue, encoding: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if let Some(object) = value.as_object()
        && let Some(data) = object.downcast_ref::<KeyData>()
    {
        return Ok(data.bytes.clone());
    }
    data_bytes(value, encoding, context)
}

fn create_cipher(
    args: &[JsValue],
    encrypt: bool,
    prototype: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = text_of(&arg(args, 0), context)?;
    let (size, mode) = cipher_named(&name)
        .ok_or_else(|| coded_error(context, "Invalid cipher type", "ERR_CRYPTO_UNKNOWN_CIPHER"))?;
    let key = key_bytes(&arg(args, 1), &JsValue::undefined(), context)?;
    if key.len() != size {
        return Err(coded_error(
            context,
            "Invalid key length",
            "ERR_CRYPTO_INVALID_KEYLEN",
        ));
    }
    let iv_value = arg(args, 2);
    let iv = if iv_value.is_null_or_undefined() {
        Vec::new()
    } else {
        data_bytes(&iv_value, &JsValue::undefined(), context)?
    };
    let iv_ok = match mode {
        Mode::Ecb => true,
        Mode::Gcm => !iv.is_empty(),
        Mode::Cbc | Mode::Ctr => iv.len() == 16,
    };
    if !iv_ok {
        return Err(coded_error(
            context,
            "Invalid initialization vector",
            "ERR_CRYPTO_INVALID_IV",
        ));
    }
    let tag_len = match number_member(&arg(args, 3), "authTagLength", context)? {
        Some(n) if (4. ..=16.).contains(&n) => n as usize,
        Some(_) => {
            return Err(coded_error(
                context,
                "Invalid authentication tag length",
                "ERR_CRYPTO_INVALID_AUTH_TAG",
            ));
        }
        None => 16,
    };
    let aes = Aes::new(&key).expect("a checked key length");
    let chain: Block = iv
        .get(..16)
        .and_then(|b| b.try_into().ok())
        .unwrap_or([0; 16]);
    let cipher = Cipher {
        mode,
        encrypt,
        padding: true,
        chain,
        pending: Vec::new(),
        ctr: (mode == Mode::Ctr).then(|| Ctr::new(aes.clone(), chain, 128, false)),
        gcm: (mode == Mode::Gcm).then(|| Gcm::new(aes.clone(), &iv, encrypt)),
        aes,
        tag_len,
        tag: None,
    };
    Ok(node_object(prototype, State::Cipher(Box::new(cipher))))
}

fn with_cipher<R>(
    this: &JsValue,
    context: &mut Context,
    f: impl FnOnce(&mut Cipher) -> R,
) -> JsResult<R> {
    let outcome = with_state(this, |state, _| match state {
        State::Cipher(cipher) => Some(f(cipher)),
        _ => None,
    })?;
    outcome.ok_or_else(|| coded_error(context, "Unsupported state", "ERR_CRYPTO_INVALID_STATE"))
}

fn cipher_update(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data = data_bytes(&arg(args, 0), &arg(args, 1), context)?;
    let out = with_cipher(this, context, |c| c.update(&data))?;
    bytes_out(out, &arg(args, 2), context)
}

fn cipher_final(this: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let outcome = with_cipher(this, context, |c| c.finish())?;
    let out = outcome.map_err(|message| {
        let code = if message == "bad decrypt" {
            "ERR_OSSL_BAD_DECRYPT"
        } else {
            "ERR_CRYPTO_INVALID_STATE"
        };
        coded_error(context, message, code)
    })?;
    // Kept for `getAuthTag`; nothing more can be fed it.
    Ok(out)
}

const CIPHER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("update", 3, cipher_update),
    ("final", 1, |this, args, cx| {
        let out = cipher_final(this, cx)?;
        bytes_out(out, &arg(args, 0), cx)
    }),
    ("setAAD", 2, |this, args, cx| {
        let aad = data_bytes(&arg(args, 0), &JsValue::undefined(), cx)?;
        with_cipher(this, cx, |c| {
            if let Some(gcm) = c.gcm.as_mut() {
                gcm.aad.extend_from_slice(&aad);
            }
        })?;
        Ok(this.clone())
    }),
    ("getAuthTag", 0, |this, _, cx| {
        let tag = with_cipher(this, cx, |c| if c.encrypt { c.tag.clone() } else { None })?;
        match tag {
            Some(tag) => crate::encoding::make_buffer(cx, tag),
            None => Err(coded_error(
                cx,
                "Invalid state for operation getAuthTag",
                "ERR_CRYPTO_INVALID_STATE",
            )),
        }
    }),
    ("setAuthTag", 2, |this, args, cx| {
        let tag = data_bytes(&arg(args, 0), &arg(args, 1), cx)?;
        if tag.is_empty() || tag.len() > 16 {
            return Err(coded_error(
                cx,
                "Invalid authentication tag length",
                "ERR_CRYPTO_INVALID_AUTH_TAG",
            ));
        }
        with_cipher(this, cx, |c| c.tag = Some(tag))?;
        Ok(this.clone())
    }),
    ("setAutoPadding", 1, |this, args, cx| {
        let on = args.first().is_none_or(JsValue::to_boolean);
        with_cipher(this, cx, |c| c.padding = on)?;
        Ok(this.clone())
    }),
    ("write", 3, |this, args, cx| {
        let data = data_bytes(&arg(args, 0), &arg(args, 1), cx)?;
        let out = with_cipher(this, cx, |c| c.update(&data))?;
        with_state(this, |_, output| output.extend(out))?;
        callback_soon(args, cx);
        Ok(true.into())
    }),
    ("end", 3, |this, args, cx| {
        let first = arg(args, 0);
        if !first.is_undefined() && !first.is_callable() {
            let data = data_bytes(&first, &arg(args, 1), cx)?;
            let out = with_cipher(this, cx, |c| c.update(&data))?;
            with_state(this, |_, output| output.extend(out))?;
        }
        match cipher_final(this, cx) {
            Ok(out) => {
                with_state(this, |_, output| output.extend(out))?;
            }
            // As a stream, the failure is an `error` event.
            Err(error) => {
                let error = error.into_opaque(cx)?;
                crate::native::invoke(this, "emit", &[js_text("error"), error], cx)?;
            }
        }
        callback_soon(args, cx);
        Ok(this.clone())
    }),
    ("read", 1, read_output),
    ("setEncoding", 1, |this, _, _| Ok(this.clone())),
];

// ─── Node's crypto: keys ────────────────────────────────────────────────────────────────────

/// A secret `KeyObject`'s bytes.
#[derive(Trace, Finalize, JsData)]
struct SecretKey {
    #[unsafe_ignore_trace]
    bytes: Vec<u8>,
}

fn secret_of(value: &JsValue) -> Option<Vec<u8>> {
    let object = value.as_object()?;
    let data = object.downcast_ref::<SecretKey>()?;
    Some(data.bytes.clone())
}

fn make_secret(bytes: Vec<u8>, context: &mut Context) -> JsResult<JsValue> {
    let prototype = crate::modules::node_module(context, "crypto")
        .and_then(|crypto| get(&crypto, "KeyObject", context))
        .and_then(|class| get(&class, "prototype", context))
        .ok()
        .and_then(|p| p.as_object());
    let size = bytes.len();
    let key = JsObject::from_proto_and_data(prototype, SecretKey { bytes });
    key.set(js_string!("type"), js_text("secret"), false, context)?;
    key.set(js_string!("symmetricKeySize"), size, false, context)?;
    Ok(key.into())
}

const KEY_OBJECT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("export", 1, |this, args, cx| {
        let bytes = secret_of(this).ok_or_else(|| type_error("not a KeyObject"))?;
        let format = get(&arg(args, 0), "format", cx)?;
        if crate::print(&format) == "jwk" {
            let jwk = object(cx);
            jwk.set(js_string!("kty"), js_text("oct"), false, cx)?;
            jwk.set(
                js_string!("k"),
                js_text(&crate::encoding::base64(&bytes, true)),
                false,
                cx,
            )?;
            return Ok(jwk.into());
        }
        crate::encoding::make_buffer(cx, bytes)
    }),
    ("equals", 1, |this, args, _| {
        let a = secret_of(this).unwrap_or_default();
        let b = secret_of(&arg(args, 0));
        Ok(b.is_some_and(|b| same_bytes(&a, &b)).into())
    }),
];

// ─── Node's crypto: the module ──────────────────────────────────────────────────────────────

fn hash_alg_arg(value: &JsValue, context: &mut Context) -> JsResult<HashAlg> {
    let name = text_of(value, context)?;
    HashAlg::named(&name).ok_or_else(|| {
        coded_error(
            context,
            &format!("Digest method not supported: {name}"),
            "ERR_CRYPTO_INVALID_DIGEST",
        )
    })
}

fn length_arg(value: &JsValue, name: &str, context: &mut Context) -> JsResult<usize> {
    let n = value.to_number(context)?;
    if !n.is_finite() || n < 0. || n.fract() != 0. || n > 2_147_483_647. {
        return Err(crate::native::range_error(format!(
            "The value of \"{name}\" is out of range. Received {n}"
        )));
    }
    Ok(n as usize)
}

/// Calls `callback(error, value)` or `callback(null, value)` once this turn is over: Node's
/// asynchronous forms, done synchronously and answered later.
fn answer_later(
    callback: &JsValue,
    outcome: JsResult<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let args = match outcome {
        Ok(value) => vec![JsValue::null(), value],
        Err(error) => vec![error.into_opaque(context)?],
    };
    crate::microtask(context, callback.clone(), args);
    Ok(JsValue::undefined())
}

fn unsupported(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Err(coded_error(
        context,
        "Public-key cryptography (RSA, elliptic curves, Diffie-Hellman) is not available in this runtime",
        "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
    ))
}

fn random_int(args: &[JsValue], context: &mut Context) -> JsResult<f64> {
    let numbers: Vec<&JsValue> = args.iter().filter(|a| a.is_number()).collect();
    let (min, max) = match numbers.as_slice() {
        [max] => (0., max.to_number(context)?),
        [min, max, ..] => (min.to_number(context)?, max.to_number(context)?),
        [] => {
            return Err(coded_error(
                context,
                "The \"max\" argument must be a safe integer",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
    };
    if !(min.fract() == 0. && max.fract() == 0.) || max <= min || max - min > 281_474_976_710_655. {
        return Err(crate::native::range_error(format!(
            "The value of \"max\" is out of range. It must be greater than the value of \"min\" ({min}). Received {max}"
        )));
    }
    let range = (max - min) as u64;
    // Rejection sampling, so every value is as likely.
    let limit = u64::MAX - u64::MAX % range;
    loop {
        let bytes: [u8; 8] = random_bytes(8).try_into().expect("eight bytes");
        let n = u64::from_le_bytes(bytes);
        if n < limit {
            return Ok(min + (n % range) as f64);
        }
    }
}

fn scrypt_args(args: &[JsValue], context: &mut Context) -> JsResult<Vec<u8>> {
    let password = data_bytes(&arg(args, 0), &JsValue::undefined(), context)?;
    let salt = data_bytes(&arg(args, 1), &JsValue::undefined(), context)?;
    let length = length_arg(&arg(args, 2), "keylen", context)?;
    let options = arg(args, 3);
    let option = |keys: [&str; 2], default: f64, context: &mut Context| -> JsResult<f64> {
        for key in keys {
            if let Some(n) = number_member(&options, key, context)? {
                return Ok(n);
            }
        }
        Ok(default)
    };
    let n = option(["N", "cost"], 16384., context)?;
    let r = option(["r", "blockSize"], 8., context)?;
    let p = option(["p", "parallelization"], 1., context)?;
    let maxmem = option(["maxmem", "maxmem"], 32. * 1024. * 1024., context)?;
    if n < 2. || (n as u64).count_ones() != 1 {
        return Err(coded_error(
            context,
            "Invalid scrypt params: N must be a power of 2 greater than 1",
            "ERR_CRYPTO_INVALID_SCRYPT_PARAMS",
        ));
    }
    if 128. * n * r > maxmem {
        return Err(coded_error(
            context,
            "Invalid scrypt params: memory limit exceeded",
            "ERR_CRYPTO_INVALID_SCRYPT_PARAMS",
        ));
    }
    let log_n = (n as u64).trailing_zeros() as u8;
    let params = scrypt::Params::new(log_n, r as u32, p as u32, length.max(10)).map_err(|e| {
        coded_error(
            context,
            &format!("Invalid scrypt params: {e}"),
            "ERR_CRYPTO_INVALID_SCRYPT_PARAMS",
        )
    })?;
    let mut out = vec![0u8; length];
    scrypt::scrypt(&password, &salt, &params, &mut out).map_err(|e| {
        coded_error(
            context,
            &format!("scrypt: {e}"),
            "ERR_CRYPTO_INVALID_SCRYPT_PARAMS",
        )
    })?;
    Ok(out)
}

fn pbkdf2_args(args: &[JsValue], context: &mut Context) -> JsResult<Vec<u8>> {
    let password = data_bytes(&arg(args, 0), &JsValue::undefined(), context)?;
    let salt = data_bytes(&arg(args, 1), &JsValue::undefined(), context)?;
    let iterations = length_arg(&arg(args, 2), "iterations", context)?;
    if iterations == 0 {
        return Err(crate::native::range_error(
            "The value of \"iterations\" is out of range. It must be >= 1",
        ));
    }
    let length = length_arg(&arg(args, 3), "keylen", context)?;
    let digest = arg(args, 4);
    if !digest.is_string() {
        return Err(coded_error(
            context,
            "The \"digest\" argument must be of type string",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let alg = hash_alg_arg(&digest, context)?;
    Ok(pbkdf2(alg, &password, &salt, iterations as u32, length))
}

fn hkdf_args(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let alg = hash_alg_arg(&arg(args, 0), context)?;
    let key = key_bytes(&arg(args, 1), &JsValue::undefined(), context)?;
    let salt = data_bytes(&arg(args, 2), &JsValue::undefined(), context)?;
    let info = data_bytes(&arg(args, 3), &JsValue::undefined(), context)?;
    let length = length_arg(&arg(args, 4), "keylen", context)?;
    let out = hkdf(alg, &key, &salt, &info, length)
        .ok_or_else(|| coded_error(context, "Invalid key length", "ERR_CRYPTO_INVALID_KEYLEN"))?;
    array_buffer(out, context)
}

const CRYPTO_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("hash", 3, |_, args, cx| {
        let alg = hash_alg_arg(&arg(args, 0), cx)?;
        let data = data_bytes(&arg(args, 1), &JsValue::undefined(), cx)?;
        let encoding = match arg(args, 2) {
            e if e.is_undefined() => js_text("hex"),
            e => e,
        };
        bytes_out(alg.digest(&data), &encoding, cx)
    }),
    ("randomBytes", 2, |_, args, cx| {
        let size = length_arg(&arg(args, 0), "size", cx)?;
        let buffer = crate::encoding::make_buffer(cx, random_bytes(size));
        match args.get(1).filter(|c| c.is_callable()) {
            Some(callback) => answer_later(callback, buffer, cx),
            None => buffer,
        }
    }),
    ("randomFillSync", 3, |_, args, cx| {
        let offset = args
            .get(1)
            .filter(|v| !v.is_undefined())
            .map(|v| length_arg(v, "offset", cx))
            .transpose()?;
        let size = args
            .get(2)
            .filter(|v| !v.is_undefined())
            .map(|v| length_arg(v, "size", cx))
            .transpose()?;
        fill_random(&arg(args, 0), offset, size, cx)?;
        Ok(arg(args, 0))
    }),
    ("randomFill", 4, |_, args, cx| {
        let callback = args
            .iter()
            .rev()
            .find(|a| a.is_callable())
            .cloned()
            .ok_or_else(|| type_error("The \"callback\" argument must be of type function"))?;
        let numbers: Vec<JsValue> = args[1..]
            .iter()
            .filter(|a| a.is_number())
            .cloned()
            .collect();
        let offset = numbers
            .first()
            .map(|v| length_arg(v, "offset", cx))
            .transpose()?;
        let size = numbers
            .get(1)
            .map(|v| length_arg(v, "size", cx))
            .transpose()?;
        let outcome = fill_random(&arg(args, 0), offset, size, cx).map(|_| arg(args, 0));
        answer_later(&callback, outcome, cx)
    }),
    ("randomUUID", 1, |_, _, _| Ok(js_text(&random_uuid()))),
    ("randomInt", 3, |_, args, cx| {
        let outcome = random_int(args, cx).map(JsValue::from);
        match args.iter().rev().find(|a| a.is_callable()) {
            Some(callback) => answer_later(&callback.clone(), outcome, cx),
            None => outcome,
        }
    }),
    ("getRandomValues", 1, |_, args, cx| {
        fill_random(&arg(args, 0), None, None, cx)?;
        Ok(arg(args, 0))
    }),
    ("timingSafeEqual", 2, |_, args, cx| {
        let a = crate::encoding::bytes_of(&arg(args, 0), cx)?.ok_or_else(|| type_error("The \"buf1\" argument must be an instance of ArrayBuffer, Buffer, TypedArray, or DataView"))?;
        let b = crate::encoding::bytes_of(&arg(args, 1), cx)?.ok_or_else(|| type_error("The \"buf2\" argument must be an instance of ArrayBuffer, Buffer, TypedArray, or DataView"))?;
        if a.len() != b.len() {
            let error = crate::native::range_error("Input buffers must have the same byte length")
                .into_opaque(cx)?;
            if let Some(object) = error.as_object() {
                object.set(
                    js_string!("code"),
                    js_text("ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH"),
                    false,
                    cx,
                )?;
            }
            return Err(JsError::from_opaque(error));
        }
        Ok(same_bytes(&a, &b).into())
    }),
    ("pbkdf2Sync", 5, |_, args, cx| {
        let out = pbkdf2_args(args, cx)?;
        crate::encoding::make_buffer(cx, out)
    }),
    ("pbkdf2", 6, |_, args, cx| {
        let callback = arg(args, 5);
        if !callback.is_callable() {
            return Err(coded_error(
                cx,
                "The \"callback\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        let outcome = pbkdf2_args(args, cx).and_then(|out| crate::encoding::make_buffer(cx, out));
        answer_later(&callback, outcome, cx)
    }),
    ("scryptSync", 4, |_, args, cx| {
        let out = scrypt_args(args, cx)?;
        crate::encoding::make_buffer(cx, out)
    }),
    ("scrypt", 5, |_, args, cx| {
        let callback = args
            .iter()
            .rev()
            .find(|a| a.is_callable())
            .cloned()
            .ok_or_else(|| type_error("The \"callback\" argument must be of type function"))?;
        let given: Vec<JsValue> = args.iter().filter(|a| !a.is_callable()).cloned().collect();
        let outcome = scrypt_args(&given, cx).and_then(|out| crate::encoding::make_buffer(cx, out));
        answer_later(&callback, outcome, cx)
    }),
    ("hkdfSync", 5, |_, args, cx| hkdf_args(args, cx)),
    ("hkdf", 6, |_, args, cx| {
        let callback = arg(args, 5);
        if !callback.is_callable() {
            return Err(coded_error(
                cx,
                "The \"callback\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        let outcome = hkdf_args(args, cx);
        answer_later(&callback, outcome, cx)
    }),
    ("getHashes", 0, |_, _, cx| {
        let names: Vec<JsValue> = HASHES.iter().map(|n| js_text(n)).collect();
        Ok(array(cx, names))
    }),
    ("getCiphers", 0, |_, _, cx| {
        let names: Vec<JsValue> = CIPHERS.iter().map(|n| js_text(n)).collect();
        Ok(array(cx, names))
    }),
    ("getCurves", 0, |_, _, cx| Ok(JsArray::new(cx)?.into())),
    ("getFips", 0, |_, _, _| Ok(0.into())),
    ("setFips", 1, |_, _, _| Ok(JsValue::undefined())),
    ("setEngine", 2, |_, _, _| Ok(JsValue::undefined())),
    ("secureHeapUsed", 0, |_, _, cx| Ok(object(cx).into())),
    ("createSecretKey", 2, |_, args, cx| {
        let bytes = data_bytes(&arg(args, 0), &arg(args, 1), cx)?;
        make_secret(bytes, cx)
    }),
    ("generateKeySync", 2, |_, args, cx| {
        let kind = text_of(&arg(args, 0), cx)?;
        let length = number_member(&arg(args, 1), "length", cx)?.unwrap_or(0.) as usize;
        if !matches!(kind.as_str(), "hmac" | "aes") || length == 0 || !length.is_multiple_of(8) {
            return Err(coded_error(
                cx,
                "generateKeySync: only 'hmac' and 'aes' keys of a whole number of bytes",
                "ERR_INVALID_ARG_VALUE",
            ));
        }
        make_secret(random_bytes(length / 8), cx)
    }),
    ("generateKey", 3, |_, args, cx| {
        let callback = arg(args, 2);
        let kind = text_of(&arg(args, 0), cx)?;
        let length = number_member(&arg(args, 1), "length", cx)?.unwrap_or(0.) as usize;
        let outcome = if matches!(kind.as_str(), "hmac" | "aes") && length > 0 && length.is_multiple_of(8) {
            make_secret(random_bytes(length / 8), cx)
        } else {
            Err(coded_error(
                cx,
                "generateKey: only 'hmac' and 'aes' keys of a whole number of bytes",
                "ERR_INVALID_ARG_VALUE",
            ))
        };
        answer_later(&callback, outcome, cx)
    }),
    ("createPublicKey", 1, unsupported),
    ("createPrivateKey", 1, unsupported),
    ("createSign", 2, unsupported),
    ("createVerify", 2, unsupported),
    ("sign", 4, unsupported),
    ("verify", 5, unsupported),
    ("generateKeyPair", 3, unsupported),
    ("generateKeyPairSync", 2, unsupported),
    ("publicEncrypt", 2, unsupported),
    ("privateDecrypt", 2, unsupported),
    ("privateEncrypt", 2, unsupported),
    ("publicDecrypt", 2, unsupported),
    ("createECDH", 1, unsupported),
    ("createDiffieHellman", 2, unsupported),
    ("createDiffieHellmanGroup", 1, unsupported),
    ("getDiffieHellman", 1, unsupported),
    ("diffieHellman", 1, unsupported),
    ("generatePrime", 3, unsupported),
    ("generatePrimeSync", 2, unsupported),
    ("checkPrime", 3, unsupported),
    ("checkPrimeSync", 2, unsupported),
];

/// A prototype inheriting `EventEmitter.prototype`, with `list` on it.
fn emitter_prototype(
    context: &mut Context,
    list: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<JsObject> {
    let prototype = object(context);
    let emitter = crate::modules::node_module(context, "events")?;
    if let Some(parent) = get(&emitter, "prototype", context)?.as_object() {
        prototype.set_prototype(Some(parent));
    }
    methods(context, &prototype, list)?;
    Ok(prototype)
}

/// A function of the module that makes objects on `prototype` (`createHash`), and the class
/// Node exports for them (`Hash`) — which makes them too.
fn maker(
    context: &mut Context,
    module: &JsObject,
    names: (&str, &str),
    prototype: &JsObject,
    make: fn(&JsValue, &[JsValue], &JsObject, &mut Context) -> JsResult<JsValue>,
) -> JsResult<()> {
    let function = closure(context, names.0, 2, make, prototype.clone());
    module.set(JsString::from(names.0), function, false, context)?;
    let class: JsObject = boa_engine::object::FunctionObjectBuilder::new(
        context.realm(),
        boa_engine::NativeFunction::from_copy_closure_with_captures(
            move |_, args, prototype: &JsObject, context| {
                make(&JsValue::undefined(), args, prototype, context)
            },
            prototype.clone(),
        ),
    )
    .name(JsString::from(names.1))
    .length(2)
    .constructor(true)
    .build()
    .into();
    class.set(js_string!("prototype"), prototype.clone(), false, context)?;
    hidden(prototype, js_string!("constructor"), class.clone(), context)?;
    module.set(JsString::from(names.1), class, false, context)?;
    Ok(())
}

/// `require('crypto')`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    crate::native::exports(context, &module, CRYPTO_FUNCTIONS)?;

    let hash = emitter_prototype(context, HASH_METHODS)?;
    maker(
        context,
        &module,
        ("createHash", "Hash"),
        &hash,
        |_, args, prototype, cx| {
            let alg = hash_alg_arg(&arg(args, 0), cx)?;
            Ok(node_object(prototype, State::Hash(alg.hasher())))
        },
    )?;
    let hmac = emitter_prototype(context, HASH_METHODS)?;
    maker(
        context,
        &module,
        ("createHmac", "Hmac"),
        &hmac,
        |_, args, prototype, cx| {
            let alg = hash_alg_arg(&arg(args, 0), cx)?;
            let key = key_bytes(&arg(args, 1), &JsValue::undefined(), cx)?;
            Ok(node_object(prototype, State::Hmac(Mac::new(alg, &key))))
        },
    )?;
    let cipher = emitter_prototype(context, CIPHER_METHODS)?;
    maker(
        context,
        &module,
        ("createCipheriv", "Cipheriv"),
        &cipher,
        |_, args, prototype, cx| create_cipher(args, true, prototype, cx),
    )?;
    let decipher = emitter_prototype(context, CIPHER_METHODS)?;
    maker(
        context,
        &module,
        ("createDecipheriv", "Decipheriv"),
        &decipher,
        |_, args, prototype, cx| create_cipher(args, false, prototype, cx),
    )?;

    let (key_object, _) = crate::native::class(
        context,
        crate::native::ClassSpec {
            name: "KeyObject",
            init: |_, _, _| Err(type_error("Illegal constructor")),
            methods: KEY_OBJECT_METHODS,
            statics: &[("from", 1, |_, args, cx| {
                let key = arg(args, 0);
                let bytes = key
                    .as_object()
                    .and_then(|o| o.downcast_ref::<KeyData>().map(|d| d.bytes.clone()))
                    .ok_or_else(|| {
                        type_error("The \"key\" argument must be an instance of CryptoKey")
                    })?;
                make_secret(bytes, cx)
            })],
            ..Default::default()
        },
    )?;
    module.set(js_string!("KeyObject"), key_object, false, context)?;

    let web = crate::native::global(context, "crypto")?;
    module.set(js_string!("webcrypto"), web.clone(), false, context)?;
    let subtle = get(&web, "subtle", context)?;
    module.set(js_string!("subtle"), subtle, false, context)?;
    let constants = object(context);
    for (name, value) in [
        ("OPENSSL_VERSION_NUMBER", 0x3000_0000_u32),
        ("SSL_OP_ALL", 0x8000_0850),
        ("SSL_OP_NO_SSLv2", 0),
        ("SSL_OP_NO_SSLv3", 0x0200_0000),
        ("SSL_OP_NO_TLSv1", 0x0400_0000),
        ("RSA_PKCS1_PADDING", 1),
        ("RSA_NO_PADDING", 3),
        ("RSA_PKCS1_OAEP_PADDING", 4),
        ("RSA_PKCS1_PSS_PADDING", 6),
        ("RSA_PSS_SALTLEN_DIGEST", 0xffff_ffff),
        ("POINT_CONVERSION_COMPRESSED", 2),
        ("POINT_CONVERSION_UNCOMPRESSED", 4),
    ] {
        constants.set(JsString::from(name), value, false, context)?;
    }
    module.set(js_string!("constants"), constants, false, context)?;
    Ok(module.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_wrap_within_their_bits() {
        let mut c = [0u8; 16];
        c[15] = 0xff;
        increment(&mut c, 8);
        assert_eq!(c[15], 0);
        assert_eq!(c[14], 0);
        c[15] = 0xff;
        increment(&mut c, 16);
        assert_eq!((c[14], c[15]), (1, 0));
        let mut c = [0xffu8; 16];
        increment(&mut c, 128);
        assert_eq!(c, [0; 16]);
    }

    #[test]
    fn key_wrap_matches_rfc_3394() {
        let kek = from_hex(
            "000102030405060708090A0B0C0D0E0F"
                .to_ascii_lowercase()
                .as_str(),
        );
        let key = from_hex("00112233445566778899aabbccddeeff");
        let wrapped = key_wrap(&kek, &key).unwrap();
        assert_eq!(
            to_hex(&wrapped),
            "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5"
        );
        assert_eq!(key_unwrap(&kek, &wrapped).unwrap(), key);
    }

    #[test]
    fn gcm_matches_the_specs_vectors() {
        // Test case 4 of the GCM specification (McGrew & Viega), AES-128.
        let key = from_hex("feffe9928665731c6d6a8f9467308308");
        let iv = from_hex("cafebabefacedbaddecaf888");
        let plain = from_hex(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        );
        let aad = from_hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let sealed = gcm_seal(&key, &iv, &aad, &plain, 16).unwrap();
        assert_eq!(
            to_hex(&sealed),
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e0915bc94fbc3221a5db94fae95ae7121a47"
        );
        assert_eq!(gcm_open(&key, &iv, &aad, &sealed, 16).unwrap(), plain);
        // Test case 6: a 60-byte IV, hashed to its counter block.
        let iv = from_hex(
            "9313225df88406e555909c5aff5269aa6a7a9538534f7da1e4c303d2a318a728c3c0c95156809539fcf0e2429a6b525416aedbf5a0de6a57a637b39b",
        );
        let sealed = gcm_seal(&key, &iv, &aad, &plain, 16).unwrap();
        assert_eq!(
            to_hex(&sealed[sealed.len() - 16..]),
            "619cc5aefffe0bfa462af43c1699d050"
        );
    }
}
