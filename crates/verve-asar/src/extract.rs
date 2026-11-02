//! Packed files written out to real ones: what a program that is not this runtime — a child
//! process, a native module — can open by path. Electron does the same for `execFile` and
//! friends, into a temporary folder it removes on exit.
//!
//! The folder is made new, under a name no one can guess, and readable by its owner alone; each
//! file in it is created new too. A directory or a link someone else put in the way is an
//! error, not a place to write, since the file is about to be run.

use std::collections::HashMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use verve::encoding::hex;

use super::archive::{Archive, Entry, Resolved};

/// How many names are tried for the folder before giving up: each is a fresh 64-bit draw.
const ATTEMPTS: usize = 8;

type Made = Arc<Mutex<Option<PathBuf>>>;

/// The files extracted so far, in a folder of this overlay's own that goes when it does.
pub(super) struct Extracted {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    root: Option<PathBuf>,
    made: HashMap<(PathBuf, String), Made>,
}

impl Extracted {
    /// Nothing extracted yet; the folder is made when the first file is.
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::default(),
        }
    }

    /// The real file `found`, in `archive`, is written to: once for each file, under its own
    /// name inside a folder named for where it came from, so two files with one name do not
    /// meet. Extractions of different files do not wait for one another.
    pub(super) fn file(&self, archive: &Archive, found: &Resolved<'_>) -> io::Result<PathBuf> {
        let key = (archive.path().to_path_buf(), found.path.clone());
        let (root, made) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let root = match &state.root {
                Some(root) => root.clone(),
                None => {
                    let root = private_root(&std::env::temp_dir())?;
                    state.root = Some(root.clone());
                    root
                }
            };
            (root, Arc::clone(state.made.entry(key).or_default()))
        };
        let mut slot = made.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(path) = slot.as_ref() {
            return Ok(path.clone());
        }
        let path = write_file(&root, archive, found)?;
        *slot = Some(path.clone());
        Ok(path)
    }
}

impl Drop for Extracted {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(root) = &state.root {
            // Best effort: a folder left in the temporary directory is only untidy.
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// A new folder in `parent` with a name of its own, which only this user can enter.
fn private_root(parent: &Path) -> io::Result<PathBuf> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..ATTEMPTS {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = format!(
            "verve-electron-asar-{}-{}",
            std::process::id(),
            hex(&random)
        );
        let root = parent.join(name);
        match private_dir(&root) {
            Ok(()) => return Ok(root),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Makes the folder `path`, which must not exist: one that does is someone else's.
fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn write_file(root: &Path, archive: &Archive, found: &Resolved<'_>) -> io::Result<PathBuf> {
    let bytes = archive.read_found(found)?;
    let origin = format!("{}\0{}", archive.path().display(), found.path);
    let folder = root.join(&hex(&Sha256::digest(origin))[..16]);
    private_dir(&folder)?;
    let name = found.path.rsplit('/').next().unwrap_or(&found.path);
    let path = folder.join(name);
    let executable = matches!(found.entry, Entry::File(file) if file.executable);
    let written = create_new(&path, executable).and_then(|mut file| file.write_all(&bytes));
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(error);
    }
    Ok(path)
}

/// A file that must not exist yet — so no link is followed — private to this user.
fn create_new(path: &Path, executable: bool) -> io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o700 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_exists_is_never_taken_over() {
        let parent =
            std::env::temp_dir().join(format!("verve-asar-extract-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let planted = parent.join("planted");
        std::fs::create_dir(&planted).unwrap();
        let error = private_dir(&planted).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        // A file made new refuses one that is there, a link or not.
        let file = planted.join("f");
        std::fs::write(&file, "mine").unwrap();
        let error = create_new(&file, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&file).unwrap(), b"mine");
        // Roots made in the same place do not meet.
        let (a, b) = (
            private_root(&parent).unwrap(),
            private_root(&parent).unwrap(),
        );
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
