//! Verve: the Node-shaped environment a community plugin's `main.js` expects, on Boa.
//!
//! A plugin is a CommonJS module. It is handed `module`, `exports` and `require`, it reaches
//! for `setTimeout`, `setInterval`, `queueMicrotask`, `console` and `process`, and it `await`s.
//! Each of those is here as a native Rust function on [Boa], a JavaScript interpreter written in
//! Rust — there is no Node, no Electron, and no JavaScript written to stand in for either.
//!
//! What a module may `require` beyond its own files is whatever the host registered under that
//! name with [`Runtime::define_builtin`]: the Obsidian host registers `obsidian` there, as a
//! module object whose classes and functions are native too.
//!
//! The runtime does not keep time on its own. The host owns the clock and the loop: it asks
//! [`Runtime::next_due`] when the next timer is, and calls [`Runtime::run_due`] then — from a
//! window's frame, a test's fake clock, whatever drives it. Every entry into JavaScript ends by
//! running the microtasks it queued, so a promise a callback started settles before the host
//! looks again.
//!
//! A script can also be a program of its own: [`cli::main`] is `verve run`, which runs one as
//! `node` would, under the permission model ([`Permissions`]).
//!
//! [Boa]: https://boajs.dev

/// The Boa this runtime is built on, for a host defining natives of its own in it: the same
/// crates, at the same version, so the values it makes are this runtime's.
pub use boa_engine;
pub use boa_gc;

mod assert;
mod buffer;
mod cjs_exports;
pub mod cli;
mod code_cache;
mod crypto;
pub mod dom_events;
mod encoding;
mod esm;
mod esm_resolve;
mod events;
mod fetch;
mod fs;
mod hooks;
mod indexeddb;
mod intl;
pub mod isolate;
mod messaging;
mod module_hooks;
mod modules;
mod native;
mod navigator;
mod network;
mod node;
mod node_url;
#[cfg(feature = "npm")]
pub mod npm;
mod os;
mod package_json;
mod path;
mod permission;
mod process;
mod querystring;
mod readline;
mod sockets;
mod stream;
mod streams;
mod string_decoder;
mod timers;
mod typescript;
mod url;
mod url_pattern;
mod util;
mod vm;
mod wasm;
mod web;
mod worker;
mod zlib;

pub use code_cache::CodeCacheStats;

/// The user's own code cache secret: 32 random bytes in a file only they can read, outside any
/// cache directory (`%LOCALAPPDATA%\Verve\code-cache-secret` on Windows,
/// `$XDG_CONFIG_HOME/verve/code-cache-secret` or `~/.config/verve/code-cache-secret`
/// elsewhere), made the first time.
///
/// # Errors
///
/// If there is no such place, or the file or its folder is not private to the user, or the file
/// is not a secret.
pub fn code_cache_user_secret() -> std::io::Result<[u8; 32]> {
    code_cache::user_secret().map(|(secret, _)| secret)
}
pub use network::{ListenPermission, NetworkDiagnostics, NetworkRule, ProxyEnv};
pub use permission::{FsAccess, Permissions};

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
    context::ContextBuilder,
    job::{Job, JobExecutor, PromiseJob},
    js_string,
    object::ObjectInitializer,
    property::Attribute,
};

