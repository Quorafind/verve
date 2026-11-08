//! Electron's `app.asar` for [Verve](verve): a folder packed into one file, which Electron's
//! `fs` and `require` read as a folder (`lib/node/asar-fs-wrapper.ts` in Electron 39) and
//! `original-fs` does not.
//!
//! [`Archive`] is one opened `.asar`: the header parsed once, a file's bytes read where they
//! lie. [`AsarOverlay`] is what a host gives `Runtime::set_vfs`: it finds the archive in a path
//! and answers `fs` for what is inside, in Electron's words.

mod archive;
mod extract;
mod integrity;
mod overlay;

#[cfg(test)]
mod tests;

pub use archive::{Archive, AsarError};
pub use overlay::{AsarOptions, AsarOverlay, IntegrityFailure};
