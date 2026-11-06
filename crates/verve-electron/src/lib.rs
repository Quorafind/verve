//! Electron on Verve: the browser process, one isolate per page, and the messages between them.
//!
//! An Electron app here is one process and a handful of threads. The OS thread runs the shell
//! (`crates/verve-gpui`) and only draws. [`MainGuest`] runs the app's own script in an isolate of
//! its own; every page a window shows is a [`RendererGuest`] in another. They never call each
//! other: the [`Hub`] carries what they say — `docs/electron/packages/P02-skeleton-and-contracts.md`
//! defines every message and the rule that makes them safe (only a page may wait for the main
//! process, never the other way round).
//!
//! Every API a script calls is a native Rust function on Boa: nothing in this crate is
//! JavaScript standing in for Electron.

pub mod api;
mod beat;
pub mod headless;
pub mod hub;
pub mod ids;
mod ipc;
pub mod launcher;
mod loader;
pub mod main_guest;
pub mod msg;
mod os;
mod oscrypt;
mod pixels;
mod probe;
pub mod renderer_guest;
mod single_instance;
mod window_state;

use std::path::PathBuf;

pub use headless::{FakeLayout, HeadlessOptions, HeadlessShell, HeadlessWindow};
pub use hub::{ConsoleLine, Hub, RendererHandle, ShellPort};
pub use main_guest::MainGuest;
pub use renderer_guest::RendererGuest;
pub use verve::{ELECTRON_VERSION, MODULES_VERSION};

/// What the app is before its first script runs.
pub struct ElectronOptions {
    /// The app's directory — `electron <dir>`. A relative entry is resolved against it, and P10
    /// makes it `app.getAppPath()`.
    pub app_path: PathBuf,
    /// `nativeTheme.shouldUseDarkColors` at start: what a page's `prefers-color-scheme` says.
    pub dark: bool,
}