thread_local! {
    /// The stack size of this thread, when whoever made it said; see [`set_thread_stack`].
    static THREAD_STACK: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Says how big this thread's stack is, so a runtime made on it lets scripts use about half of
/// it before deep recursion stops with a `RangeError`. Call it first thing on a thread made with
/// `std::thread::Builder::stack_size`; workers and isolates do it themselves.
pub fn set_thread_stack(bytes: usize) {
    THREAD_STACK.with(|stack| stack.set(Some(bytes)));
}

/// What makes a module of a family the host defined; see [`Runtime::define_builtin_family`].
pub type ModuleMaker = fn(&mut Context, &str) -> JsResult<JsValue>;

/// Milliseconds on the host's clock. Only differences between two readings mean anything.
pub type Millis = f64;

/// One line a module wrote to the console.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleLine {
    pub level: Level,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// A timer `setTimeout` or `setInterval` started.
struct Timer {
    due: Millis,
    repeat: Option<Millis>,
    callback: JsValue,
    args: Vec<JsValue>,
    /// Whether it keeps a process waiting for it: Node's `ref()` and `unref()`.
    refed: bool,
}

/// What a host is told with when work arrives for its realm from another thread — I/O the
/// network finished, a message a worker sent — so it can sleep until then instead of looking
/// every millisecond. See [`Runtime::set_waker`].
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// The realm's waker, shared with the threads that wake it.
#[derive(Default)]
pub(crate) struct WakeSlot(std::sync::RwLock<Option<Waker>>);

impl WakeSlot {
    pub(crate) fn wake(&self) {
        if let Ok(waker) = self.0.read()
            && let Some(waker) = waker.as_ref()
        {
            waker();
        }
    }

    fn set(&self, waker: Option<Waker>) {
        if let Ok(mut slot) = self.0.write() {
            *slot = waker;
        }
    }

    fn is_set(&self) -> bool {
        self.0.read().is_ok_and(|slot| slot.is_some())
    }
}

/// What the runtime's native functions share, kept in the context's host data.
struct Shared {
    clock: Box<dyn Fn() -> Millis>,
    next_timer: Cell<u32>,
    timers: RefCell<BTreeMap<u32, Timer>>,
    builtins: RefCell<HashMap<String, JsValue>>,
    /// Families of built-in modules: every name starting with the prefix is made by the
    /// function, once, and kept with the rest.
    families: RefCell<Vec<(String, ModuleMaker)>>,
    /// `require.cache`: the module object of every file run, by its path. A second `require`
    /// of the same file answers the same `module.exports`, as Node's cache does.
    module_cache: JsObject,
    /// The URL-keyed ESM graph, shared by dynamic imports and require(esm).
    esm: Rc<esm::Loader>,
    /// The `this` a class was called on without `new` (`EventEmitter.call(this)`), from its
    /// `apply` trap to its constructor; see `native::instance`.
    called_on: RefCell<Option<JsValue>>,
    /// The working folder `process.cwd()` answers and `process.chdir` changes.
    cwd: RefCell<String>,
    console: RefCell<Vec<ConsoleLine>>,
    /// `console.time`'s labels and when each was started; `console.count`'s counts.
    console_timers: RefCell<HashMap<String, Millis>>,
    console_counts: RefCell<HashMap<String, u64>>,
    /// How deep in `console.group` the next line is written.
    console_depth: std::cell::Cell<usize>,
    echo: bool,
    /// What each `URL.createObjectURL` address stands for: the blob's bytes.
    blobs: RefCell<HashMap<String, Vec<u8>>>,
    /// The workers this realm started.
    network: RefCell<network::State>,
    workers: RefCell<Vec<worker::WorkerSlot>>,
    /// In a worker's own realm: its messages, coming and going.
    port: RefCell<Option<worker::Port>>,
    /// IndexedDB's databases, transactions and cursors.
    idb: RefCell<indexeddb::IdbState>,
    /// WebAssembly's modules and instances.
    wasm: RefCell<wasm::WasmState>,
    /// Who to wake when the network or a worker has something for this realm.
    wake: std::sync::Arc<WakeSlot>,
    /// Set when this realm is a process of its own (`verve run`): see `process::Profile`.
    profile: RefCell<Option<Rc<process::Profile>>>,
    /// Where compiled modules are kept between runs, if anywhere: see `code_cache`.
    code_cache: RefCell<code_cache::Shared>,
    /// 即使 Worker 自身不启用缓存，也会继承这些访问拒绝规则。
    code_cache_protection: RefCell<code_cache::Protection>,
}

/// The microtask queue: promise reactions, `queueMicrotask`, `process.nextTick`.
#[derive(Default)]
struct Microtasks {
    queue: RefCell<VecDeque<PromiseJob>>,
    generic: RefCell<VecDeque<boa_engine::job::GenericJob>>,
    asynchronous: RefCell<VecDeque<boa_engine::job::NativeAsyncJob>>,
}

impl JobExecutor for Microtasks {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        match job {
            // Carrying the `AsyncLocalStorage` stores set as it was queued.
            Job::PromiseJob(job) => self
                .queue
                .borrow_mut()
                .push_back(hooks::carry(job, context)),
            Job::GenericJob(job) => self.generic.borrow_mut().push_back(job),
            // The native module loader returns Ready. TLA is continued by promise reactions,
            // never by waiting for a host timer while a job holds the context.
            Job::AsyncJob(job) => self.asynchronous.borrow_mut().push_back(job),
            // Timers are this runtime's own; finalization jobs do not keep a realm alive.
            _ => {}
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        loop {
            let next = self.queue.borrow_mut().pop_front();
            if let Some(job) = next {
                if let Err(error) = job.call(context) {
                    report(context, &error);
                }
                continue;
            }
            let next = self.generic.borrow_mut().pop_front();
            match next {
                Some(job) => {
                    if let Err(error) = job.call(context) {
                        report(context, &error);
                    }
                }
                None => {
                    let next = self.asynchronous.borrow_mut().pop_front();
                    let Some(job) = next else {
                        // The turn's microtasks are done: what it left rejected and unhandled
                        // is told now, and a listener's own microtasks run before the turn ends.
                        if hooks::report_rejections(context) {
                            continue;
                        }
                        return Ok(());
                    };
                    let context = RefCell::new(&mut *context);
                    let polled = {
                        let mut future = std::pin::pin!(job.call(&context));
                        let mut task = std::task::Context::from_waker(std::task::Waker::noop());
                        std::future::Future::poll(future.as_mut(), &mut task)
                    };
                    // Release any context borrow held by the future before reporting errors.
                    match polled {
                        std::task::Poll::Ready(Ok(_)) => {}
                        std::task::Poll::Ready(Err(error)) => {
                            report(&mut context.borrow_mut(), &error)
                        }
                        std::task::Poll::Pending => {
                            // Runtime installs no asynchronous source loader. It must not
                            // block an embedder's tick on an externally supplied future.
                            let error = JsNativeError::error()
                                .with_message("A native module job unexpectedly suspended")
                                .into();
                            report(&mut context.borrow_mut(), &error);
                        }
                    }
                }
            }
        }
    }
}

/// The runtime: one JavaScript realm, its module loader, its timers and its console.
pub struct Runtime {
    context: Context,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Replace realm-local capabilities: the network (default denied), the filesystem and
    /// workers (default unrestricted, as embedders have always had them). A changed network
    /// policy closes existing sockets; filesystem grants apply to the next operation — files
    /// already open stay open, as Node's descriptors opened before do. Workers this realm starts
    /// from now on share its filesystem and worker grants; their network is
    /// [`Runtime::set_worker_permissions`]'s. Child process execution stays disabled.
    ///
    /// Filesystem grants are resolved when set — made absolute against the process's working
    /// directory, links followed — and every `fs` entry point and every module read from disk
    /// checks against them, failing with Node's `ERR_ACCESS_DENIED`. See [`FsAccess`].
    pub fn set_permissions(&mut self, permissions: Permissions) {
        let state = shared(&self.context);
        let mut net = state.network.borrow_mut();
        if !net.permissions.same_network(&permissions) {
            net.reactor = None;
            let sockets = net.targets.keys().copied().collect::<Vec<_>>();
            let queries = net.dns.pending.keys().copied().collect::<Vec<_>>();
            net.revoked.extend(
                sockets
                    .into_iter()
                    .map(|id| (id, network::reactor::Event::Closed)),
            );
            net.revoked.extend(
                queries
                    .into_iter()
                    .map(|id| (id, network::reactor::Event::Dns(Err("EACCES".into())))),
            );
        }
        if net.permissions.fs_read != permissions.fs_read
            || net.permissions.fs_write != permissions.fs_write
        {
            net.grants = Rc::new(permission::Grants::of(&permissions));
        }
        net.permissions = permissions;
    }

    pub fn permissions(&self) -> Permissions {
        shared(&self.context).network.borrow().permissions.clone()
    }

    /// The network the workers this realm starts from now on are granted. Workers are denied
    /// the network unless the embedder grants it here; a worker's own workers get the same.
    /// Their filesystem and worker grants are this realm's own (see
    /// [`Runtime::set_permissions`]), whatever this says: a worker reaches no file its page
    /// cannot.
    pub fn set_worker_permissions(&mut self, permissions: Permissions) {
        shared(&self.context)
            .network
            .borrow_mut()
            .worker_permissions = permissions;
    }

    /// Trusts the certificates in `pem` besides the system's, for `fetch`, `https` and
    /// `tls.connect` without a `ca` of their own — as Node's `NODE_EXTRA_CA_CERTS` does — here
    /// and in the workers started from now on. Answers how many certificates it read; a PEM
    /// with none is an error.
    pub fn add_trusted_certificates(&mut self, pem: &[u8]) -> std::io::Result<usize> {
        let count = network::tls::certificates(pem)?.len();
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no PEM certificate",
            ));
        }
        shared(&self.context)
            .network
            .borrow_mut()
            .trusted
            .push(pem.to_vec());
        Ok(count)
    }

    /// Routes `fetch`, `WebSocket` and the `http`/`https` global agents through a proxy, as Node
    /// 24 does when `NODE_USE_ENV_PROXY=1` (`verve run` sets it from its environment that way;
    /// [`ProxyEnv::from_process_env`] reads it for an embedder that wants the same). `None`
    /// connects directly again. Workers started from now on get the same proxy.
    ///
    /// Outbound permissions are checked against each request's target, never its proxy: the
    /// proxies named here need no [`NetworkRule`] of their own. Connections already open stay
    /// as they are. See the `proxy` module of `network` for what is proxied and how `NO_PROXY`
    /// matches.
    pub fn set_proxy_env(&mut self, proxy: Option<ProxyEnv>) {
        let state = shared(&self.context);
        let mut net = state.network.borrow_mut();
        net.proxy = proxy.map(|env| {
            let config = Rc::new(network::proxy::Config::parse(&env));
            (env, config)
        });
        let proxies = network::configured_proxies(&net);
        if let Some(reactor) = &net.reactor {
            reactor.send(network::reactor::Command::Proxies(proxies));
        }
    }

    /// The proxy [`Runtime::set_proxy_env`] set, if any.
    pub fn proxy_env(&self) -> Option<ProxyEnv> {
        shared(&self.context)
            .network
            .borrow()
            .proxy
            .as_ref()
            .map(|(env, _)| env.clone())
    }

    /// Read counters without waiting for any network thread.
    pub fn network_diagnostics(&self) -> NetworkDiagnostics {
        network::diagnostics(&self.context)
    }

    /// Deliver already queued I/O, without waiting for the reactor or resolver pool.
    pub fn run_network_until(&mut self, deadline: Option<std::time::Instant>) -> usize {
        network::poll(&mut self.context, deadline)
    }

    /// A runtime on the system's monotonic clock.
    pub fn new() -> Self {
        let start = web_time::Instant::now();
        Self::with_clock(move || start.elapsed().as_secs_f64() * 1000.)
    }

    /// A runtime on a clock of the host's choosing — a test's, which only moves when told to.
    pub fn with_clock(clock: impl Fn() -> Millis + 'static) -> Self {
        let microtasks = Rc::new(Microtasks::default());
        let esm = Rc::new(esm::Loader::default());
        let mut context = ContextBuilder::new()
            .host_hooks(Rc::new(hooks::Hooks))
            .job_executor(microtasks)
            .module_loader(esm.clone())
            .build()
            .expect("a Boa context builds");
        // Deep recursion through natives stops with a RangeError before the thread's stack
        // runs out: half of it, on a thread whose size is known (see `set_thread_stack`).
        if let Some(stack) = THREAD_STACK.with(Cell::get) {
            context
                .runtime_limits_mut()
                .set_native_stack_limit(stack / 2);
        }
        #[cfg(feature = "jit")]
        jit_from_env(&mut context);
        // A whole test suite can run with a code cache: see the `code-cache-env` feature.
        #[cfg(feature = "code-cache-env")]
        let code_cache = std::env::var_os("VERVE_CODE_CACHE")
            .filter(|dir| !dir.is_empty())
            .and_then(|dir| {
                let (secret, file) = code_cache::user_secret().ok()?;
                code_cache::CodeCache::open(std::path::Path::new(&dir), secret, vec![file]).ok()
            })
            .map(Rc::new);
        #[cfg(not(feature = "code-cache-env"))]
        let code_cache: code_cache::Shared = None;
        let code_cache_protection = code_cache
            .as_ref()
            .map(|cache| cache.protection())
            .unwrap_or_default();
        // Functions compile when first called, unless `VERVE_LAZY_COMPILE=0`.
        context.set_lazy_compile(!std::env::var("VERVE_LAZY_COMPILE").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        }));
        context.insert_data(Rc::new(Shared {
            clock: Box::new(clock),
            next_timer: Cell::new(1),
            timers: RefCell::new(BTreeMap::new()),
            builtins: RefCell::new(HashMap::new()),
            families: RefCell::new(Vec::new()),
            module_cache: JsObject::with_null_proto(),
            esm,
            called_on: RefCell::new(None),
            cwd: RefCell::new(
                std::env::current_dir()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "/".into()),
            ),
            console: RefCell::new(Vec::new()),
            console_timers: RefCell::new(HashMap::new()),
            console_counts: RefCell::new(HashMap::new()),
            console_depth: std::cell::Cell::new(0),
            echo: std::env::var_os("VERVE_ECHO").is_some(),
            blobs: RefCell::new(HashMap::new()),
            network: RefCell::new(network::State::default()),
            workers: RefCell::new(Vec::new()),
            port: RefCell::new(None),
            idb: RefCell::new(indexeddb::IdbState::default()),
            wasm: RefCell::new(wasm::WasmState::default()),
            wake: std::sync::Arc::default(),
            profile: RefCell::new(None),
            code_cache: RefCell::new(code_cache),
            code_cache_protection: RefCell::new(code_cache_protection),
        }));
        install_globals(&mut context).expect("the globals install");
        Self { context }
    }

    /// Compile each function's body when the function is first called (the default), or with
    /// the module or script that declares it. What runs is the same either way; lazily, the
    /// code a module carries but never calls is never compiled, which is most of a bundle's at
    /// startup. `VERVE_LAZY_COMPILE=0` (also `off`, `false`, `no`) starts a runtime eager. It
    /// applies to code loaded after the change.
    pub fn set_lazy_compile(&mut self, lazy: bool) {
        self.context.set_lazy_compile(lazy);
    }

    /// Keeps the compiled code of the ES modules this runtime loads in `dir`, and reads it back
    /// in later runs instead of parsing and compiling the same source again. Off by default.
    /// Entries are signed with `secret`, and only an authentic entry for the very source text,
    /// build and global scope is read. `dir` is made private to the user if it is not there
    /// (owner-only on Windows, `0700` elsewhere), and refused unless the user owns it and no
    /// one else can write it; JavaScript this runtime and its workers run cannot read or write it.
    ///
    /// Whoever holds `secret` can make every module this runtime loads run code of their
    /// choosing: keep it where code less trusted than those modules cannot read it, never in
    /// `dir` (see `docs/startup.md`). [`code_cache_user_secret`] is the user's own, which
    /// `verve run --code-cache` uses.
    ///
    /// # Errors
    ///
    /// If `dir` cannot be made or is not private to the user.
    pub fn set_code_cache_dir_with_secret(
        &mut self,
        dir: &std::path::Path,
        secret: [u8; 32],
    ) -> std::io::Result<()> {
        self.set_code_cache(dir, secret, Vec::new())
    }

    fn set_code_cache(
        &mut self,
        dir: &std::path::Path,
        secret: [u8; 32],
        protected: Vec<PathBuf>,
    ) -> std::io::Result<()> {
        let cache = code_cache::CodeCache::open(dir, secret, protected)?;
        let state = shared(&self.context);
        *state.code_cache_protection.borrow_mut() = cache.protection();
        *state.code_cache.borrow_mut() = Some(Rc::new(cache));
        Ok(())
    }

    /// [`Runtime::set_code_cache_dir_with_secret`] with the user's own secret
    /// ([`code_cache_user_secret`]), which JavaScript this runtime runs cannot read either:
    /// what `verve run --code-cache` does.
    ///
    /// # Errors
    ///
    /// If `dir` cannot be made or is not private to the user, or the user's secret cannot be
    /// read, is not private to them or is not a secret.
    pub fn set_code_cache_dir_with_user_secret(
        &mut self,
        dir: &std::path::Path,
    ) -> std::io::Result<()> {
        let (secret, file) = code_cache::user_secret()?;
        self.set_code_cache(dir, secret, vec![file])
    }

    /// Stops keeping compiled code; what is compiled but not written yet is written first.
    pub fn disable_code_cache(&mut self) {
        *shared(&self.context).code_cache.borrow_mut() = None;
    }

    /// What the code cache has done since [`Runtime::set_code_cache_dir_with_secret`] set it:
    /// modules read
    /// back, compiled, refused and kept. `None` without one.
    pub fn code_cache_stats(&self) -> Option<CodeCacheStats> {
        code_cache::stats(&self.context)
    }

    /// Writes what the code cache has compiled to its directory now. It is written anyway
    /// once a burst of loading settles, when the process exits and when the runtime is
    /// dropped; a host about to be stopped without either can call this first.
    pub fn flush_code_cache(&mut self) {
        code_cache::flush_now(&self.context);
    }

    /// Whether functions are compiled when first called; see [`Runtime::set_lazy_compile`].
    pub fn lazy_compile(&self) -> bool {
        self.context.lazy_compile()
    }

    /// Whether hot functions run as native code. With the `jit` feature it is on unless
    /// `VERVE_JIT=0` (or `BOA_JIT=0`) is in the environment; without it, always off and this
    /// does nothing. Switched off, what was compiled stays unused: everything runs interpreted
    /// from the next call or loop iteration on.
    pub fn set_jit_enabled(&mut self, enabled: bool) {
        #[cfg(feature = "jit")]
        {
            self.context.jit_options_mut().enabled = enabled;
        }
        #[cfg(not(feature = "jit"))]
        let _ = enabled;
    }

    /// Whether hot functions run as native code; see [`Runtime::set_jit_enabled`].
    pub fn jit_enabled(&self) -> bool {
        #[cfg(feature = "jit")]
        {
            self.context.jit_options().enabled
        }
        #[cfg(not(feature = "jit"))]
        false
    }

    /// The JIT's options: how hot a function gets before it is compiled, and the limits on what
    /// is compiled.
    #[cfg(feature = "jit")]
    pub fn jit_options_mut(&mut self) -> &mut boa_engine::vm::JitOptions {
        self.context.jit_options_mut()
    }

    /// What the JIT has compiled and run so far.
    #[cfg(feature = "jit")]
    pub fn jit_stats(&self) -> boa_engine::vm::JitStats {
        self.context.jit_stats()
    }

    /// How much native stack a script may use before deep recursion becomes a `RangeError` —
    /// about half the stack of the thread it runs on. A runtime made on a thread of
    /// Verve's own (a worker, an isolate), or after [`set_thread_stack`], has it already.
    pub fn set_native_stack_limit(&mut self, bytes: usize) {
        self.context
            .runtime_limits_mut()
            .set_native_stack_limit(bytes);
    }

    /// 复制当前 realm 的 `URL.createObjectURL` 已有存储中的字节。
    ///
    /// 撤销后不再提供新副本；已交给解码器的副本由宿主管理。本接口不获取远程地址，
    /// 也不授予网络权限，非 blob URL 或不存在的 URL 返回 `None`。
    pub fn object_url_bytes(&self, url: &str) -> Option<Vec<u8>> {
        shared(&self.context).blobs.borrow().get(url).cloned()
    }

    /// The realm itself, for a host defining natives of its own in it.
    pub fn context(&mut self) -> &mut Context {
        &mut self.context
    }

    /// Makes `require` answer every module whose name starts with `prefix` with what `make`
    /// makes for that name — once for each name.
    pub fn define_builtin_family(&mut self, prefix: &str, make: ModuleMaker) {
        shared(&self.context)
            .families
            .borrow_mut()
            .push((prefix.to_owned(), make));
    }

    /// Makes `require(name)` — and `require("node:" + name)` — answer `exports`.
    pub fn define_builtin(&mut self, name: &str, exports: JsValue) {
        shared(&self.context)
            .builtins
            .borrow_mut()
            .insert(name.to_owned(), exports);
    }

    /// What `require(specifier)` answers, asked from native code: a built-in — the host's own
    /// (`obsidian`, `electron`), or Node's, with or without `node:` — or a file by its absolute
    /// path. A relative name is resolved against `process.cwd()`.
    pub fn require(&mut self, specifier: &str) -> Result<JsValue, String> {
        let result = require(&mut self.context, specifier);
        self.run_microtasks();
        result.map_err(|error| describe(&mut self.context, &error))
    }

    /// A `require` function of no module's — what Electron's renderer has as `window.require`,
    /// which desktop plugins call (`window.require('fs')`): built-ins by either name, files by
    /// absolute path, relative names against `process.cwd()`; with `resolve` and `cache`. The
    /// host puts it on `window` itself, if it has one.
    pub fn global_require(&mut self) -> JsValue {
        let cwd = PathBuf::from(shared(&self.context).cwd.borrow().clone());
        modules::require_function(&mut self.context, &cwd, None)
            .expect("a require function is made")
    }

    /// Runs `source` in the Node-selected format at `filename`: CommonJS exports or the
    /// synchronous ESM namespace. Use [`Self::load_esm`] for graphs with top-level await.
    /// A module's own `require` resolves relative names against the folder it is in.
    pub fn load_module(&mut self, filename: &Path, source: &str) -> Result<JsValue, String> {
        let path = esm::absolute(filename, &self.context);
        let result = (|| {
            if esm::is_esm(&path, source, &mut self.context)? {
                let loader = shared(&self.context).esm.clone();
                loader.insert_source(&path, source, &mut self.context)?;
                loader.require(&path, &mut self.context)
            } else {
                modules::run_module(&mut self.context, &path, source, None)
            }
        })();
        self.run_microtasks();
        result.map_err(|error| {
            // A module that throws as it runs is an uncaught error: on the console with where.
            report(&mut self.context, &error);
            describe(&mut self.context, &error)
        })
    }

    /// Loads native ESM source. The promise fulfills with its namespace; top-level await may
    /// leave it pending until the host drives timers or completes the awaited operation.
    pub fn load_esm(
        &mut self,
        filename: &Path,
        source: &str,
    ) -> Result<boa_engine::object::builtins::JsPromise, String> {
        let loader = shared(&self.context).esm.clone();
        let result = loader
            .insert_source(filename, source, &mut self.context)
            .and_then(|url| loader.start(&url, &mut self.context));
        self.run_microtasks();
        result.map_err(|error| describe(&mut self.context, &error))
    }

    /// Imports a URL, package, builtin or file relative to `parent` (or process.cwd()).
    /// Resolution failures are rejected promises, as dynamic import failures are in Node.
    pub fn import_module(
        &mut self,
        specifier: &str,
        parent: Option<&Path>,
    ) -> Result<boa_engine::object::builtins::JsPromise, String> {
        let loader = shared(&self.context).esm.clone();
        let result = loader
            .resolve(
                specifier,
                parent,
                esm_resolve::Mode::Import,
                true,
                &mut self.context,
            )
            .and_then(|url| loader.start(&url, &mut self.context));
        let promise = match result {
            Ok(promise) => Ok(promise),
            Err(error) => boa_engine::object::builtins::JsPromise::reject(error, &mut self.context),
        };
        self.run_microtasks();
        promise.map_err(|error| describe(&mut self.context, &error))
    }

    /// Additional package conditions, following Node's --conditions. Defaults stay Node's;
    /// browser conditions are opt-in and never grant network permissions.
    pub fn set_module_conditions(&mut self, conditions: Vec<String>) {
        shared(&self.context).esm.conditions(conditions);
    }

    /// Forgets a module run from `filename`, so loading it again runs it afresh — what
    /// disabling a plugin and enabling it again needs.
    pub fn forget_module(&mut self, filename: &Path) {
        modules::forget(&mut self.context, filename);
    }

    /// Runs a script in the global scope. For tests and for a host's own probing.
    pub fn eval(&mut self, source: &str) -> Result<JsValue, String> {
        let result = self.context.eval(Source::from_bytes(source));
        self.run_microtasks();
        result.map_err(|error| describe(&mut self.context, &error))
    }

    /// Calls `function` with `this` and `args`, then runs the microtasks it queued.
    pub fn call(
        &mut self,
        function: &JsValue,
        this: &JsValue,
        args: &[JsValue],
    ) -> Result<JsValue, String> {
        let result = call(&mut self.context, function, this, args);
        self.run_microtasks();
        result.map_err(|error| describe(&mut self.context, &error))
    }

    /// Runs every queued microtask, and the ones they queue, until there are none.
    pub fn run_microtasks(&mut self) {
        let _ = self.context.run_jobs();
        code_cache::flush(&self.context, false);
    }

    /// Where IndexedDB keeps its databases — one JSON file each. With none, they live in memory
    /// for as long as the runtime does.
    pub fn set_storage_dir(&mut self, dir: Option<PathBuf>) {
        indexeddb::set_dir(&self.context, dir);
    }

    /// One beat of the workers this realm started: their messages delivered both ways, their
    /// timers run. Answers whether anything happened.
    pub fn run_workers(&mut self) -> bool {
        let busy = network::poll(&mut self.context, None) > 0;
        let busy = worker::run_workers(&mut self.context, None) || busy;
        self.run_microtasks();
        busy
    }

    /// [`Runtime::run_workers`], stopping at `deadline` once something has been heard: what is
    /// left is heard on a later call.
    pub fn run_workers_until(&mut self, deadline: std::time::Instant) -> bool {
        let busy = network::poll(&mut self.context, Some(deadline)) > 0;
        let busy = worker::run_workers(&mut self.context, Some(deadline)) || busy;
        self.run_microtasks();
        busy
    }

    /// A handle any thread can use to stop the script this runtime is running; see
    /// [`isolate::Isolate::interrupt`].
    pub fn interrupt_handle(&mut self) -> isolate::InterruptHandle {
        self.context.interrupt_handle()
    }

    /// Waits, up to `timeout`, until a worker busy with a message has something for this realm;
    /// answers whether one has. The window never needs it — it hears its workers on its beat —
    /// but a host on a test's clock has to let their threads catch up.
    pub fn wait_for_workers(&mut self, timeout: std::time::Duration) -> bool {
        worker::wait(&self.context, timeout)
    }

    /// What the host's clock says now.
    pub fn now(&self) -> Millis {
        (shared(&self.context).clock)()
    }

    /// Tells the runtime how to wake its host: `waker` is called, from whichever thread has it,
    /// when the network has delivered something or a worker has sent something this realm has
    /// yet to hear. With one set, [`Runtime::next_due`] answers the realm's own timers only —
    /// it no longer asks to be looked at every millisecond while a socket is open or a worker
    /// is busy — so a host can sleep until the earlier of that and a wake.
    pub fn set_waker(&mut self, waker: Option<Waker>) {
        shared(&self.context).wake.set(waker);
    }

    /// When the earliest timer is due, on the host's clock; `None` when there are none.
    pub fn next_due(&self) -> Option<Millis> {
        let state = shared(&self.context);
        let own = state
            .timers
            .borrow()
            .values()
            .map(|timer| timer.due)
            .reduce(f64::min);
        let woken = state.wake.is_set();
        // A worker's, on this realm's clock.
        let worker = worker::next_due(&self.context, woken).map(|wait| self.now() + wait);
        let own = if network::active(&self.context) && (!woken || network::queued(&self.context)) {
            Some(own.map_or(self.now() + 1., |due| due.min(self.now() + 1.)))
        } else {
            own
        };
        match (own, worker) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Fires every timer due by now, in the order they fall due, and the microtasks each one
    /// queues. A timer set by a callback here waits for the next call even if it is due at once,
    /// so a `setTimeout(f, 0)` that sets itself again cannot hold the host in this loop.
    /// Answers how many fired.
    pub fn run_due(&mut self) -> usize {
        self.run_due_until(None)
    }

    /// [`Runtime::run_due`], stopping at `deadline` once one has fired: the rest stay due, and
    /// fire on a later call — as a browser spreads a burst of tasks over its frames.
    pub fn run_due_until(&mut self, deadline: Option<std::time::Instant>) -> usize {
        network::poll(&mut self.context, deadline);
        let now = self.now();
        let state = shared(&self.context);
        let mut due: Vec<(Millis, u32)> = state
            .timers
            .borrow()
            .iter()
            .filter(|(_, timer)| timer.due <= now)
            .map(|(id, timer)| (timer.due, *id))
            .collect();
        due.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut fired = 0;
        for (_, id) in due {
            // Cleared by a callback that fired before it.
            let Some((callback, args)) = ({
                let mut timers = state.timers.borrow_mut();
                match timers.get_mut(&id) {
                    None => None,
                    Some(timer) => {
                        let fire = (timer.callback.clone(), timer.args.clone());
                        match timer.repeat {
                            Some(every) => timer.due = now + every,
                            None => {
                                timers.remove(&id);
                            }
                        }
                        Some(fire)
                    }
                }
            }) else {
                continue;
            };
            fired += 1;
            if let Err(error) = call(&mut self.context, &callback, &JsValue::undefined(), &args) {
                report(&mut self.context, &error);
            }
            self.run_microtasks();
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                break;
            }
        }
        // The IndexedDB databases changed since they were last written, if it is time.
        state.idb.borrow_mut().flush_due(now);
        fired
    }

    /// Whether the timer `id` is still set.
    pub fn has_timer(&self, id: u32) -> bool {
        shared(&self.context).timers.borrow().contains_key(&id)
    }

    /// How many timers are set.
    pub fn timer_count(&self) -> usize {
        shared(&self.context).timers.borrow().len()
    }

    /// Each pending timer as its callback's name, its source when it has none, and when it
    /// is due: for finding what a script left running.
    pub fn pending_timers(&mut self) -> Vec<String> {
        let timers: Vec<(Millis, Option<Millis>, JsValue)> = shared(&self.context)
            .timers
            .borrow()
            .values()
            .map(|t| (t.due, t.repeat, t.callback.clone()))
            .collect();
        timers
            .into_iter()
            .map(|(due, repeat, callback)| {
                let name = callback
                    .as_object()
                    .and_then(|f| f.get(js_string!("name"), &mut self.context).ok())
                    .map(|n| print(&n))
                    .unwrap_or_default();
                let name = if name.is_empty() {
                    callback
                        .as_object()
                        .and_then(|f| f.get(js_string!("toString"), &mut self.context).ok())
                        .and_then(|to| call(&mut self.context, &to, &callback, &[]).ok())
                        .map(|s| print(&s).chars().take(120).collect())
                        .unwrap_or_default()
                } else {
                    name
                };
                format!(
                    "{name} due {due}{}",
                    repeat.map(|r| format!(" every {r}")).unwrap_or_default()
                )
            })
            .collect()
    }

    /// Takes what was written to the console since the last time it was taken.
    pub fn take_console(&mut self) -> Vec<ConsoleLine> {
        std::mem::take(&mut *shared(&self.context).console.borrow_mut())
    }

    /// Runs the collector, so what nothing holds any more is gone.
    pub fn collect_garbage(&mut self) {
        boa_gc::force_collect();
    }
}

