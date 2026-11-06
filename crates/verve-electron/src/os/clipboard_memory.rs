//! A clipboard that lives in the process, for platforms without a native backend yet.

use std::sync::{Mutex, MutexGuard};

use super::clipboard::Clip;
use crate::pixels::NativeImage;

static CLIPBOARD: Mutex<Vec<Clip>> = Mutex::new(Vec::new());

fn held() -> MutexGuard<'static, Vec<Clip>> {
    CLIPBOARD.lock().unwrap_or_else(|p| p.into_inner())
}

pub(super) fn write(items: &[Clip]) -> bool {
    *held() = items.to_vec();
    true
}

pub(super) fn text() -> String {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Text(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn html() -> String {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Html(html) => Some(html.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn rtf() -> String {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Rtf(rtf) => Some(rtf.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn image() -> NativeImage {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Image(image) => Some(image.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn bookmark() -> String {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Bookmark(url) => Some(url.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn buffer(format: &str) -> Vec<u8> {
    held()
        .iter()
        .find_map(|clip| match clip {
            Clip::Buffer {
                format: name,
                bytes,
            } if name == format => Some(bytes.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn formats() -> Vec<String> {
    let mime = |clip: &Clip| match clip {
        Clip::Text(_) => Some("text/plain"),
        Clip::Html(_) => Some("text/html"),
        Clip::Rtf(_) => Some("text/rtf"),
        Clip::Image(_) => Some("image/png"),
        _ => None,
    };
    held().iter().filter_map(mime).map(str::to_owned).collect()
}

pub(super) fn has(format: &str) -> bool {
    held()
        .iter()
        .any(|clip| matches!(clip, Clip::Buffer { format: name, .. } if name == format))
}
