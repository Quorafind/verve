//! Dialogs: what the main process asks a person, shown by GPUI's own platform prompts so no
//! dialog library is linked, and answered without ever waiting on the OS thread's loop. A prompt
//! hands back a future; a task awaits it and says the answer to the main process, as
//! [`FromShell::DialogResult`] for a promise or on the channel a synchronous call waits on.
//!
//! GPUI's prompts do not take everything Electron's options do: a message box has no title or
//! checkbox and no icon, a file dialog no filters, title or start folder (the start folder of a
//! save dialog and the suggested name are used). A message box needs a window to show over, the
//! dialog's `parent` or else any window the shell has; with none, the dialog is answered as
//! closed, which the main process takes for a cancel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AsyncApp, PathPromptOptions, PromptButton, PromptLevel};
use verve_electron::Hub;
use verve_electron::msg::{
    DialogReply, DialogResult, DialogSpec, FromShell, MessageKind, OpenProps,
};

use crate::window::Window;

/// Where a dialog's answer goes, and what it is called there.
struct Answer {
    hub: Arc<Hub>,
    call: u64,
    reply: DialogReply,
}

impl Answer {
    fn say(self, result: DialogResult) {
        match self.reply {
            DialogReply::Sync(channel) => {
                // A main process that stopped waiting has nothing left to hear.
                let _ = channel.send(result);
            }
            DialogReply::Async => self.hub.from_shell(FromShell::DialogResult {
                call: self.call,
                result,
            }),
        }
    }
}

/// Answers `call` as closed, for a dialog whose parent window is gone.
pub(crate) fn closed(hub: &Arc<Hub>, call: u64, reply: DialogReply) {
    let answer = Answer {
        hub: Arc::clone(hub),
        call,
        reply,
    };
    answer.say(DialogResult::Closed);
}

/// Shows `spec` over `window`, and answers `call` when the person has.
pub(crate) fn show(
    hub: &Arc<Hub>,
    call: u64,
    window: Option<&Window>,
    spec: DialogSpec,
    reply: DialogReply,
    cx: &mut AsyncApp,
) {
    let answer = Answer {
        hub: Arc::clone(hub),
        call,
        reply,
    };
    match spec {
        DialogSpec::Open {
            properties,
            button_label,
            ..
        } => open(&properties, button_label, answer, cx),
        DialogSpec::Save { default_path, .. } => save(&default_path, answer, cx),
        DialogSpec::MessageBox { .. } => message_box(spec, window, answer, cx),
    }
}

/// `showErrorBox`: a box with one button, over any window; with none, [`Hub::error_box`] has already
/// shown the system's own. With `VERVE_NO_SYSTEM_DIALOGS` it shows nothing and writes the box to
/// standard error, as the headless shell does.
pub(crate) fn error_box(title: &str, content: &str, window: Option<&Window>, cx: &mut AsyncApp) {
    if !crate::native::touches_desktop() {
        eprintln!("[app] error box {title:?} {content:?}");
        return;
    }
    let Some(window) = window else {
        return;
    };
    let message = format!("{title}\n{content}");
    // The answer is only that the person read it.
    if let Some(done) = window.prompt(PromptLevel::Critical, &message, None, &[ok()], cx) {
        cx.spawn(async move |_| {
            let _ = done.await;
        })
        .detach();
    }
}

fn ok() -> PromptButton {
    PromptButton::ok("OK")
}

fn open(properties: &OpenProps, label: String, answer: Answer, cx: &mut AsyncApp) {
    let options = PathPromptOptions {
        files: properties.open_file || !properties.open_directory,
        directories: properties.open_directory,
        multiple: properties.multi_selections,
        prompt: (!label.is_empty()).then(|| label.into()),
    };
    let chosen = cx.update(|app| app.prompt_for_paths(options));
    cx.spawn(async move |_| {
        let result = match chosen.await {
            Ok(Ok(Some(paths))) => DialogResult::Open {
                canceled: false,
                file_paths: paths,
            },
            Ok(_) => DialogResult::Open {
                canceled: true,
                file_paths: Vec::new(),
            },
            Err(_) => DialogResult::Closed,
        };
        answer.say(result);
    })
    .detach();
}

fn save(default_path: &str, answer: Answer, cx: &mut AsyncApp) {
    let (folder, name) = split_default(default_path);
    let chosen = cx.update(|app| app.prompt_for_new_path(&folder, name.as_deref()));
    cx.spawn(async move |_| {
        let result = match chosen.await {
            Ok(Ok(Some(path))) => DialogResult::Save {
                canceled: false,
                file_path: Some(path),
            },
            Ok(_) => DialogResult::Save {
                canceled: true,
                file_path: None,
            },
            Err(_) => DialogResult::Closed,
        };
        answer.say(result);
    })
    .detach();
}

/// `defaultPath` as a folder to start in and the file name to suggest: a path to a folder is
/// only the folder.
fn split_default(default_path: &str) -> (PathBuf, Option<String>) {
    let path = Path::new(default_path);
    if default_path.is_empty() {
        return (std::env::current_dir().unwrap_or_default(), None);
    }
    if path.is_dir() {
        return (path.to_path_buf(), None);
    }
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    let folder = folder.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        Path::to_path_buf,
    );
    (
        folder,
        path.file_name().map(|n| n.to_string_lossy().into_owned()),
    )
}

fn message_box(spec: DialogSpec, window: Option<&Window>, answer: Answer, cx: &mut AsyncApp) {
    let DialogSpec::MessageBox {
        kind,
        buttons,
        cancel_id,
        message,
        detail,
        checkbox_checked,
        title,
        ..
    } = spec
    else {
        return;
    };
    let level = match kind {
        MessageKind::Warning => PromptLevel::Warning,
        MessageKind::Error => PromptLevel::Critical,
        MessageKind::None | MessageKind::Info | MessageKind::Question => PromptLevel::Info,
    };
    let labels = if buttons.is_empty() {
        vec!["OK".to_owned()]
    } else {
        buttons
    };
    let answers: Vec<PromptButton> = labels
        .iter()
        .enumerate()
        .map(|(at, label)| {
            if at == cancel_id && labels.len() > 1 {
                PromptButton::cancel(label.clone())
            } else {
                PromptButton::new(label.clone())
            }
        })
        .collect();
    let text = if message.is_empty() { title } else { message };
    let detail = (!detail.is_empty()).then_some(detail);
    let asked = window.and_then(|w| w.prompt(level, &text, detail.as_deref(), &answers, cx));
    let Some(chosen) = asked else {
        answer.say(DialogResult::Closed);
        return;
    };
    cx.spawn(async move |_| {
        let result = match chosen.await {
            Some(response) => DialogResult::MessageBox {
                response,
                // The prompt has no checkbox to change it.
                checkbox_checked,
            },
            None => DialogResult::Closed,
        };
        answer.say(result);
    })
    .detach();
}
