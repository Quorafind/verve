//! What type a file of this name is, as a server would say: the one table the page loader and
//! `net.fetch`'s `file:` URLs share. It lists the types the web uses, not the whole registry
//! (`mime_guess` would add a hundred kilobytes for the rest); anything else is
//! `application/octet-stream`.

use std::path::Path;

const TYPES: &[(&str, &str)] = &[
    ("html", "text/html"),
    ("htm", "text/html"),
    ("xhtml", "application/xhtml+xml"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("cjs", "text/javascript"),
    ("css", "text/css"),
    ("json", "application/json"),
    ("map", "application/json"),
    ("webmanifest", "application/manifest+json"),
    ("xml", "text/xml"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("ics", "text/calendar"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("apng", "image/apng"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
    ("cur", "image/x-icon"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("eot", "application/vnd.ms-fontobject"),
    ("wasm", "application/wasm"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("7z", "application/x-7z-compressed"),
    ("rar", "application/vnd.rar"),
    ("bin", "application/octet-stream"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("mid", "audio/midi"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
    ("avi", "video/x-msvideo"),
    ("mpeg", "video/mpeg"),
    ("vtt", "text/vtt"),
    ("rtf", "application/rtf"),
    ("doc", "application/msword"),
    ("xls", "application/vnd.ms-excel"),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
];

/// The type a file named like `path` is.
pub(crate) fn of_path(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lower = extension.to_ascii_lowercase();
    TYPES
        .iter()
        .find(|(known, _)| *known == lower)
        .map_or("application/octet-stream", |(_, mime)| mime)
}

/// An extension a file of type `mime` is usually given, if it has a distinctive one.
pub(crate) fn extension_of(mime: &str) -> Option<&'static str> {
    if mime == "application/octet-stream" {
        return None;
    }
    TYPES
        .iter()
        .find(|(_, known)| *known == mime)
        .map(|(extension, _)| *extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_the_type_its_extension_says_whatever_the_case() {
        for (name, mime) in [
            ("index.HTML", "text/html"),
            ("a/b/app.mjs", "text/javascript"),
            ("font.woff2", "font/woff2"),
            ("x.wasm", "application/wasm"),
            ("noextension", "application/octet-stream"),
            ("archive.unknown", "application/octet-stream"),
        ] {
            assert_eq!(of_path(Path::new(name)), mime, "{name}");
        }
    }
}
