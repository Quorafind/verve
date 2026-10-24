//! Node's own modules, as Electron gives them to a desktop plugin: the table `require` reads
//! them from. Each is made, natively, the first time it is required, and kept; each answers to
//! its bare name and to `node:` before it (see `crate::modules`).
//!
//! What would reach outside the runtime is there to be required and refuses when used, as Node
//! does when the system says no: `child_process` starts no program, and `net`, `http` and the
//! rest connect to nothing and listen on nothing — plugins ask the network through
//! `requestUrl`. `fs` reads and writes the disk as Node's does: a desktop Obsidian plugin may,
//! and some do.

/// `process.platform` and `os.platform()`: Node's name for the system.
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// Node's modules, by the names `require` answers them to — each made the first time it is
/// required, and kept.
pub(crate) const MODULES: &[(&str, crate::ModuleMaker)] = &[
    ("path", crate::path::module),
    ("path/posix", crate::path::posix_module),
    ("path/win32", crate::path::win32_module),
    ("util", crate::util::util_module),
    ("util/types", crate::util::types_module),
    ("sys", |cx, _| crate::modules::node_module(cx, "util")),
    ("assert", crate::assert::assert_module),
    ("assert/strict", crate::assert::strict_module),
    ("buffer", crate::buffer::module),
    ("events", crate::events::events_module),
    ("os", crate::os::module),
    ("constants", crate::os::constants_module),
    ("url", crate::node_url::module),
    ("querystring", crate::querystring::querystring_module),
    ("punycode", crate::querystring::punycode_module),
    ("fs", crate::fs::module),
    ("fs/promises", crate::fs::promises_module),
    ("child_process", crate::sockets::child_process_module),
    ("http", crate::sockets::http_module),
    ("https", crate::sockets::https_module),
    ("http2", crate::sockets::http2_module),
    ("net", crate::sockets::net_module),
    ("tls", crate::sockets::tls_module),
    ("dns", crate::sockets::dns_module),
    ("dns/promises", crate::sockets::dns_promises_module),
    ("dgram", crate::sockets::dgram_module),
    ("cluster", crate::sockets::cluster_module),
    ("module", crate::modules::module_module),
    ("timers", crate::timers::module),
    ("async_hooks", crate::hooks::async_hooks_module),
    (
        "diagnostics_channel",
        crate::hooks::diagnostics_channel_module,
    ),
    ("perf_hooks", crate::hooks::perf_hooks_module),
    ("inspector", crate::hooks::inspector_module),
    ("inspector/promises", crate::hooks::inspector_module),
    ("trace_events", crate::hooks::trace_events_module),
    ("timers/promises", crate::timers::promises_module),
    ("vm", crate::vm::vm_module),
    ("v8", crate::vm::v8_module),
    ("worker_threads", crate::vm::worker_threads_module),
    ("wasi", crate::vm::wasi_module),
    ("readline", crate::readline::readline_module),
    (
        "readline/promises",
        crate::readline::readline_promises_module,
    ),
    ("tty", crate::readline::tty_module),
    ("repl", crate::readline::repl_module),
    ("console", crate::readline::console_module),
    ("process", crate::process::module),
    ("crypto", crate::crypto::module),
    ("zlib", crate::zlib::module),
    ("stream/web", crate::streams::module),
    ("stream", crate::stream::stream_module),
    ("stream/promises", crate::stream::promises_module),
    ("_stream_readable", crate::stream::legacy_module),
    ("_stream_writable", crate::stream::legacy_module),
    ("_stream_duplex", crate::stream::legacy_module),
    ("_stream_transform", crate::stream::legacy_module),
    ("_stream_passthrough", crate::stream::legacy_module),
    ("string_decoder", crate::string_decoder::module),
];
