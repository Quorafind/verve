//! A [`Tree`] as bytes and back: the format a process's IPC carries a clone in. It is this
//! runtime's own — a tag byte for each value, counts and numbers little-endian — and nothing
//! else is expected to read it.

use std::fmt;

use super::tree::{Tree, ViewKind};

const TAG_UNDEFINED: u8 = 0;
const TAG_NULL: u8 = 1;
const TAG_FALSE: u8 = 2;
const TAG_TRUE: u8 = 3;
const TAG_NUMBER: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_BIGINT: u8 = 6;
const TAG_DATE: u8 = 7;
const TAG_REGEXP: u8 = 8;
const TAG_ARRAY_BUFFER: u8 = 9;
const TAG_VIEW: u8 = 10;
const TAG_BLOB: u8 = 11;
const TAG_ARRAY: u8 = 12;
const TAG_OBJECT: u8 = 13;
const TAG_ERROR: u8 = 14;
const TAG_MAP: u8 = 15;
const TAG_SET: u8 = 16;
const TAG_SEEN: u8 = 17;

/// How deep a clone is read: far past anything a script builds on purpose, and well inside the
/// stack a thread has.
const MAX_DEPTH: usize = 512;

/// What is wrong with bytes that [`crate::clone::Cloned::from_bytes`] was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneBytesError {
    /// The bytes end at `at`, before a value does.
    Truncated {
        /// The offset the bytes ran out at.
        at: usize,
    },
    /// A tag this runtime never writes.
    UnknownTag {
        /// The tag found.
        tag: u8,
        /// Where it was found.
        at: usize,
    },
    /// A view kind this runtime never writes.
    UnknownViewKind {
        /// Where it was found.
        at: usize,
    },
    /// Text that is not UTF-8.
    NotUtf8 {
        /// Where the text ends.
        at: usize,
    },
    /// A count larger than the bytes could hold.
    TooMany {
        /// The count found.
        count: usize,
        /// How many bytes there are in all.
        len: usize,
    },
    /// A clone is whole, and bytes follow it.
    Trailing {
        /// How many bytes are left over.
        count: usize,
    },
    /// Values nested deeper than a clone is read: bytes from another process are not trusted to
    /// stop, and reading them recurses.
    TooDeep {
        /// Where the nesting passed the limit.
        at: usize,
    },
    /// A length that does not fit the four bytes the format keeps it in (writing).
    TooLarge {
        /// The length.
        len: usize,
    },
}

impl fmt::Display for CloneBytesError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { at } => write!(out, "the clone ends at byte {at}"),
            Self::UnknownTag { tag, at } => write!(out, "unknown tag {tag} at byte {at}"),
            Self::UnknownViewKind { at } => write!(out, "unknown view kind at byte {at}"),
            Self::NotUtf8 { at } => write!(out, "text at byte {at} is not UTF-8"),
            Self::TooMany { count, len } => write!(out, "{count} items in {len} bytes"),
            Self::Trailing { count } => write!(out, "{count} bytes left over"),
            Self::TooDeep { at } => write!(out, "values nested too deep at byte {at}"),
            Self::TooLarge { len } => write!(out, "{len} is too long to write"),
        }
    }
}

impl std::error::Error for CloneBytesError {}

type Read<T> = Result<T, CloneBytesError>;

