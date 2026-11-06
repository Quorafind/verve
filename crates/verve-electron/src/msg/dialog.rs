//! The file and message dialogs the main process asks the shell for, and their answers.

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;

/// A file dialog's filter: `{ name, extensions }`, where `['*']` is every file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// A dialog, as Electron's options say it.
#[derive(Clone, Debug)]
pub enum DialogSpec {
    Open {
        title: String,
        default_path: String,
        button_label: String,
        message: String,
        filters: Vec<FileFilter>,
        properties: OpenProps,
    },
    Save {
        title: String,
        default_path: String,
        button_label: String,
        message: String,
        filters: Vec<FileFilter>,
        name_field_label: String,
        shows_tag_field: bool,
        properties: SaveProps,
    },
    MessageBox {
        kind: MessageKind,
        buttons: Vec<String>,
        /// `-1` when the app named none.
        default_id: i32,
        cancel_id: usize,
        title: String,
        message: String,
        detail: String,
        checkbox_label: String,
        checkbox_checked: bool,
        icon: Option<PathBuf>,
        no_link: bool,
    },
}

/// `showOpenDialog`'s `properties`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenProps {
    pub open_file: bool,
    pub open_directory: bool,
    pub multi_selections: bool,
    pub show_hidden_files: bool,
    pub create_directory: bool,
    pub prompt_to_create: bool,
    pub no_resolve_aliases: bool,
    pub treat_package_as_directory: bool,
    pub dont_add_to_recent: bool,
}

/// `showSaveDialog`'s `properties`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveProps {
    pub show_hidden_files: bool,
    pub create_directory: bool,
    pub treat_package_as_directory: bool,
    pub show_overwrite_confirmation: bool,
    pub dont_add_to_recent: bool,
}

/// A message box's `type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessageKind {
    #[default]
    None,
    Info,
    Warning,
    Error,
    Question,
}

/// Where a dialog's answer goes.
#[derive(Clone, Debug)]
pub enum DialogReply {
    /// A [`super::FromShell::DialogResult`] for the main process to settle a promise with.
    Async,
    /// The main process's thread waits on this channel (`show*Sync`): the only place it waits for
    /// the shell, and the shell never waits for it.
    Sync(SyncSender<DialogResult>),
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResult {
    Open {
        canceled: bool,
        file_paths: Vec<PathBuf>,
    },
    Save {
        canceled: bool,
        file_path: Option<PathBuf>,
    },
    MessageBox {
        response: usize,
        checkbox_checked: bool,
    },
    /// The parent window went, or the app is ending: a cancel.
    Closed,
}