/// The runtime's shared state, from inside a native function.
pub(crate) fn shared(context: &Context) -> Rc<Shared> {
    context
        .get_data::<Rc<Shared>>()
        .cloned()
        .expect("verve installed its state")
}

/// What `require(specifier)` answers, from a host's native function; see [`Runtime::require`].
pub fn require(context: &mut Context, specifier: &str) -> JsResult<JsValue> {
    let cwd = PathBuf::from(shared(context).cwd.borrow().clone());
    modules::require(context, &cwd, specifier, None)
}

/// Clears the timer `id`, from a host's native function (`registerInterval`'s cleanup).
pub fn clear_timer(context: &Context, id: u32) {
    shared(context).timers.borrow_mut().remove(&id);
}

/// What the host's clock says now, from inside a native function.
pub fn now(context: &Context) -> Millis {
    (shared(context).clock)()
}

/// Writes a line to the console the host reads with [`Runtime::take_console`].
pub fn console_line(context: &Context, level: Level, text: String) {
    let state = shared(context);
    if let Some(profile) = state.profile.borrow().as_ref() {
        profile.console(level, &text);
        return;
    }
    if state.echo {
        eprintln!("[verve {level:?}] {text}");
    }
    state.console.borrow_mut().push(ConsoleLine { level, text });
}

