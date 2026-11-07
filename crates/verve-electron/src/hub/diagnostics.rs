//! What the Hub says on the terminal: the app's [`Log`], and which pages' consoles go to it.

use std::fmt::{self, Debug, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use verve::Level;
use verve::clone::Cloned;

use super::{ContentsId, Hub, lock};
use crate::log::{self, Log, diag};
use crate::msg::{FromRenderer, FromShell, ToRenderer, ToRendererInput, ToShell};

/// The longest a message is written in full on a diagnostic line.
const BRIEF: usize = 200;

/// A writer that takes `BRIEF` characters and refuses the rest, which stops `Debug` there.
struct Brief {
    text: String,
    left: usize,
}

impl fmt::Write for Brief {
    fn write_str(&mut self, part: &str) -> fmt::Result {
        for character in part.chars() {
            if self.left == 0 {
                self.text.push('…');
                return Err(fmt::Error);
            }
            self.left -= 1;
            self.text.push(character);
        }
        Ok(())
    }
}

/// `value` as `Debug` shows it, cut short: a diagnostic line is not the place for a window's icon.
fn brief(value: &dyn Debug) -> String {
    let mut out = Brief {
        text: String::new(),
        left: BRIEF,
    };
    // An error here is the cut itself.
    let _ = write!(out, "{value:?}");
    out.text
}

/// How many arguments a message carries: they travel as the one array [`ipc::pack`] made.
///
/// [`ipc::pack`]: crate::ipc
fn count(args: &[Cloned]) -> usize {
    args.first().and_then(Cloned::array_len).unwrap_or(0)
}

impl Hub {
    /// The terminal output of this app.
    pub fn log(&self) -> &Arc<Log> {
        self.log.get_or_init(|| Arc::clone(log::process_log()))
    }

    /// Gives the app a log of its own, which a test reads. Before anything else uses the Hub: a
    /// log that has been used stays.
    pub fn set_log(&self, log: Arc<Log>) {
        // A second answer is the one the first user already got.
        let _ = self.log.set(log);
    }

    /// Page `contents` has its DevTools open, or has not: its console goes to the terminal while
    /// it does.
    pub(crate) fn set_devtools_open(&self, contents: ContentsId, open: bool) {
        let mut pages = lock(&self.devtools);
        if open {
            pages.insert(contents);
        } else {
            pages.remove(&contents);
        }
        self.any_devtools
            .store(!pages.is_empty(), Ordering::Relaxed);
    }

    /// Whether page `contents` has its DevTools open.
    pub(crate) fn is_devtools_open(&self, contents: ContentsId) -> bool {
        lock(&self.devtools).contains(&contents)
    }

    /// Writes a line of page `contents`' console to the terminal, if logging is on or its
    /// DevTools are open; Electron writes nothing otherwise.
    pub(super) fn forward_console(
        &self,
        contents: ContentsId,
        level: Level,
        at: (&str, u32),
        text: &str,
    ) {
        let log = self.log();
        let devtools = self.any_devtools.load(Ordering::Relaxed) && self.is_devtools_open(contents);
        if log.console() || devtools {
            log.console_line(contents, level, at, text);
        }
    }

    /// Says what a page told the main process, in the `ipc`, `protocol` and `load` categories.
    pub(super) fn log_from_renderer(&self, contents: ContentsId, event: &FromRenderer) {
        let log = self.log();
        match event {
            FromRenderer::Ipc { channel, args } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "renderer {contents} -> main send {channel:?} ({} args)",
                    count(args)
                );
            }
            FromRenderer::Invoke {
                call,
                channel,
                args,
            } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "renderer {contents} -> main invoke #{call} {channel:?} ({} args)",
                    count(args)
                );
            }
            FromRenderer::SendSync { channel, args, .. } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "renderer {contents} -> main sendSync {channel:?} ({} args)",
                    count(args)
                );
            }
            FromRenderer::SendTo {
                target, channel, ..
            } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "renderer {contents} -> renderer {target} sendTo {channel:?}"
                );
            }
            FromRenderer::Post { channel, ports, .. } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "renderer {contents} -> main postMessage {channel:?} ({} ports)",
                    ports.len()
                );
            }
            FromRenderer::ProtocolRequest { call, request } => {
                diag!(
                    log,
                    Protocol,
                    Info,
                    "renderer {contents} asks #{call} {}",
                    brief(request)
                );
            }
            FromRenderer::Reload { url } => {
                diag!(log, Load, Info, "renderer {contents} reloads {url}");
            }
            _ => {}
        }
    }

    /// Says what the main process asked of a page, in the `ipc`, `protocol` and `load` categories.
    pub(super) fn log_to_renderer(&self, contents: ContentsId, msg: &ToRenderer) {
        let log = self.log();
        match msg {
            ToRenderer::Ipc {
                channel,
                args,
                sender_id,
            } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "{} -> renderer {contents} send {channel:?} ({} args)",
                    sender_name(*sender_id),
                    count(args)
                );
            }
            ToRenderer::InvokeReply { call, result } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "main -> renderer {contents} invoke #{call} {}",
                    if result.is_ok() {
                        "resolved"
                    } else {
                        "rejected"
                    }
                );
            }
            ToRenderer::SendSyncReply { call, .. } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "main -> renderer {contents} sendSync #{call} reply"
                );
            }
            ToRenderer::Post { channel, .. } => {
                diag!(
                    log,
                    Ipc,
                    Info,
                    "main -> renderer {contents} postMessage {channel:?}"
                );
            }
            ToRenderer::Load { url, .. } => {
                diag!(log, Load, Info, "renderer {contents} loads {url}");
            }
            ToRenderer::ProtocolResponse { call, response } => {
                diag!(
                    log,
                    Protocol,
                    Info,
                    "main answers #{call} {}",
                    brief(response)
                );
            }
            _ => {}
        }
    }

    /// Says what the main process asked of the shell.
    pub(super) fn log_to_shell(&self, msg: &ToShell) {
        diag!(self.log(), Shell, Info, "main -> shell {}", brief(msg));
    }

    /// Says what the shell reported: its windows' events in the `lifecycle` category, all of it in
    /// `shell`.
    pub(super) fn log_from_shell(&self, msg: &FromShell) {
        let log = self.log();
        diag!(log, Shell, Info, "shell -> main {}", brief(msg));
        if let FromShell::Window { window, event } = msg {
            diag!(log, Lifecycle, Info, "window {window}: {}", brief(event));
        }
    }

    /// Says what a person did to page `contents`.
    pub(super) fn log_input(&self, contents: ContentsId, msg: &ToRendererInput) {
        diag!(self.log(), Input, Info, "page {contents} {}", brief(msg));
    }
}

/// Who sent a message to a page: `0` is the main process.
fn sender_name(sender: ContentsId) -> String {
    if sender == 0 {
        "main".to_owned()
    } else {
        format!("renderer {sender}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_value_is_cut_at_the_limit_and_a_short_one_is_whole() {
        assert_eq!(brief(&"short"), "\"short\"");
        let long = "x".repeat(1000);
        let cut = brief(&long);
        assert_eq!(cut.chars().count(), BRIEF + 1);
        assert!(cut.starts_with("\"xxx") && cut.ends_with('…'));
    }
}
