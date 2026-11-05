//! The shell: the OS's thread, the app's windows, and the wake that tells it there is news.
//!
//! The Hub is what the app's JavaScript threads use; this side owns none of them. A message
//! lands in a queue from whichever thread asked for it, the queue's owner is woken through a
//! channel of its own, and the OS thread turns what it finds into GPUI windows — so a script
//! that never returns holds its own thread and nothing else.
//!
//! P02 draws a page's text nodes and nothing else; P30 replaces [`DomView`]'s drawing with the
//! display list `verve-layout` makes on the page's own thread, and P34 fills in the rest of the
//! window operations.

mod dom_view;
mod shell;

use std::sync::Arc;

use gpui::{App, AppContext as _, QuitMode};
use verve_electron::Hub;
use verve_electron::hub::Program;
use verve_electron::hub::ShellPort;
use verve_electron::msg::ToShell;

use crate::shell::{Messages, Shell};

/// Runs an app: the OS thread's GPUI application, the windows the main process asks it for, and
/// the app's script — which starts once the shell is listening. Answers the code the process
/// leaves with.
pub fn run(hub: Arc<Hub>, program: Program) -> i32 {
    let (wake, woken) = async_channel::bounded(1);
    let messages = Messages::default();
    hub.set_shell(Arc::new(Port {
        messages: messages.clone(),
        wake,
    }));
    let app = Arc::clone(&hub);
    // The app ends when the main process says so (`ToShell::Quit`, or the Hub's exit code), never
    // because the last window closed: GPUI's default quits then on Windows and Linux, and that
    // would end an app whose `'closed'` listener is about to open another window, with code 0.
    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            let shell = cx.new(|_| Shell::new(Arc::clone(&app), messages.clone()));
            let pump = shell.clone();
            cx.spawn(async move |cx| {
                while woken.recv().await.is_ok() {
                    pump.update(cx, |shell, cx| shell.process(cx));
                }
            })
            .detach();
            app.start(program);
        });
    hub.exit_code().unwrap_or(0)
}

/// The Hub's end of the shell: messages land in the shell's queue from whichever thread sent
/// them, and the OS thread is woken to take them.
struct Port {
    messages: Messages,
    wake: async_channel::Sender<()>,
}

impl ShellPort for Port {
    fn send(&self, msg: ToShell) {
        self.messages.push(msg);
    }

    fn wake(&self) {
        // One pending wake is all the pump needs: it takes everything queued when it runs, so a
        // wake that finds the slot full has been coalesced into the one waiting.
        let _ = self.wake.try_send(());
    }
}