/// An error nobody caught, written where an uncaught error goes: the console.
pub fn report(context: &mut Context, error: &JsError) {
    if shared(context).profile.borrow().is_some() {
        process::uncaught(context, error);
        return;
    }
    let text = describe(context, error);
    let frames = thrown_at(context, error);
    let line = if frames.is_empty() {
        format!("Uncaught {text}")
    } else {
        format!("Uncaught {text} ({frames})")
    };
    console_line(context, Level::Error, line);
}

/// Where an error was thrown: the first few frames of its stack, enough to find the line.
pub fn thrown_at(context: &mut Context, error: &JsError) -> String {
    let Ok(value) = error.clone().into_opaque(context) else {
        return String::new();
    };
    let Some(object) = value.as_object() else {
        return String::new();
    };
    let stack = object
        .get(js_string!("stack"), context)
        .ok()
        .and_then(|v| v.as_string())
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    stack
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(4)
        .map(short_frame)
        .collect::<Vec<_>>()
        .join("; ")
}

/// A frame with its file named from the plugins folder on — `calendar/main.js:6428:20` —
/// rather than from the disk's root.
fn short_frame(frame: &str) -> String {
    let marker = ["plugins\\", "plugins/"]
        .iter()
        .filter_map(|m| frame.rfind(m).map(|at| at + m.len()))
        .max();
    let Some(start) = marker else {
        return frame.to_owned();
    };
    let open = frame[..start].rfind('(').map_or(0, |at| at + 1);
    format!("{}{}", &frame[..open], frame[start..].replace('\\', "/"))
}

