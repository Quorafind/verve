//! The integrity record an archive keeps for a packed file: the SHA-256 of the whole file, as
//! `@electron/asar` writes it and Electron's `EnableEmbeddedAsarIntegrityValidation` fuse checks
//! it on the `fs` read path (`ValidateIntegrityOrDie` in `shell/common/asar/asar_util.cc`). The
//! per-block digests beside it are Chromium's streaming validator's; reading a whole file does
//! not need them. Only packed files are checked; Electron does not check unpacked ones.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use verve::encoding::hex;

/// A file's digest, as the header has it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Integrity {
    algorithm: String,
    hash: String,
}

impl Integrity {
    /// Whether `bytes`, the whole file, hash to what the header says.
    pub(crate) fn accepts(&self, bytes: &[u8]) -> bool {
        self.algorithm.eq_ignore_ascii_case("SHA256")
            && self.hash.eq_ignore_ascii_case(&hex(&Sha256::digest(bytes)))
    }
}
