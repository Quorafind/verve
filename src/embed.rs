//! What an embedder puts in place of this runtime's defaults. Each method here changes one thing
//! about a realm — the process it says it is, the files it reads, where its console goes — and
//! nothing runs unless an embedder calls it, so a realm that never does is the one `verve run`
//! and the plugin host have always had.

use std::path::{Path, PathBuf};

use boa_engine::JsResult;

use crate::vfs;
use crate::{ProcessInfo, Runtime, native, process, shared};

impl Runtime {
    /// Makes `process` what this realm is: an Electron main process, a renderer, a utility —
    /// what [`ProcessInfo`] describes, from `type` and `title` to what `exit` and an uncaught
    /// error do here. It takes effect at once, on the `process` this realm already has; the
    /// default is the plugin host's shape, unchanged.
    ///
    /// # Errors
    ///
    /// A property of `process` that could not be written.
    pub fn set_process_info(&mut self, info: ProcessInfo) -> JsResult<()> {
        *shared(&self.context).process.borrow_mut() = info.clone();
        let process = native::global(&mut self.context, "process").unwrap_or_default();
        match process.as_object() {
            Some(process) => process::apply(&mut self.context, &process, &info),
            None => Ok(()),
        }
    }

    /// Reads and writes through `overlay` — a packed `app.asar`, or whatever else the embedder
    /// keeps out of the disk's sight — or, with `None`, the disk alone. Every `fs` entry point,
    /// `require` and the ES module loader go through it from the next call on; files already
    /// open stay as they are. See [`vfs::Overlay`].
    pub fn set_vfs(&mut self, overlay: Option<std::sync::Arc<dyn vfs::Overlay>>) {
        let holds = overlay.is_some();
        shared(&self.context).vfs.borrow_mut().overlay = overlay;
        // `process.noAsar` exists where there is something for it to put aside. Only a
        // `process` that cannot be changed refuses, and then the embedder still has
        // `set_no_asar`.
        let _ = if holds {
            process::expose_no_asar(&mut self.context)
        } else {
            process::hide_no_asar(&mut self.context)
        };
    }

    /// Puts the overlay aside, as `process.noAsar = true` does in Electron (and does here, on a
    /// realm that has an overlay): what is read from now on comes from the disk alone, whatever
    /// [`Runtime::set_vfs`] set.
    pub fn set_no_asar(&mut self, on: bool) {
        shared(&self.context).vfs.borrow_mut().no_asar = on;
    }

    /// Where a file only the virtual filesystem holds can be read by something that is not this
    /// runtime — a program `child_process` runs, a native module; `None` when the disk has it.
    pub fn materialize(&self, path: &Path) -> Option<std::io::Result<PathBuf>> {
        vfs::View::of(&self.context).materialize(path)
    }
}