/// An error as one line: `TypeError: x is not a function`.
pub fn describe(context: &mut Context, error: &JsError) -> String {
    let value = match error.clone().into_opaque(context) {
        Ok(value) => value,
        Err(_) => return error.to_string(),
    };
    if let Some(object) = value.as_object() {
        let name = object
            .get(js_string!("name"), context)
            .ok()
            .and_then(|v| v.as_string())
            .map(|s| s.to_std_string_escaped());
        let message = object
            .get(js_string!("message"), context)
            .ok()
            .and_then(|v| v.as_string())
            .map(|s| s.to_std_string_escaped());
        if let (Some(name), Some(message)) = (name, message) {
            return format!("{name}: {message}");
        }
    }
    value.display().to_string()
}

/// Calls `function` if it is callable, and throws the TypeError JavaScript would if not.
pub fn call(
    context: &mut Context,
    function: &JsValue,
    this: &JsValue,
    args: &[JsValue],
) -> JsResult<JsValue> {
    match function.as_callable() {
        Some(callable) => callable.call(this, args, context),
        None => Err(JsNativeError::typ()
            .with_message(format!("{} is not a function", function.display()))
            .into()),
    }
}

/// A value as `console.log` prints it: strings as they are, everything else as JavaScript
/// would show it.
pub fn print(value: &JsValue) -> String {
    match value.as_string() {
        Some(text) => text.to_std_string_escaped(),
        None => value.display().to_string(),
    }
}