impl Tree {
    /// Appends this tree to `out`, in the format [`Tree::read_from`] reads.
    pub(crate) fn write_to(&self, out: &mut Vec<u8>) -> Read<()> {
        match self {
            Self::Undefined => out.push(TAG_UNDEFINED),
            Self::Null => out.push(TAG_NULL),
            Self::Bool(false) => out.push(TAG_FALSE),
            Self::Bool(true) => out.push(TAG_TRUE),
            Self::Number(number) => write_float(out, TAG_NUMBER, *number)?,
            Self::Date(time) => write_float(out, TAG_DATE, *time)?,
            Self::String(units) => {
                out.push(TAG_STRING);
                write_units(out, units)?;
            }
            Self::BigInt(text) => {
                out.push(TAG_BIGINT);
                write_bytes(out, text.as_bytes())?;
            }
            Self::RegExp { source, flags } => {
                out.push(TAG_REGEXP);
                write_units(out, source)?;
                write_units(out, flags)?;
            }
            Self::ArrayBuffer(bytes) => {
                out.push(TAG_ARRAY_BUFFER);
                write_bytes(out, bytes)?;
            }
            Self::View { kind, bytes } => {
                out.push(TAG_VIEW);
                out.push(kind.number());
                write_bytes(out, bytes)?;
            }
            Self::Blob { .. } => self.write_blob(out)?,
            Self::Array(items) => write_list(out, TAG_ARRAY, items)?,
            Self::Set(items) => write_list(out, TAG_SET, items)?,
            Self::Object(fields) => write_fields(out, TAG_OBJECT, fields)?,
            Self::Error(fields) => write_fields(out, TAG_ERROR, fields)?,
            Self::Map(pairs) => {
                out.push(TAG_MAP);
                write_count(out, pairs.len())?;
                for (key, value) in pairs {
                    key.write_to(out)?;
                    value.write_to(out)?;
                }
            }
            Self::Seen(at) => {
                out.push(TAG_SEEN);
                write_count(out, *at)?;
            }
        }
        Ok(())
    }

    fn write_blob(&self, out: &mut Vec<u8>) -> Read<()> {
        let Self::Blob {
            bytes,
            media_type,
            name,
            modified,
        } = self
        else {
            return Ok(());
        };
        out.push(TAG_BLOB);
        write_bytes(out, bytes)?;
        write_bytes(out, media_type.as_bytes())?;
        match name {
            Some(name) => {
                out.push(1);
                write_bytes(out, name.as_bytes())?;
                out.extend_from_slice(&modified.unwrap_or_default().to_le_bytes());
            }
            None => out.push(0),
        }
        Ok(())
    }

    /// The tree `bytes` holds at `at`, which is moved past what it read.
    pub(crate) fn read_from(bytes: &[u8], at: &mut usize) -> Read<Self> {
        Self::read_nested(bytes, at, 0)
    }

    fn read_nested(bytes: &[u8], at: &mut usize, depth: usize) -> Read<Self> {
        if depth > MAX_DEPTH {
            return Err(CloneBytesError::TooDeep { at: *at });
        }
        let tag = take(bytes, at, 1)?[0];
        Ok(match tag {
            TAG_UNDEFINED => Self::Undefined,
            TAG_NULL => Self::Null,
            TAG_FALSE => Self::Bool(false),
            TAG_TRUE => Self::Bool(true),
            TAG_NUMBER => Self::Number(read_number(bytes, at)?),
            TAG_STRING => Self::String(read_units(bytes, at)?),
            TAG_BIGINT => Self::BigInt(read_text(bytes, at)?),
            TAG_DATE => Self::Date(read_number(bytes, at)?),
            TAG_REGEXP => Self::RegExp {
                source: read_units(bytes, at)?,
                flags: read_units(bytes, at)?,
            },
            TAG_ARRAY_BUFFER => Self::ArrayBuffer(read_bytes(bytes, at)?),
            TAG_VIEW => Self::View {
                kind: ViewKind::from_number(take(bytes, at, 1)?[0])
                    .ok_or(CloneBytesError::UnknownViewKind { at: *at - 1 })?,
                bytes: read_bytes(bytes, at)?,
            },
            TAG_BLOB => Self::read_blob(bytes, at)?,
            TAG_ARRAY => Self::Array(read_list(bytes, at, depth + 1)?),
            TAG_OBJECT => Self::Object(read_fields(bytes, at, depth + 1)?),
            TAG_ERROR => Self::Error(read_fields(bytes, at, depth + 1)?),
            TAG_MAP => Self::Map(read_pairs(bytes, at, depth + 1)?),
            TAG_SET => Self::Set(read_list(bytes, at, depth + 1)?),
            TAG_SEEN => Self::Seen(read_count(bytes, at)?),
            tag => return Err(CloneBytesError::UnknownTag { tag, at: *at - 1 }),
        })
    }

