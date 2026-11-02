//! What an embedder puts in place of this runtime's defaults. Each method here changes one thing
//! about a realm — the process it says it is, the files it reads, where its console goes — and
//! nothing runs unless an embedder calls it, so a realm that never does is the one `verve run`
//! and the plugin host have always had.

use boa_engine::JsResult;

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
}