fn install_globals(context: &mut Context) -> JsResult<()> {
    let global = context.global_object();
    context.register_global_property(js_string!("global"), global.clone(), Attribute::all())?;

    // Timers.
    let functions: [(
        &str,
        usize,
        boa_engine::native_function::NativeFunctionPointer,
    ); 6] = [
        ("setTimeout", 2, |_, args, context| {
            set_timer(args, false, context)
        }),
        ("setInterval", 2, |_, args, context| {
            set_timer(args, true, context)
        }),
        ("setImmediate", 1, |_, args, context| {
            let mut with_zero = vec![args.first().cloned().unwrap_or_default(), 0.into()];
            with_zero.extend(args.iter().skip(1).cloned());
            set_timer(&with_zero, false, context)
        }),
        ("clearTimeout", 1, clear_timer_native),
        ("clearInterval", 1, clear_timer_native),
        ("clearImmediate", 1, clear_timer_native),
    ];
    for (name, length, function) in functions {
        context.register_global_callable(
            JsString::from(name),
            length,
            NativeFunction::from_fn_ptr(function),
        )?;
    }
    context.register_global_callable(
        js_string!("queueMicrotask"),
        1,
        NativeFunction::from_fn_ptr(|_, args, context| {
            let callback = args.first().cloned().unwrap_or_default();
            if !callback.is_callable() {
                return Err(JsNativeError::typ()
                    .with_message("queueMicrotask: callback is not a function")
                    .into());
            }
            microtask(context, callback, Vec::new());
            Ok(JsValue::undefined())
        }),
    )?;

    // The console.
    let mut console = ObjectInitializer::new(context);
    let levels: [(&str, boa_engine::native_function::NativeFunctionPointer); 6] = [
        ("log", |_, args, context| log(Level::Log, args, context)),
        ("info", |_, args, context| log(Level::Info, args, context)),
        ("warn", |_, args, context| log(Level::Warn, args, context)),
        ("error", |_, args, context| log(Level::Error, args, context)),
        ("debug", |_, args, context| log(Level::Debug, args, context)),
        ("trace", |_, args, context| log(Level::Debug, args, context)),
    ];
    for (name, function) in levels {
        console.function(
            NativeFunction::from_fn_ptr(function),
            JsString::from(name),
            0,
        );
    }
    let more: [(&str, boa_engine::native_function::NativeFunctionPointer); 12] = [
        ("dir", |_, args, context| {
            log(Level::Log, &args[..args.len().min(1)], context)
        }),
        ("table", |_, args, context| log(Level::Log, args, context)),
        ("assert", |_, args, context| {
            if args.first().is_some_and(JsValue::to_boolean) {
                return Ok(JsValue::undefined());
            }
            let mut rest = vec![JsValue::from(js_string!("Assertion failed:"))];
            rest.extend(args.iter().skip(1).cloned());
            log(Level::Error, &rest, context)
        }),
        ("time", |_, args, context| {
            let label = console_label(args, context)?;
            let state = shared(context);
            let now = (state.clock)();
            state.console_timers.borrow_mut().insert(label, now);
            Ok(JsValue::undefined())
        }),
        ("timeLog", |_, args, context| {
            console_time(args, context, false)
        }),
        ("timeEnd", |_, args, context| {
            console_time(args, context, true)
        }),
        ("count", |_, args, context| {
            let label = console_label(args, context)?;
            let count = {
                let state = shared(context);
                let mut counts = state.console_counts.borrow_mut();
                let count = counts.entry(label.clone()).or_default();
                *count += 1;
                *count
            };
            console_line(context, Level::Log, format!("{label}: {count}"));
            Ok(JsValue::undefined())
        }),
        ("countReset", |_, args, context| {
            let label = console_label(args, context)?;
            shared(context).console_counts.borrow_mut().remove(&label);
            Ok(JsValue::undefined())
        }),
        ("group", |_, args, context| console_group(args, context)),
        ("groupCollapsed", |_, args, context| {
            console_group(args, context)
        }),
        ("groupEnd", |_, _, context| {
            let state = shared(context);
            state
                .console_depth
                .set(state.console_depth.get().saturating_sub(1));
            Ok(JsValue::undefined())
        }),
        ("clear", |_, _, _| Ok(JsValue::undefined())),
    ];
    for (name, function) in more {
        console.function(
            NativeFunction::from_fn_ptr(function),
            JsString::from(name),
            0,
        );
    }
    let console = console.build();
    context.register_global_property(js_string!("console"), console, Attribute::all())?;

    intl::install(context)?;
    encoding::install(context)?;
    url::install(context)?;
    url_pattern::install(context)?;
    web::install(context)?;
    crypto::install(context)?;
    streams::install(context)?;
    fetch::install(context)?;
    zlib::install(context)?;
    dom_events::install(context)?;
    messaging::install(context)?;
    navigator::install(context)?;
    worker::install(context)?;
    indexeddb::install(context)?;
    wasm::install(context)?;
    // Node's own modules, which Electron gives a desktop plugin, are made when first required;
    // `process` is a global too, and an `EventEmitter`.
    process::install(context)?;
    Ok(())
}