    fn read_blob(bytes: &[u8], at: &mut usize) -> Read<Self> {
        let data = read_bytes(bytes, at)?;
        let media_type = read_text(bytes, at)?;
        let (name, modified) = match take(bytes, at, 1)?[0] {
            0 => (None, None),
            _ => (Some(read_text(bytes, at)?), Some(read_number(bytes, at)?)),
        };
        Ok(Self::Blob {
            bytes: data,
            media_type,
            name,
            modified,
        })
    }
}

fn write_float(out: &mut Vec<u8>, tag: u8, number: f64) -> Read<()> {
    out.push(tag);
    out.extend_from_slice(&number.to_le_bytes());
    Ok(())
}

/// A count as the format has it: four bytes, so a length past `u32::MAX` is an error and not
/// a number that wrapped.
fn write_count(out: &mut Vec<u8>, count: usize) -> Read<()> {
    let count = u32::try_from(count).map_err(|_| CloneBytesError::TooLarge { len: count })?;
    out.extend_from_slice(&count.to_le_bytes());
    Ok(())
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Read<()> {
    write_count(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn write_units(out: &mut Vec<u8>, units: &[u16]) -> Read<()> {
    write_count(out, units.len())?;
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn write_list(out: &mut Vec<u8>, tag: u8, items: &[Tree]) -> Read<()> {
    out.push(tag);
    write_count(out, items.len())?;
    for item in items {
        item.write_to(out)?;
    }
    Ok(())
}

fn write_fields(out: &mut Vec<u8>, tag: u8, fields: &[(Vec<u16>, Tree)]) -> Read<()> {
    out.push(tag);
    write_count(out, fields.len())?;
    for (name, value) in fields {
        write_units(out, name)?;
        value.write_to(out)?;
    }
    Ok(())
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize) -> Read<&'a [u8]> {
    let slice = at
        .checked_add(count)
        .and_then(|end| bytes.get(*at..end))
        .ok_or(CloneBytesError::Truncated { at: *at })?;
    *at += count;
    Ok(slice)
}

fn read_count(bytes: &[u8], at: &mut usize) -> Read<usize> {
    let slice = take(bytes, at, 4)?;
    let count = u32::from_le_bytes(slice.try_into().expect("four bytes")) as usize;
    // Every item takes at least a byte of its own.
    if count > bytes.len() {
        return Err(CloneBytesError::TooMany {
            count,
            len: bytes.len(),
        });
    }
    Ok(count)
}

fn read_number(bytes: &[u8], at: &mut usize) -> Read<f64> {
    let slice = take(bytes, at, 8)?;
    Ok(f64::from_le_bytes(slice.try_into().expect("eight bytes")))
}

fn read_bytes(bytes: &[u8], at: &mut usize) -> Read<Vec<u8>> {
    let count = read_count(bytes, at)?;
    Ok(take(bytes, at, count)?.to_vec())
}

fn read_units(bytes: &[u8], at: &mut usize) -> Read<Vec<u16>> {
    let count = read_count(bytes, at)?;
    let slice = take(bytes, at, count * 2)?;
    Ok(slice
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

fn read_text(bytes: &[u8], at: &mut usize) -> Read<String> {
    let count = read_count(bytes, at)?;
    let slice = take(bytes, at, count)?;
    String::from_utf8(slice.to_vec()).map_err(|_| CloneBytesError::NotUtf8 { at: *at })
}

fn read_list(bytes: &[u8], at: &mut usize, depth: usize) -> Read<Vec<Tree>> {
    let count = read_count(bytes, at)?;
    (0..count)
        .map(|_| Tree::read_nested(bytes, at, depth))
        .collect()
}

fn read_fields(bytes: &[u8], at: &mut usize, depth: usize) -> Read<Vec<(Vec<u16>, Tree)>> {
    let count = read_count(bytes, at)?;
    (0..count)
        .map(|_| Ok((read_units(bytes, at)?, Tree::read_nested(bytes, at, depth)?)))
        .collect()
}

fn read_pairs(bytes: &[u8], at: &mut usize, depth: usize) -> Read<Vec<(Tree, Tree)>> {
    let count = read_count(bytes, at)?;
    (0..count)
        .map(|_| {
            Ok((
                Tree::read_nested(bytes, at, depth)?,
                Tree::read_nested(bytes, at, depth)?,
            ))
        })
        .collect()
}
