//! `safeStorage`: strings encrypted so that only this user, on this machine, reads them, in the
//! format Electron's `OSCrypt` writes ([`crate::oscrypt`]) so the ciphertexts of an app moved from
//! Electron still open. The key is the profile's: `userData/Local State` holds it wrapped by the
//! system, and is made on first use. Windows only so far; elsewhere the module says encryption is
//! not available, as Electron does when it has no key store.

use std::cell::RefCell;

use boa_engine::{Context, JsResult, JsValue};
use verve::encoding::bytes_of;
use verve::native::{self, arg};

use super::define::{self, Args};

use super::shared;
use crate::oscrypt::{self, OpenError};

/// The profile's key once it has been read or made.
#[derive(Default)]
struct Key(RefCell<Option<[u8; 32]>>);

/// The `safeStorage` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("isEncryptionAvailable", 0, is_available),
            ("encryptString", 1, encrypt_string),
            ("decryptString", 1, decrypt_string),
            ("setUsePlainTextEncryption", 1, |_, a, cx| {
                Args::new(a).bool_or(0, "plain", false, cx)?;
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    if cfg!(target_os = "linux") {
        native::exports(
            context,
            &module,
            &[("getSelectedStorageBackend", 0, backend)],
        )?;
    }
    Ok(module.into())
}

fn backend(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(native::js_text("basic_text"))
}

/// The profile's key, read from `Local State` or made. `None` before `ready`, or where the system
/// has no store to wrap it in.
fn key(context: &mut Context) -> Option<[u8; 32]> {
    let state = shared(context);
    if !state.app.is_ready() {
        return None;
    }
    if let Some(key) = context.get_data::<Key>().and_then(|k| *k.0.borrow()) {
        return Some(key);
    }
    let path = state.app.paths.borrow().user_data().join("Local State");
    let text = std::fs::read_to_string(&path).ok();
    let known = text.as_deref().and_then(oscrypt::wrapped_key);
    let key = known
        .and_then(|wrapped| crate::os::dpapi_unprotect(&wrapped))
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .or_else(|| make_key(&path, text.as_deref()))?;
    context.insert_data(Key(RefCell::new(Some(key))));
    Some(key)
}

/// A new key, wrapped and written to `Local State` beside the fields already in it.
fn make_key(path: &std::path::Path, text: Option<&str>) -> Option<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).ok()?;
    let wrapped = crate::os::dpapi_protect(&key)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    // Written beside and renamed over, so a crash leaves the old file, never half of a new one;
    // synced first, so the rename cannot outrun the data.
    let beside = path.with_extension("tmp");
    let written = std::fs::File::create(&beside).and_then(|mut file| {
        std::io::Write::write_all(
            &mut file,
            oscrypt::with_wrapped_key(text, &wrapped).as_bytes(),
        )?;
        file.sync_all()
    });
    if written.is_err() || std::fs::rename(&beside, path).is_err() {
        let _ = std::fs::remove_file(&beside);
        return None;
    }
    Some(key)
}

fn is_available(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(key(context).is_some().into())
}

fn not_ready() -> boa_engine::JsError {
    define::error("safeStorage cannot be used before app is ready")
}

fn encrypt_string(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !shared(context).app.is_ready() {
        return Err(not_ready());
    }
    let plain = Args::new(args).string(0, "plainText", context)?;
    let Some(key) = key(context) else {
        return Err(define::error(
            "Error while encrypting the text provided to safeStorage.encryptString. Encryption is not available.",
        ));
    };
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|error| define::error(&error.to_string()))?;
    define::buffer_of(context, oscrypt::seal(&key, &nonce, plain.as_bytes()))
}

fn decrypt_string(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !shared(context).app.is_ready() {
        return Err(not_ready());
    }
    let given = arg(args, 0);
    let bytes = if given.is_string() {
        None
    } else {
        bytes_of(&given, context)?
    };
    let Some(bytes) = bytes else {
        return Err(define::error(
            "Expected the first argument of decryptString() to be a buffer",
        ));
    };
    let failure = |detail: &str| {
        define::error(&format!(
            "Error while decrypting the ciphertext provided to safeStorage.decryptString.{detail}"
        ))
    };
    let Some(key) = key(context) else {
        return Err(failure(" Decryption is not available."));
    };
    match oscrypt::open(&key, &bytes) {
        Ok(plain) => Ok(native::js_text(&String::from_utf8_lossy(&plain))),
        Err(OpenError::NotEncrypted) => {
            Err(failure(" Ciphertext does not appear to be encrypted."))
        }
        Err(OpenError::Failed) => Err(failure("")),
    }
}
