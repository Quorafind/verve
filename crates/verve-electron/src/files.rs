//! Reading a file as the app sees it: through its virtual filesystem (so a path inside
//! `app.asar` reads), then from the disk. A page's `file:` loads and `net.fetch` of a `file:` URL
//! share it, and share its failures: Chromium's net errors.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use verve::browser_net::NetError;
use verve::vfs::Overlay;

use crate::msg::{ERR_ACCESS_DENIED, ERR_FAILED, ERR_FILE_NOT_FOUND};

/// The bytes of the file at `path`, or the net error Chromium reports: `ERR_FILE_NOT_FOUND`,
/// `ERR_ACCESS_DENIED`, `ERR_FAILED` (a folder, or anything else).
pub(crate) fn read(overlay: Option<&Arc<dyn Overlay>>, path: &Path) -> Result<Vec<u8>, NetError> {
    let packed = overlay.and_then(|overlay| overlay.read(path));
    let read = match packed {
        Some(read) => read.map(|bytes| bytes.into_owned()),
        None if path.is_dir() => return Err(NetError::from_code(ERR_FAILED)),
        None => std::fs::read(path),
    };
    read.map_err(|error| {
        NetError::from_code(match error.kind() {
            ErrorKind::NotFound => ERR_FILE_NOT_FOUND,
            ErrorKind::PermissionDenied => ERR_ACCESS_DENIED,
            _ => ERR_FAILED,
        })
    })
}
