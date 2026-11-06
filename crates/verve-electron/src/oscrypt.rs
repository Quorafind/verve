//! Chromium's `OSCrypt` ciphertext as Electron 39's `safeStorage` makes it on Windows, so that what
//! an app encrypted under Electron still opens under Verve: `"v10"`, a 12-byte nonce, the AES-256-GCM
//! ciphertext and its 16-byte tag. The key lives in the profile's `Local State` JSON, as
//! `os_crypt.encrypted_key`: base64 of `"DPAPI"` and the key wrapped by the user's DPAPI.
//!
//! Only the format is here; wrapping the key is the system's ([`crate::os`]).

use verve::crypto_primitives::{aes_gcm_open, aes_gcm_seal};

const VERSION: &[u8] = b"v10";
const NONCE: usize = 12;
const TAG: usize = 16;
const KEY_PREFIX: &[u8] = b"DPAPI";

/// Why a ciphertext did not open.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// It does not start with `v10`: it was never made by `OSCrypt`.
    NotEncrypted,
    /// It is too short, or the key or tag is not its own.
    Failed,
}

/// `plain` sealed under `key` with `nonce`.
pub(crate) fn seal(key: &[u8; 32], nonce: &[u8; NONCE], plain: &[u8]) -> Vec<u8> {
    let sealed = aes_gcm_seal(key, nonce, plain).expect("a 32-byte key and a 12-byte nonce seal");
    [VERSION, nonce, &sealed].concat()
}

/// What `data` holds, sealed by [`seal`] or by Chromium.
pub(crate) fn open(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, OpenError> {
    let Some(rest) = data.strip_prefix(VERSION) else {
        return Err(OpenError::NotEncrypted);
    };
    if rest.len() < NONCE + TAG {
        return Err(OpenError::Failed);
    }
    let (nonce, sealed) = rest.split_at(NONCE);
    aes_gcm_open(key, nonce, sealed).ok_or(OpenError::Failed)
}

/// The wrapped key in `Local State`'s text, when it has one.
pub(crate) fn wrapped_key(local_state: &str) -> Option<Vec<u8>> {
    let state: serde_json::Value = serde_json::from_str(local_state).ok()?;
    let text = state.pointer("/os_crypt/encrypted_key")?.as_str()?;
    let bytes = verve::encoding::unbase64(text)?;
    bytes.strip_prefix(KEY_PREFIX).map(<[u8]>::to_vec)
}

/// `Local State` with `wrapped` as the key, every other field of `local_state` kept.
pub(crate) fn with_wrapped_key(local_state: Option<&str>, wrapped: &[u8]) -> String {
    let parsed = local_state.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
    let mut state = parsed
        .filter(serde_json::Value::is_object)
        .unwrap_or_default();
    let key = verve::encoding::base64(&[KEY_PREFIX, wrapped].concat(), false);
    let os_crypt = &mut state["os_crypt"];
    if !os_crypt.is_object() {
        *os_crypt = serde_json::json!({});
    }
    os_crypt["encrypted_key"] = serde_json::Value::String(key);
    state.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn a_sealed_text_is_v10_nonce_ciphertext_and_tag() {
        let sealed = seal(&KEY, &[1; 12], b"hello");
        assert_eq!(&sealed[..3], b"v10");
        assert_eq!(&sealed[3..15], &[1; 12]);
        assert_eq!(sealed.len(), 3 + 12 + 5 + 16);
        assert_eq!(open(&KEY, &sealed).unwrap(), b"hello");
    }

    #[test]
    fn what_does_not_open_says_why() {
        assert_eq!(open(&KEY, b"plain text"), Err(OpenError::NotEncrypted));
        assert_eq!(open(&KEY, b"v10abc"), Err(OpenError::Failed));
        let mut sealed = seal(&KEY, &[1; 12], b"hello");
        *sealed.last_mut().unwrap() ^= 1;
        assert_eq!(open(&KEY, &sealed), Err(OpenError::Failed));
        assert_eq!(
            open(&[8; 32], &seal(&KEY, &[1; 12], b"x")),
            Err(OpenError::Failed)
        );
    }

    #[test]
    fn the_key_in_local_state_is_dpapi_prefixed_and_other_fields_stay() {
        let before = r#"{"browser":{"enabled":true},"os_crypt":{"audit_enabled":true}}"#;
        let after = with_wrapped_key(Some(before), b"blob");
        assert_eq!(wrapped_key(&after).unwrap(), b"blob");
        let state: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(state["browser"]["enabled"], true);
        assert_eq!(state["os_crypt"]["audit_enabled"], true);
        assert_eq!(state["os_crypt"]["encrypted_key"], "RFBBUElibG9i");
        assert!(wrapped_key("not json").is_none());
        assert!(wrapped_key(&with_wrapped_key(None, b"x")).is_some());
    }
}