pub(crate) fn microtask(context: &mut Context, callback: JsValue, args: Vec<JsValue>) {
    context.enqueue_job(
        PromiseJob::new(move |context| {
            call(context, &callback, &JsValue::undefined(), &args)?;
            Ok(JsValue::undefined())
        })
        .into(),
    );
}

/// A console line, written as Node's console writes it (`util.format`): `%s` and the rest
/// filled in, objects as `util.inspect` shows them. A value whose own inspect function throws
/// is written plainly rather than lose the line.
fn log(level: Level, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = match util::console_text(args, context) {
        Ok(text) => text,
        Err(_) => args.iter().map(print).collect::<Vec<_>>().join(" "),
    };
    let indent = "  ".repeat(shared(context).console_depth.get());
    let text = if indent.is_empty() {
        text
    } else {
        text.replace('\n', &format!("\n{indent}"))
    };
    console_line(context, level, format!("{indent}{text}"));
    Ok(JsValue::undefined())
}

/// `console.time`'s and `console.count`'s label: `default` when none is given.
fn console_label(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    match args.first() {
        None => Ok("default".to_owned()),
        Some(v) if v.is_undefined() => Ok("default".to_owned()),
        Some(v) => Ok(v.to_string(context)?.to_std_string_escaped()),
    }
}

/// `timeLog` and `timeEnd`: how long since the label's `time`, written as Chrome writes it.
fn console_time(args: &[JsValue], context: &mut Context, end: bool) -> JsResult<JsValue> {
    let label = console_label(args, context)?;
    let started = {
        let state = shared(context);
        let mut timers = state.console_timers.borrow_mut();
        if end {
            timers.remove(&label)
        } else {
            timers.get(&label).copied()
        }
    };
    let Some(started) = started else {
        console_line(
            context,
            Level::Warn,
            format!("Timer '{label}' does not exist"),
        );
        return Ok(JsValue::undefined());
    };
    let elapsed = (shared(context).clock)() - started;
    let mut text = format!("{label}: {elapsed:.3} ms");
    for extra in args.iter().skip(1) {
        text.push(' ');
        text.push_str(&print(extra));
    }
    console_line(context, Level::Log, text);
    Ok(JsValue::undefined())
}

fn console_group(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !args.is_empty() {
        log(Level::Log, args, context)?;
    }
    let state = shared(context);
    state.console_depth.set(state.console_depth.get() + 1);
    Ok(JsValue::undefined())
}

