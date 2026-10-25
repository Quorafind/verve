//! Subresource Integrity, as npm records a tarball's hash: `sha512-<base64>`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

/// The digest algorithms npm writes, weakest first: the strongest one present is checked.
const ALGORITHMS: [&str; 4] = ["sha1", "sha256", "sha384", "sha512"];

fn digest(algorithm: &str, bytes: &[u8]) -> Vec<u8> {
    match algorithm {
        "sha1" => Sha1::digest(bytes).to_vec(),
        "sha256" => Sha256::digest(bytes).to_vec(),
        "sha384" => Sha384::digest(bytes).to_vec(),
        _ => Sha512::digest(bytes).to_vec(),
    }
}

/// `sha512-…` of `bytes`: what is recorded for a tarball that came with no hash of its own.
pub fn sha512(bytes: &[u8]) -> String {
    format!("sha512-{}", STANDARD.encode(Sha512::digest(bytes)))
}

/// The hex SHA-512 of `bytes`: the name a tarball has in the content-addressed cache.
pub fn sha512_hex(bytes: &[u8]) -> String {
    hex(&Sha512::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The strongest hash of an SRI string, as `(algorithm, digest)`.
fn strongest(sri: &str) -> Option<(&'static str, Vec<u8>)> {
    sri.split_whitespace()
        .filter_map(|token| {
            let (algorithm, rest) = token.split_once('-')?;
            let algorithm = ALGORITHMS.iter().copied().find(|a| *a == algorithm)?;
            // `?opts` may follow the digest; nothing npm writes uses them.
            let encoded = rest.split('?').next()?;
            let bytes = STANDARD.decode(encoded).ok()?;
            Some((algorithm, bytes))
        })
        .max_by_key(|(algorithm, _)| ALGORITHMS.iter().position(|a| a == algorithm))
}

/// The hex SHA-512 an SRI string names, when it names one: the tarball's key in the cache.
pub fn sha512_key(sri: &str) -> Option<String> {
    match strongest(sri)? {
        ("sha512", bytes) => Some(hex(&bytes)),
        _ => None,
    }
}

/// What a hash check found wrong: the hash expected and the one the bytes have.
#[derive(Debug)]
pub struct Mismatch {
    pub expected: String,
    pub actual: String,
}

/// Checks `bytes` against the registry's `dist.integrity` (else the older hex `dist.shasum`)
/// and answers the integrity to record: the one given, or `sha512` of the bytes when the
/// registry gave only a SHA-1 or nothing at all.
pub fn verify(
    bytes: &[u8],
    integrity: Option<&str>,
    shasum: Option<&str>,
) -> Result<String, Mismatch> {
    if let Some(sri) = integrity.filter(|sri| !sri.trim().is_empty()) {
        let Some((algorithm, expected)) = strongest(sri) else {
            return Err(Mismatch {
                expected: sri.to_owned(),
                actual: "no hash in a supported algorithm (sha1, sha256, sha384, sha512)".into(),
            });
        };
        let actual = digest(algorithm, bytes);
        if actual != expected {
            return Err(Mismatch {
                expected: format!("{algorithm}-{}", STANDARD.encode(&expected)),
                actual: format!("{algorithm}-{}", STANDARD.encode(&actual)),
            });
        }
        return Ok(if algorithm == "sha512" {
            sri.trim().to_owned()
        } else {
            sha512(bytes)
        });
    }
    if let Some(shasum) = shasum.filter(|s| !s.is_empty()) {
        let actual = hex(&Sha1::digest(bytes));
        if !actual.eq_ignore_ascii_case(shasum) {
            return Err(Mismatch {
                expected: format!("sha1 {shasum}"),
                actual: format!("sha1 {actual}"),
            });
        }
    }
    Ok(sha512(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strongest_hash_is_checked() {
        let bytes = b"tarball";
        let good = sha512(bytes);
        assert_eq!(verify(bytes, Some(&good), None).unwrap(), good);
        let with_weak = format!("sha1-AAAA {good}");
        assert!(
            verify(bytes, Some(&with_weak), None).is_ok(),
            "sha512 wins over a bad sha1"
        );
        assert!(verify(b"other", Some(&good), None).is_err());
        let sha1_hex = hex(&Sha1::digest(bytes));
        assert_eq!(verify(bytes, None, Some(&sha1_hex)).unwrap(), good);
        assert!(verify(bytes, None, Some("00")).is_err());
        assert!(verify(bytes, Some("md5-xyz"), None).is_err());
        assert_eq!(sha512_key(&good), Some(sha512_hex(bytes)));
    }
}
