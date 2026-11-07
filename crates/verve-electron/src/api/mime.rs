//! What type a file of this name is, as a server would say: the one table the page loader
//! names a file's type from. Anything it does not list is `application/octet-stream`.

use std::path::Path;

const TYPES: &[(&str, &str)] = &[
    ("html", "text/html"),
    ("htm", "text/html"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("cjs", "text/javascript"),
    ("css", "text/css"),
    ("json", "application/json"),
    ("map", "application/json"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("ico", "image/x-icon"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
    ("wasm", "application/wasm"),
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