fn set_timer(args: &[JsValue], repeat: bool, context: &mut Context) -> JsResult<JsValue> {
    let callback = args.first().cloned().unwrap_or_default();
    if !callback.is_callable() {
        return Err(JsNativeError::typ()
            .with_message("The \"callback\" argument must be of type function")
            .into());
    }
    let delay = match args.get(1) {
        Some(value) => value.to_number(context)?,
        None => 0.,
    };
    // As browsers and Node both do: a delay that is not a positive number is no delay, and an
    // interval of no delay is one millisecond, or it would never let anything else run.
    // Run, when it fires, with the `AsyncLocalStorage` stores set as it was started.
    let callback = hooks::carry_callback(callback, context);
    let id = set_timeout(
        context,
        callback,
        delay,
        repeat,
        args.iter().skip(2).cloned().collect(),
    );
    Ok(JsValue::from(id))
}

/// Starts a timer from a host's native function, as `setTimeout` (or, with `repeat`,
/// `setInterval`) would: `callback` is called with `args` after `delay` milliseconds on the host's
/// clock. Answers the timer's id, which `clearTimeout` takes.
pub fn set_timeout(
    context: &Context,
    callback: JsValue,
    delay: Millis,
    repeat: bool,
    args: Vec<JsValue>,
) -> u32 {
    let delay = if delay.is_finite() && delay > 0. {
        delay
    } else {
        0.
    };
    let state = shared(context);
    let id = state.next_timer.get();
    state.next_timer.set(id.wrapping_add(1).max(1));
    let now = (state.clock)();
    state.timers.borrow_mut().insert(
        id,
        Timer {
            due: now + delay,
            repeat: repeat.then_some(delay.max(1.)),
            callback,
            args,
            refed: true,
        },
    );
    id
}

/// Whether the timer `id` keeps a process waiting for it; see `Timeout.unref()`.
pub(crate) fn ref_timer(context: &Context, id: u32, refed: bool) {
    if let Some(timer) = shared(context).timers.borrow_mut().get_mut(&id) {
        timer.refed = refed;
    }
}

/// Whether any timer keeps a process waiting for it.
pub(crate) fn has_ref_timers(context: &Context) -> bool {
    shared(context).timers.borrow().values().any(|t| t.refed)
}

fn clear_timer_native(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let given = args.first().cloned().unwrap_or_default();
    // A number, or a `Timeout` from `require('timers')`, which turns into its number.
    let id = if given.is_object() {
        given.to_number(context)?
    } else {
        given.as_number().unwrap_or(f64::NAN)
    };
    if id.is_finite() {
        clear_timer(context, id as u32);
    }
    Ok(JsValue::undefined())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_clock() -> (Rc<Cell<f64>>, Runtime) {
        let time = Rc::new(Cell::new(0.));
        let read = Rc::clone(&time);
        (time, Runtime::with_clock(move || read.get()))
    }

    #[test]
    fn a_module_exports_through_module_exports_and_reads_a_builtin() {
        let (_, mut rt) = fake_clock();
        let builtin = rt.eval("({ greeting: 'hi' })").unwrap();
        rt.define_builtin("greeter", builtin);
        let exports = rt
            .load_module(
                Path::new("/plugins/a/main.js"),
                "var g = require('greeter'); module.exports = { said: g.greeting + ' ' + typeof __dirname };",
            )
            .unwrap();
        let said = exports
            .as_object()
            .unwrap()
            .get(js_string!("said"), rt.context())
            .unwrap();
        assert_eq!(print(&said), "hi string");
        let missing = rt.load_module(Path::new("/plugins/b/main.js"), "require('nope')");
        assert_eq!(missing.unwrap_err(), "Error: Cannot find module 'nope'");
    }

    #[test]
    fn timers_fire_on_the_hosts_clock_and_clear() {
        let (time, mut rt) = fake_clock();
        rt.eval(
            "var log = []; var t = setTimeout(() => log.push('once'), 100);
             var i = setInterval(() => log.push('tick'), 50);
             setTimeout(() => log.push('never'), 10); clearTimeout(t + 2);",
        )
        .unwrap();
        assert_eq!(rt.next_due(), Some(50.));
        time.set(60.);
        assert_eq!(rt.run_due(), 1);
        time.set(120.);
        rt.run_due();
        rt.eval("clearInterval(i)").unwrap();
        time.set(1000.);
        rt.run_due();
        assert_eq!(print(&rt.eval("log.join(',')").unwrap()), "tick,once,tick");
        assert_eq!(rt.timer_count(), 0);
    }

    #[test]
    fn async_functions_settle_before_the_host_looks_again() {
        let (_, mut rt) = fake_clock();
        rt.eval(
            "var done = []; (async () => { await null; done.push(1); await Promise.resolve(); done.push(2); })();
             queueMicrotask(() => done.push('m')); process.nextTick(x => done.push(x), 't');",
        )
        .unwrap();
        assert_eq!(print(&rt.eval("done.join(',')").unwrap()), "1,m,t,2");
    }

    #[test]
    fn the_console_is_kept_for_the_host() {
        let (_, mut rt) = fake_clock();
        rt.eval("console.log('a', 1, {b: 2}); console.error('bad')")
            .unwrap();
        let lines = rt.take_console();
        assert_eq!(lines[0].level, Level::Log);
        assert!(lines[0].text.starts_with("a 1 "));
        assert_eq!(
            lines[1],
            ConsoleLine {
                level: Level::Error,
                text: "bad".into()
            }
        );
    }

    #[test]
    fn an_error_in_a_timer_is_reported_and_the_loop_goes_on() {
        let (time, mut rt) = fake_clock();
        rt.eval("var n = 0; setTimeout(() => { throw new TypeError('x') }, 1); setTimeout(() => n++, 2);")
            .unwrap();
        time.set(5.);
        assert_eq!(rt.run_due(), 2);
        assert_eq!(print(&rt.eval("n").unwrap()), "1");
        assert!(rt.take_console()[0].text.contains("TypeError: x"));
    }
}

/// `VERVE_JIT=0` (also `off`, `false`, `no`) switches the JIT off, and `VERVE_JIT_THRESHOLD=<n>`
/// sets how hot a function gets before it is compiled (0: before it first runs). Boa's own
/// `BOA_JIT` and `BOA_JIT_THRESHOLD` apply first.
#[cfg(feature = "jit")]
fn jit_from_env(context: &mut Context) {
    let options = context.jit_options_mut();
    if let Ok(value) = std::env::var("VERVE_JIT") {
        options.enabled = !matches!(value.trim(), "0" | "off" | "false" | "no");
    }
    if let Some(threshold) = std::env::var("VERVE_JIT_THRESHOLD")
        .ok()
        .and_then(|value| value.trim().parse().ok())
    {
        options.threshold = threshold;
    }
}
