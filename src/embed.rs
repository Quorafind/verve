//! What an embedder puts in place of this runtime's defaults. Each method here changes one thing
//! about a realm — the process it says it is, the files it reads, where its console goes — and
//! nothing runs unless an embedder calls it, so a realm that never does is the one `verve run`
//! and the plugin host have always had.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::{Context, JsResult, JsValue};

use crate::ImportMap;
use crate::vfs;
use crate::{Level, ModuleMaker, ProcessInfo, Runtime, native, navigator, process, shared};

/// Where a console line came from, when the runtime knows: the file it was written in, and the
/// line and column in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourcePos {
    /// The file it was written in, as the runtime names it.
    pub file: String,
    /// The line in that file, counting from 1.
    pub line: u32,
    /// The column in that line, counting from 1.
    pub column: u32,
}

/// What a host is told of each console line as it is written; see
/// [`Runtime::set_console_sink`]. The third argument is the line's position, when there is one: where
/// the script that called the console is, or where an uncaught error was thrown. An uncaught
/// error's text has no frames in it when its position is given.
pub type ConsoleSink = Box<dyn Fn(Level, &str, Option<&SourcePos>)>;

/// The source of a module at one of the host's own addresses (`app://`, `https://`), from what
/// the host has fetched already; `None` when it has none for that address. See
/// [`Runtime::set_module_source`].
pub type ModuleSource = Rc<dyn Fn(&url::Url) -> Option<Rc<str>>>;

/// Where the script that called the console is: the newest frame running JavaScript.
pub(crate) fn call_site(context: &Context) -> Option<SourcePos> {
    let at = context.stack_trace().next()?.position();
    let boa_engine::vm::SourcePath::Path(file) = at.path else {
        return None;
    };
    let position = at.position?;
    Some(SourcePos {
        file: file.display().to_string(),
        line: position.line_number(),
        column: position.column_number(),
    })
}

/// `value` as `util.inspect(value)` shows it, with the realm's own `util.inspect.defaultOptions`
/// (a string keeps its quotes, as in a REPL) — for a host that shows a value to a person without
/// handing it over (an inspector's console). A value whose own inspect function throws is shown
/// plainly rather than lost.
pub fn inspect_value(context: &mut Context, value: &JsValue) -> String {
    // Unlike `util.format`, which `console.log` uses, `inspect` keeps a string's quotes.
    let shown = crate::util::default_options(context)
        .and_then(|options| crate::util::inspect(value, &options, context));
    shown.unwrap_or_else(|_| crate::print(value))
}

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

    /// Makes `require(name)` answer what `make` makes, the first time it is required — for a
    /// module as large as `electron`, which a program that never reaches for it should not pay
    /// for. What it makes is kept, as [`Runtime::define_builtin`]'s is; a later
    /// [`Runtime::define_builtin`] of the same name replaces it for the next require.
    pub fn define_builtin_lazy(&mut self, name: &str, make: ModuleMaker) {
        shared(&self.context)
            .lazy
            .borrow_mut()
            .insert(name.to_owned(), make);
    }

    /// Sends each console line to `sink` as it is written, instead of keeping it for
    /// [`Runtime::take_console`]: how a renderer passes its console to the main process without
    /// being polled. `None` keeps them again. A sink set is also where an uncaught error's line
    /// goes, so the host sees it with the rest.
    pub fn set_console_sink(&mut self, sink: Option<ConsoleSink>) {
        let kept: Option<crate::ConsoleSinkRc> = sink.map(Rc::from);
        *shared(&self.context).console_sink.borrow_mut() = kept;
    }

    /// The user agent `navigator` answers, whole: Electron's renderers carry their application's
    /// name in theirs (`... Chrome/142.0.7444.59 Electron/39.2.0 MyApp/1.0 Safari/537.36`). Set
    /// before the navigator is first read or after; either way it answers this from now on.
    pub fn set_user_agent(&mut self, agent: String) {
        navigator::set_user_agent(&mut self.context, agent);
    }

    /// Lets modules come from the host's own addresses. `source` answers, from the host's own
    /// cache (the host fetches a module graph before it runs, a browser tab's way), the text of
    /// the module at an address that is none of `file:`, `data:`, `blob:` or `node:`; an
    /// address it answers for is loadable by `import` and `import()`, its relative imports
    /// resolve against it as a page's do, and `import.meta.url` is that address. An address it
    /// has nothing for is `ERR_MODULE_NOT_FOUND`. `None` takes the hook away.
    pub fn set_module_source(&mut self, source: Option<ModuleSource>) {
        shared(&self.context).esm.set_source_hook(source);
    }

    /// Says whether the page this realm runs may load local files as modules. With `false`,
    /// every `file:` module address is refused — an entry, a static `import`, an `import()`, an
    /// import map's target — whoever the importer is, with Chromium's `TypeError`: Electron's
    /// rule that a page whose own address is not a `file:` one cannot read the disk. The
    /// default is `true`, and `verve run` is untouched.
    pub fn set_local_modules(&mut self, allowed: bool) {
        shared(&self.context).esm.refuse_local(!allowed);
    }

    /// Applies an import map to every `import` and `import()` of this realm, before its own
    /// resolution; `None` takes it away. See [`ImportMap::parse`].
    pub fn set_import_map(&mut self, map: Option<ImportMap>) {
        shared(&self.context).esm.set_import_map(map);
    }

    /// Makes every `import` and `import()` of this realm resolve the web's way, whoever the
    /// importer is — a `file:`, `data:` or `blob:` module as much as a hosted one: addresses
    /// relative to the importer or whole, bare specifiers only through the import map, no
    /// Node builtins, no `node_modules`, no CommonJS. A page without `nodeIntegration` has
    /// no Node; the default is off, and `verve run` is untouched.
    pub fn set_web_modules(&mut self, on: bool) {
        shared(&self.context).esm.set_web_only(on);
    }
}
