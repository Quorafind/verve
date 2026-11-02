//! `fs`'s file descriptors: the number `openSync` answers stands for a file kept open here, and
//! `readSync`, `writeSync`, `fstatSync` and `closeSync` take it. A file only an overlay holds is
//! its bytes kept here for as long as the descriptor is — a packed file has no descriptor of the
//! system's to read — and everything else is the disk's own.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use boa_engine::{Context, JsResult, JsValue};

use crate::native::coded_error;
use crate::vfs;

use super::{fs_error, io_error, sys};

/// An open file: the disk's, or an overlay's bytes kept here.
pub(crate) enum Handle {
    Disk(File),
    Overlay {
        bytes: Vec<u8>,
        position: u64,
        meta: vfs::Metadata,
    },
}

impl Handle {
    pub(super) fn read_into(
        &mut self,
        into: &mut [u8],
        position: Option<u64>,
    ) -> std::io::Result<usize> {
        match self {
            Self::Disk(file) => read_at(file, into, position),
            Self::Overlay {
                bytes,
                position: at,
                ..
            } => {
                let start = (position.unwrap_or(*at) as usize).min(bytes.len());
                let end = (start + into.len()).min(bytes.len());
                into[..end - start].copy_from_slice(&bytes[start..end]);
                if position.is_none() {
                    *at = end as u64;
                }
                Ok(end - start)
            }
        }
    }

    /// Writes `bytes` at `position`, or where the file is; answers how many were written.
    pub(super) fn write_at(
        &mut self,
        bytes: &[u8],
        position: Option<u64>,
    ) -> std::io::Result<usize> {
        match self {
            Self::Disk(file) => write_at(file, bytes, position),
            // An overlay's files are read-only: `Overlay::write_denied` refuses the write before
            // a descriptor to one is opened.
            Self::Overlay { .. } => Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
        }
    }

    pub(super) fn read_to_end(&mut self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Disk(file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok(bytes)
            }
            Self::Overlay {
                bytes, position, ..
            } => {
                let start = (*position as usize).min(bytes.len());
                *position = bytes.len() as u64;
                Ok(bytes[start..].to_vec())
            }
        }
    }

    pub(super) fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        match self {
            Self::Disk(file) => file.set_len(len),
            Self::Overlay { .. } => Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
        }
    }

    pub(super) fn sync(&mut self) -> std::io::Result<()> {
        match self {
            Self::Disk(file) => file.sync_all(),
            Self::Overlay { .. } => Ok(()),
        }
    }

    pub(super) fn sync_data(&mut self) -> std::io::Result<()> {
        match self {
            Self::Disk(file) => file.sync_data(),
            Self::Overlay { .. } => Ok(()),
        }
    }

    pub(super) fn metadata(&self) -> std::io::Result<sys::Meta> {
        Ok(match self {
            Self::Disk(file) => sys::Meta::Disk(file.metadata()?),
            Self::Overlay { meta, .. } => sys::Meta::Overlay(*meta),
        })
    }

    /// Whether this is an overlay's file, which the disk knows nothing about.
    pub(super) fn is_overlay(&self) -> bool {
        matches!(self, Self::Overlay { .. })
    }
}

/// A file `openSync` opened, kept until `closeSync`.
pub(crate) struct OpenFile {
    pub(super) handle: Handle,
    pub(super) path: String,
}

/// The realm's open files, by the number `openSync` answered.
#[derive(Default)]
struct Descriptors {
    next: Cell<i32>,
    open: RefCell<HashMap<i32, OpenFile>>,
}

fn descriptors(context: &mut Context) -> Rc<Descriptors> {
    if let Some(table) = context.get_data::<Rc<Descriptors>>() {
        return table.clone();
    }
    let table = Rc::new(Descriptors {
        next: Cell::new(20),
        open: RefCell::new(HashMap::new()),
    });
    context.insert_data(table.clone());
    table
}

pub(super) fn fd_of(value: &JsValue, context: &mut Context) -> JsResult<i32> {
    match value.as_number() {
        Some(n) if n.fract() == 0. && n >= 0. => Ok(n as i32),
        _ => Err(coded_error(
            context,
            &format!(
                "The \"fd\" argument must be of type number. Received {}",
                value.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        )),
    }
}

/// Runs `f` with the open file `fd`, or throws `EBADF`.
pub(super) fn with_fd<R>(
    context: &mut Context,
    fd: i32,
    syscall: &str,
    f: impl FnOnce(&mut OpenFile) -> std::io::Result<R>,
) -> JsResult<R> {
    let table = descriptors(context);
    let mut open = table.open.borrow_mut();
    let Some(file) = open.get_mut(&fd) else {
        drop(open);
        return Err(fs_error(context, "EBADF", syscall, None, None));
    };
    let path = file.path.clone();
    let outcome = f(file);
    drop(open);
    let view = crate::vfs::View::disk();
    outcome.map_err(|e| io_error(&view, context, e, syscall, &path, None))
}

/// Keeps `handle` under a number of its own, and answers it.
pub(super) fn take(context: &mut Context, handle: Handle, path: String) -> i32 {
    let table = descriptors(context);
    let fd = table.next.get();
    table.next.set(fd + 1);
    table
        .open
        .borrow_mut()
        .insert(fd, OpenFile { handle, path });
    fd
}

/// Closes `fd`, or throws `EBADF` when nothing is open under it.
pub(super) fn close(context: &mut Context, fd: i32) -> JsResult<()> {
    if descriptors(context).open.borrow_mut().remove(&fd).is_none() {
        return Err(fs_error(context, "EBADF", "close", None, None));
    }
    Ok(())
}

/// Writes to the console what is written to descriptor 1 or 2.
pub(super) fn console_write(fd: i32, bytes: &[u8], context: &mut Context) {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let level = if fd == 2 {
        crate::Level::Error
    } else {
        crate::Level::Log
    };
    crate::console_line(context, level, text.to_owned());
}

/// Reads at `position` (keeping where the file was), or from where it is.
fn read_at(file: &mut File, into: &mut [u8], position: Option<u64>) -> std::io::Result<usize> {
    let Some(position) = position else {
        return read_full(file, into);
    };
    let was = file.stream_position()?;
    file.seek(SeekFrom::Start(position))?;
    let read = read_full(file, into);
    file.seek(SeekFrom::Start(was))?;
    read
}

fn read_full(file: &mut File, into: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < into.len() {
        match file.read(&mut into[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

fn write_at(file: &mut File, bytes: &[u8], position: Option<u64>) -> std::io::Result<usize> {
    let Some(position) = position else {
        file.write_all(bytes)?;
        return Ok(bytes.len());
    };
    let was = file.stream_position()?;
    file.seek(SeekFrom::Start(position))?;
    let written = file.write_all(bytes);
    file.seek(SeekFrom::Start(was))?;
    written.map(|()| bytes.len())
}
