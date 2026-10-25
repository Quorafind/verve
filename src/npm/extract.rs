//! Unpacking a package tarball: gzip over tar, its top folder (`package/`) stripped.
//!
//! A tarball is the publisher's, not ours, so nothing in it decides where a file lands. An entry
//! naming an absolute path, a drive, or a `..` refuses the whole package rather than being
//! quietly rewritten: a tarball built to escape its folder is not one to install at all. Links
//! are skipped, as pacote skips them — the registry has never served packages that need them,
//! and a link could point a later entry outside the folder. Nothing in it is ever run.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;

/// More than any published package unpacks to; a tarball past it is a decompression bomb.
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug)]
pub enum ExtractError {
    /// An entry that would land outside the package folder, or a tarball too big to be one.
    Unsafe { entry: String, reason: String },
    /// Not a gzipped tar, or truncated.
    Corrupt(String),
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
}

/// The path an entry lands at under the package folder: its name less the first component,
/// checked. `Ok(None)` for the top folder itself and for entries with nothing below it.
fn relative_path(raw: &str) -> Result<Option<PathBuf>, String> {
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err("is an absolute path".to_owned());
    }
    let parts: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.contains(&"..") {
        return Err("climbs out of the package with `..`".to_owned());
    }
    if parts
        .first()
        .is_some_and(|first| first.len() >= 2 && first.as_bytes()[1] == b':')
    {
        return Err("names a drive".to_owned());
    }
    let mut path = PathBuf::new();
    for part in parts.iter().skip(1) {
        if cfg!(windows) && part.contains([':', '<', '>', '"', '|', '?', '*']) {
            return Err("has a character Windows does not allow in a file name".to_owned());
        }
        path.push(part);
    }
    // Belt and braces: whatever `Path` makes of it must be plain names only.
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("is not a plain relative path".to_owned());
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ExtractError + '_ {
    move |error| ExtractError::Io {
        path: path.to_owned(),
        error,
    }
}

/// Unpacks `tarball` into `dest` (which must not exist yet), answering the bytes written.
pub fn extract(tarball: &[u8], dest: &Path) -> Result<u64, ExtractError> {
    std::fs::create_dir_all(dest).map_err(io(dest))?;
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));
    let entries = archive
        .entries()
        .map_err(|error| ExtractError::Corrupt(error.to_string()))?;
    let mut written = 0u64;
    for (count, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|error| ExtractError::Corrupt(error.to_string()))?;
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        if count >= MAX_ENTRIES {
            return Err(ExtractError::Unsafe {
                entry: raw,
                reason: format!("is past {MAX_ENTRIES} entries"),
            });
        }
        let kind = entry.header().entry_type();
        let relative = relative_path(&raw).map_err(|reason| ExtractError::Unsafe {
            entry: raw.clone(),
            reason,
        })?;
        let Some(relative) = relative else { continue };
        let path = dest.join(&relative);
        if kind.is_dir() {
            std::fs::create_dir_all(&path).map_err(io(&path))?;
            continue;
        }
        if !(kind.is_file() || kind == tar::EntryType::Continuous) {
            // Links, devices and FIFOs: none belongs in an npm package.
            continue;
        }
        let size = entry.header().size().unwrap_or(0);
        written += size;
        if written > MAX_UNPACKED {
            return Err(ExtractError::Unsafe {
                entry: raw,
                reason: format!("unpacks past {MAX_UNPACKED} bytes"),
            });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path).map_err(io(&path))?;
        }
        let mut bytes = Vec::with_capacity(size.min(64 * 1024 * 1024) as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| ExtractError::Corrupt(error.to_string()))?;
        std::fs::write(&path, &bytes).map_err(io(&path))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = entry.header().mode().unwrap_or(0o644) & 0o111 != 0;
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .map_err(io(&path))?;
        }
    }
    Ok(written)
}

/// The `package.json` a tarball carries, for a dependency given as a tarball URL.
pub fn read_package_json(tarball: &[u8]) -> Result<serde_json::Value, String> {
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));
    let entries = archive.entries().map_err(|error| error.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let raw = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        if matches!(relative_path(&raw), Ok(Some(path)) if path == Path::new("package.json")) {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|error| error.to_string())?;
            return serde_json::from_str(&text)
                .map_err(|error| format!("its package.json: {error}"));
        }
    }
    Err("it has no package.json".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_are_stripped_and_checked() {
        assert_eq!(
            relative_path("package/lib/a.js").unwrap(),
            Some(PathBuf::from("lib/a.js"))
        );
        assert_eq!(
            relative_path("node/index.js").unwrap(),
            Some(PathBuf::from("index.js"))
        );
        assert_eq!(relative_path("package/").unwrap(), None);
        assert_eq!(
            relative_path("./package/./x").unwrap(),
            Some(PathBuf::from("x"))
        );
        assert!(relative_path("package/../../evil").is_err());
        assert!(relative_path("/etc/passwd").is_err());
        assert!(relative_path("C:/x/y").is_err());
        assert!(relative_path("package\\..\\..\\evil").is_err());
    }
}
