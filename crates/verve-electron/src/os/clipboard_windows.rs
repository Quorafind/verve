//! The Windows clipboard, written and read as Chromium does so that other programs and Electron
//! apps see the same formats: UTF-16 text, `HTML Format` (CF_HTML with its offsets header),
//! `Rich Text Format`, `PNG`, and `UniformResourceLocatorW` for a bookmark's URL. The clipboard
//! may be held by another program for a moment, so opening it is retried.

use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};

use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::clipboard::Clip;
use crate::api::app::platform::wide;
use crate::pixels::NativeImage;

const CF_TEXT: u32 = 1;
const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;
const OPEN_TRIES: u32 = 20;

/// The clipboard, open; closed when dropped. A window the process owns it through: with no owner,
/// `EmptyClipboard` leaves `SetClipboardData` failing.
struct Open(HWND);

impl Open {
    fn new() -> Option<Self> {
        let class = wide("STATIC");
        // SAFETY: a message-only window of a system class, destroyed when this value is dropped.
        let owner = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if owner.is_null() {
            return None;
        }
        for _ in 0..OPEN_TRIES {
            // SAFETY: `owner` is a live window or null.
            if unsafe { OpenClipboard(owner) } != 0 {
                return Some(Open(owner));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: the window made above.
        unsafe { DestroyWindow(owner) };
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this value exists only while the clipboard is open and its owner lives.
        unsafe {
            CloseClipboard();
            DestroyWindow(self.0);
        }
    }
}

/// The id Windows gives the format called `name`.
fn registered(name: &str) -> u32 {
    // SAFETY: the name is NUL-terminated.
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

/// The bytes of format `format`, while the clipboard is open.
fn data(_open: &Open, format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the handle is the clipboard's, locked only while copied.
    unsafe {
        let handle: HANDLE = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle as HGLOBAL);
        let at = GlobalLock(handle as HGLOBAL).cast::<u8>();
        if at.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(at, size).to_vec();
        GlobalUnlock(handle as HGLOBAL);
        Some(bytes)
    }
}

/// Puts `bytes` on the clipboard as `format`, while it is open and emptied.
fn put(_open: &Open, format: u32, bytes: &[u8]) -> bool {
    // SAFETY: the block is the system's after `SetClipboardData` takes it, and freed if it does not.
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if block.is_null() {
            return false;
        }
        let at = GlobalLock(block).cast::<u8>();
        if at.is_null() {
            GlobalFree(block);
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
        GlobalUnlock(block);
        if SetClipboardData(format, block as HANDLE).is_null() {
            GlobalFree(block);
            return false;
        }
        true
    }
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn utf8_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// `fragment` as CF_HTML: the header names, as byte offsets, where the document and the fragment
/// start and end.
pub(super) fn cf_html(fragment: &str) -> String {
    const HEAD: &str = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let (open, close) = (
        "<html><body>\r\n<!--StartFragment-->",
        "<!--EndFragment-->\r\n</body></html>",
    );
    let start_html = HEAD.len();
    let start_fragment = start_html + open.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + close.len();
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{open}{fragment}{close}"
    )
}

/// The fragment of a CF_HTML text, by its offsets or else by its comment markers.
pub(super) fn html_fragment(text: &str) -> String {
    let offset = |name: &str| {
        let at = text.find(name)? + name.len();
        text[at..].lines().next()?.trim().parse::<usize>().ok()
    };
    if let (Some(start), Some(end)) = (offset("StartFragment:"), offset("EndFragment:"))
        && let Some(fragment) = text.get(start..end)
    {
        return fragment.to_owned();
    }
    let marker = |tag: &str| text.find(tag).map(|at| at + tag.len());
    match (
        marker("<!--StartFragment-->"),
        text.find("<!--EndFragment-->"),
    ) {
        (Some(start), Some(end)) if start <= end => text[start..end].to_owned(),
        _ => text.to_owned(),
    }
}

pub(super) fn write(items: &[Clip]) -> bool {
    let Some(open) = Open::new() else {
        return false;
    };
    // SAFETY: the clipboard is open.
    if unsafe { EmptyClipboard() } == 0 {
        return false;
    }
    items.iter().all(|item| put_item(&open, item))
}

fn put_item(open: &Open, item: &Clip) -> bool {
    match item {
        Clip::Text(text) => put(open, CF_UNICODETEXT, &utf16_bytes(text)),
        Clip::Html(html) => put(
            open,
            registered("HTML Format"),
            &nul_terminated(&cf_html(html)),
        ),
        Clip::Rtf(rtf) => put(open, registered("Rich Text Format"), &nul_terminated(rtf)),
        Clip::Image(image) => {
            let png = image.to_png(1.0);
            png.is_empty() || put(open, registered("PNG"), &png)
        }
        Clip::Bookmark(url) => put(
            open,
            registered("UniformResourceLocatorW"),
            &utf16_bytes(url),
        ),
        Clip::Buffer { format, bytes } => put(open, registered(format), bytes),
    }
}

pub(super) fn text() -> String {
    Open::new()
        .and_then(|open| data(&open, CF_UNICODETEXT))
        .map_or_else(String::new, |bytes| utf16_text(&bytes))
}

pub(super) fn html() -> String {
    let bytes = Open::new().and_then(|open| data(&open, registered("HTML Format")));
    bytes.map_or_else(String::new, |bytes| html_fragment(&utf8_text(&bytes)))
}

pub(super) fn rtf() -> String {
    let bytes = Open::new().and_then(|open| data(&open, registered("Rich Text Format")));
    bytes.map_or_else(String::new, |bytes| utf8_text(&bytes))
}

pub(super) fn image() -> NativeImage {
    let Some(open) = Open::new() else {
        return NativeImage::empty();
    };
    // A PNG is what Electron and browsers put there; a screenshot has only a device-independent
    // bitmap. The bytes are copied out and the clipboard let go before they are decoded.
    let png = data(&open, registered("PNG"));
    let bmp = if png.is_none() {
        data(&open, CF_DIB).and_then(|dib| bmp_of_dib(&dib))
    } else {
        None
    };
    drop(open);
    match png.or(bmp) {
        Some(bytes) => NativeImage::from_encoded(&bytes, 1.0),
        None => NativeImage::empty(),
    }
}

/// A `.bmp` file for a `CF_DIB` block: the block with the 14-byte file header it lacks. The sizes
/// in the block are the clipboard's to claim, so none is trusted: `None` for anything that does
/// not add up.
fn bmp_of_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let word = |at: usize| {
        let end = at.checked_add(4)?;
        let bytes: [u8; 4] = dib.get(at..end)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    };
    let header = word(0)?;
    if header < 40 || header as usize > dib.len() {
        return None;
    }
    let bits = u32::from(u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?));
    let compression = word(16)?;
    let used = word(32)?;
    // The colour table, then for a 40-byte header with bit fields the masks that follow it:
    // three for `BI_BITFIELDS`, four for `BI_ALPHABITFIELDS`.
    let colours = match bits {
        1..=8 if used == 0 => 1u32 << bits,
        _ => used,
    };
    let table = colours.checked_mul(4)?;
    let masks = match (header, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let offset = 14u32
        .checked_add(header)?
        .checked_add(table)?
        .checked_add(masks)?;
    let total = u32::try_from(dib.len()).ok()?.checked_add(14)?;
    if offset > total {
        return None;
    }
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&total.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

pub(super) fn bookmark() -> String {
    let bytes = Open::new().and_then(|open| data(&open, registered("UniformResourceLocatorW")));
    bytes.map_or_else(String::new, |bytes| utf16_text(&bytes))
}

pub(super) fn buffer(format: &str) -> Vec<u8> {
    Open::new()
        .and_then(|open| data(&open, registered(format)))
        .unwrap_or_default()
}

pub(super) fn has(format: &str) -> bool {
    // SAFETY: the call needs no open clipboard.
    unsafe { IsClipboardFormatAvailable(registered(format)) != 0 }
}

pub(super) fn formats() -> Vec<String> {
    let Some(_open) = Open::new() else {
        return Vec::new();
    };
    let present = |format: u32| {
        // SAFETY: the clipboard is open.
        unsafe { IsClipboardFormatAvailable(format) != 0 }
    };
    let kinds = [
        ("text/plain", vec![CF_UNICODETEXT, CF_TEXT]),
        ("text/html", vec![registered("HTML Format")]),
        ("text/rtf", vec![registered("Rich Text Format")]),
        ("image/png", vec![registered("PNG"), CF_DIB, CF_DIBV5]),
    ];
    let listed = kinds
        .into_iter()
        .filter(|(_, candidates)| candidates.iter().any(|format| present(*format)));
    listed.map(|(mime, _)| mime.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dib_becomes_a_bmp_the_image_codec_reads() {
        // 2x1, 24 bits, bottom-up rows padded to 4 bytes: blue then green.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        let bmp = bmp_of_dib(&dib).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        let image = NativeImage::from_encoded(&bmp, 1.0);
        assert_eq!(image.size(), (2, 1));
        assert!(bmp_of_dib(&[0; 10]).is_none());
    }

    #[test]
    fn a_dib_that_claims_more_than_it_holds_is_refused() {
        let mut dib = vec![0u8; 40];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        // A colour count whose table overflows 32 bits, and one that merely outruns the block.
        dib[32..36].copy_from_slice(&0x4000_0000u32.to_le_bytes());
        assert!(bmp_of_dib(&dib).is_none());
        dib[32..36].copy_from_slice(&1000u32.to_le_bytes());
        assert!(bmp_of_dib(&dib).is_none());
        // A header longer than the block.
        dib[..4].copy_from_slice(&124u32.to_le_bytes());
        assert!(bmp_of_dib(&dib).is_none());
    }

    #[test]
    fn alpha_bit_fields_after_a_40_byte_header_are_four_masks() {
        let mut dib = vec![0u8; 40 + 16 + 4];
        dib[..4].copy_from_slice(&40u32.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&6u32.to_le_bytes());
        let bmp = bmp_of_dib(&dib).unwrap();
        assert_eq!(
            u32::from_le_bytes(bmp[10..14].try_into().unwrap()),
            14 + 40 + 16
        );
        dib[16..20].copy_from_slice(&3u32.to_le_bytes());
        let bmp = bmp_of_dib(&dib).unwrap();
        assert_eq!(
            u32::from_le_bytes(bmp[10..14].try_into().unwrap()),
            14 + 40 + 12
        );
    }

    #[test]
    fn cf_html_names_its_fragment_by_byte_offset() {
        let text = cf_html("<b>héllo</b>");
        assert_eq!(html_fragment(&text), "<b>héllo</b>");
        let start: usize = text.split("StartFragment:").nth(1).unwrap()[..10]
            .parse()
            .unwrap();
        assert_eq!(&text[start..start + 3], "<b>");
    }

    #[test]
    fn a_fragment_is_found_by_its_markers_when_the_offsets_are_wrong() {
        let text = "Version:0.9\r\nStartFragment:9998\r\nEndFragment:9999\r\n<!--StartFragment-->x<!--EndFragment-->";
        assert_eq!(html_fragment(text), "x");
    }

    #[test]
    fn utf16_text_round_trips_and_stops_at_the_terminator() {
        assert_eq!(utf16_text(&utf16_bytes("héllo ✓")), "héllo ✓");
    }
}
