//! `path`: Node's two flavours of it, `path.posix` and `path.win32`, each working as Node's does
//! case for case (they follow the algorithms of Node's own `lib/path.js`), and the one
//! `require('path')` answers.
//!
//! On macOS, Linux and the rest, `require('path')` is `path.posix`, as in Node. On Windows it is
//! a third flavour, the one this runtime has always had: it reads `\` and `/` alike and knows a
//! drive (`C:`), but writes `/` — `path.join('notes', 'a.md')` is `notes/a.md`. Node's own
//! `path.win32` would write `notes\a.md`, and a plugin that builds a vault path with
//! `path.join` and hands it to Obsidian's API (whose paths are always `/`-separated) then names
//! a file that does not exist; Windows itself takes `/` in a file path as well as `\`. A plugin
//! that wants Node's exact Windows behaviour has it as `path.win32`.

use boa_engine::{
    Context, JsResult, JsString, JsValue, js_string, native_function::NativeFunctionPointer,
};

use crate::native::{arg, coded_error, function, get, js_text, object, text_of};

/// The flavours, as the constant each function is made for.
pub(crate) const POSIX: u8 = 0;
pub(crate) const WIN32: u8 = 1;
/// Drive-aware, `/`-writing: `require('path')` on Windows.
const MIXED: u8 = 2;

fn is_posix_sep(c: u8) -> bool {
    c == b'/'
}

fn is_sep(c: u8) -> bool {
    c == b'/' || c == b'\\'
}

fn is_drive_letter(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Resolves `.` and `..` in a path with no root, Node's `normalizeString`: `allow_above_root`
/// keeps a `..` that climbs out of it, as a relative path may.
fn normalize_string(
    path: &[u8],
    allow_above_root: bool,
    separator: u8,
    is_separator: fn(u8) -> bool,
) -> Vec<u8> {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length = 0usize;
    let mut last_slash: isize = -1;
    let mut dots: i32 = 0;
    let mut code = 0u8;
    let mut i = 0usize;
    while i <= path.len() {
        if i < path.len() {
            code = path[i];
        } else if is_separator(code) {
            break;
        } else {
            code = b'/';
        }
        if is_separator(code) {
            if last_slash == i as isize - 1 || dots == 1 {
                // Nothing: an empty segment or `.`.
            } else if dots == 2 {
                let ends_in_dots = res.len() >= 2
                    && last_segment_length == 2
                    && res[res.len() - 1] == b'.'
                    && res[res.len() - 2] == b'.';
                if !ends_in_dots {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| c == separator) {
                            None => {
                                res.clear();
                                last_segment_length = 0;
                            }
                            Some(at) => {
                                res.truncate(at);
                                // `lastIndexOf` answers -1 when there is none.
                                last_segment_length =
                                    match res.iter().rposition(|&c| c == separator) {
                                        Some(p) => res.len() - 1 - p,
                                        None => res.len(),
                                    };
                            }
                        }
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(separator);
                    }
                    res.extend_from_slice(b"..");
                    last_segment_length = 2;
                }
            } else {
                let segment = &path[(last_slash + 1) as usize..i];
                if !res.is_empty() {
                    res.push(separator);
                }
                res.extend_from_slice(segment);
                last_segment_length = i - (last_slash + 1) as usize;
            }
            last_slash = i as isize;
            dots = 0;
        } else if code == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

/// The working folder, as `path.resolve` starts from it, in a flavour's terms.
pub(crate) fn cwd(context: &Context, flavour: u8) -> String {
    let cwd = crate::shared(context).cwd.borrow().clone();
    match flavour {
        // Node's: on Windows, the working folder with its drive taken off and `/` throughout.
        POSIX if cfg!(windows) => {
            let slashed = cwd.replace('\\', "/");
            match slashed.find('/') {
                Some(at) => slashed[at..].to_owned(),
                None => "/".into(),
            }
        }
        MIXED => cwd.replace('\\', "/"),
        _ => cwd,
    }
}

// ─── posix ──────────────────────────────────────────────────────────────────────────────────

pub(crate) fn posix_resolve(paths: &[String], cwd: &str) -> String {
    let mut resolved: Vec<u8> = Vec::new();
    let mut absolute = false;
    let mut i = paths.len() as isize - 1;
    while i >= -1 && !absolute {
        let path = if i >= 0 {
            paths[i as usize].as_bytes()
        } else {
            cwd.as_bytes()
        };
        i -= 1;
        if path.is_empty() {
            continue;
        }
        let mut next = path.to_vec();
        next.push(b'/');
        next.extend_from_slice(&resolved);
        resolved = next;
        absolute = path[0] == b'/';
    }
    let tail = normalize_string(&resolved, !absolute, b'/', is_posix_sep);
    if absolute {
        format!("/{}", text(&tail))
    } else if tail.is_empty() {
        ".".into()
    } else {
        text(&tail)
    }
}

fn posix_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".into();
    }
    let absolute = bytes[0] == b'/';
    let trailing = bytes[bytes.len() - 1] == b'/';
    let mut out = normalize_string(bytes, !absolute, b'/', is_posix_sep);
    if out.is_empty() {
        if absolute {
            return "/".into();
        }
        return if trailing { "./".into() } else { ".".into() };
    }
    if trailing {
        out.push(b'/');
    }
    if absolute {
        format!("/{}", text(&out))
    } else {
        text(&out)
    }
}

fn posix_join(paths: &[String]) -> String {
    let parts: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return ".".into();
    }
    posix_normalize(&parts.join("/"))
}

/// `path.posix.relative` of two paths already resolved.
fn posix_relative_resolved(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let (from, to) = (from.as_bytes(), to.as_bytes());
    let from_start = 1;
    let from_end = from.len();
    let from_len = from_end - from_start;
    let to_start = 1;
    let to_len = to.len() - to_start;
    let length = from_len.min(to_len);
    let mut last_common: isize = -1;
    let mut i = 0;
    while i < length {
        let c = from[from_start + i];
        if c != to[to_start + i] {
            break;
        } else if c == b'/' {
            last_common = i as isize;
        }
        i += 1;
    }
    if i == length {
        if to_len > length {
            if to[to_start + i] == b'/' {
                return text(&to[to_start + i + 1..]);
            }
            if i == 0 {
                return text(&to[to_start + i..]);
            }
        } else if from_len > length {
            if from[from_start + i] == b'/' {
                last_common = i as isize;
            } else if i == 0 {
                last_common = 0;
            }
        }
    }
    let mut out = String::new();
    let mut j = (from_start as isize + last_common + 1) as usize;
    while j <= from_end {
        if j == from_end || from[j] == b'/' {
            out.push_str(if out.is_empty() { ".." } else { "/.." });
        }
        j += 1;
    }
    format!(
        "{out}{}",
        text(&to[(to_start as isize + last_common) as usize..])
    )
}

fn posix_dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".into();
    }
    let has_root = bytes[0] == b'/';
    let mut end: isize = -1;
    let mut matched_slash = true;
    for i in (1..bytes.len()).rev() {
        if bytes[i] == b'/' {
            if !matched_slash {
                end = i as isize;
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    if end == -1 {
        return if has_root { "/".into() } else { ".".into() };
    }
    if has_root && end == 1 {
        return "//".into();
    }
    text(&bytes[..end as usize])
}

/// `basename`, both flavours': `start` skips a drive, `is_separator` says what divides.
fn basename_with(
    path: &str,
    suffix: Option<&str>,
    start_at: usize,
    is_separator: fn(u8) -> bool,
) -> String {
    let bytes = path.as_bytes();
    let mut start = start_at;
    let mut end: isize = -1;
    let mut matched_slash = true;
    if let Some(suffix) = suffix.filter(|s| !s.is_empty() && s.len() <= bytes.len()) {
        if suffix == path {
            return String::new();
        }
        let suffix = suffix.as_bytes();
        let mut ext_idx = suffix.len() as isize - 1;
        let mut first_non_slash_end: isize = -1;
        let mut i = bytes.len() as isize - 1;
        while i >= start_at as isize {
            let c = bytes[i as usize];
            if is_separator(c) {
                if !matched_slash {
                    start = i as usize + 1;
                    break;
                }
            } else {
                if first_non_slash_end == -1 {
                    matched_slash = false;
                    first_non_slash_end = i + 1;
                }
                if ext_idx >= 0 {
                    if c == suffix[ext_idx as usize] {
                        ext_idx -= 1;
                        if ext_idx == -1 {
                            end = i;
                        }
                    } else {
                        ext_idx = -1;
                        end = first_non_slash_end;
                    }
                }
            }
            i -= 1;
        }
        if start as isize == end {
            end = first_non_slash_end;
        } else if end == -1 {
            end = bytes.len() as isize;
        }
        if end < start as isize {
            return String::new();
        }
        return text(&bytes[start..end as usize]);
    }
    let mut i = bytes.len() as isize - 1;
    while i >= start_at as isize {
        if is_separator(bytes[i as usize]) {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    text(&bytes[start..end as usize])
}

/// Where the extension starts in the last part of a path, both flavours' rule: dots that begin
/// a name (`.bashrc`, `..`) are not an extension. Answers `(start of part, dot, end)`.
fn find_ext(bytes: &[u8], stop: usize, is_separator: fn(u8) -> bool) -> (usize, isize, isize, i32) {
    let mut start_dot: isize = -1;
    let mut start_part = stop;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut pre_dot_state = 0;
    let mut i = bytes.len() as isize - 1;
    while i >= stop as isize {
        let c = bytes[i as usize];
        if is_separator(c) {
            if !matched_slash {
                start_part = i as usize + 1;
                break;
            }
            i -= 1;
            continue;
        }
        if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        if c == b'.' {
            if start_dot == -1 {
                start_dot = i;
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot != -1 {
            pre_dot_state = -1;
        }
        i -= 1;
    }
    (start_part, start_dot, end, pre_dot_state)
}

fn no_ext(start_part: usize, start_dot: isize, end: isize, pre_dot_state: i32) -> bool {
    start_dot == -1
        || end == -1
        || pre_dot_state == 0
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part as isize + 1)
}

fn extname_with(path: &str, stop: usize, is_separator: fn(u8) -> bool) -> String {
    let bytes = path.as_bytes();
    let (start_part, start_dot, end, pre) = find_ext(bytes, stop, is_separator);
    if no_ext(start_part, start_dot, end, pre) {
        return String::new();
    }
    text(&bytes[start_dot as usize..end as usize])
}

/// What `path.parse` answers: root, dir, base, ext, name.
#[derive(Default)]
struct Parsed {
    root: String,
    dir: String,
    base: String,
    ext: String,
    name: String,
}

fn posix_parse(path: &str) -> Parsed {
    let mut out = Parsed::default();
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return out;
    }
    let absolute = bytes[0] == b'/';
    let start = if absolute {
        out.root = "/".into();
        1
    } else {
        0
    };
    let (start_part, start_dot, end, pre) = find_ext(bytes, start, is_posix_sep);
    if end != -1 {
        let from = if start_part == 0 && absolute {
            1
        } else {
            start_part
        };
        let end = end as usize;
        if no_ext(start_part, start_dot, end as isize, pre) {
            out.base = text(&bytes[from..end]);
            out.name = out.base.clone();
        } else {
            let dot = start_dot as usize;
            out.name = text(&bytes[from..dot]);
            out.base = text(&bytes[from..end]);
            out.ext = text(&bytes[dot..end]);
        }
    }
    if start_part > 0 && start_part > start {
        out.dir = text(&bytes[..start_part - 1]);
    } else if start_part > 0 {
        out.dir = text(
            &bytes[..start_part
                .saturating_sub(1)
                .max(if absolute { 1 } else { 0 })],
        );
    } else if absolute {
        out.dir = "/".into();
    }
    out
}

// ─── win32 ──────────────────────────────────────────────────────────────────────────────────

/// The root of a Windows path: a UNC share (`\\server\share`), a drive (`C:`, `C:\`), or a lone
/// separator. Answers the device (`C:`, `\\server\share`, or empty), where the root ends, and
/// whether it is absolute. `unc_needs_tail` is `normalize`'s rule: `\\server\share` with nothing
/// after it is only a device when something follows.
fn win32_root(path: &[u8]) -> (Vec<u8>, usize, bool, bool) {
    let len = path.len();
    let mut device = Vec::new();
    let mut root_end = 0;
    let mut absolute = false;
    let mut whole_unc = false;
    if len == 0 {
        return (device, root_end, absolute, whole_unc);
    }
    let code = path[0];
    if len == 1 {
        if is_sep(code) {
            return (device, 1, true, false);
        }
        return (device, 0, false, false);
    }
    if is_sep(code) {
        absolute = true;
        if is_sep(path[1]) {
            let mut j = 2;
            let mut last = j;
            while j < len && !is_sep(path[j]) {
                j += 1;
            }
            if j < len && j != last {
                let first = path[last..j].to_vec();
                last = j;
                while j < len && is_sep(path[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_sep(path[j]) {
                        j += 1;
                    }
                    if j == len || j != last {
                        whole_unc = j == len;
                        device = [b"\\\\".as_slice(), &first, b"\\", &path[last..j]].concat();
                        root_end = j;
                    }
                }
            }
        } else {
            root_end = 1;
        }
    } else if is_drive_letter(code) && path[1] == b':' {
        device = path[..2].to_vec();
        root_end = 2;
        if len > 2 && is_sep(path[2]) {
            absolute = true;
            root_end = 3;
        }
    }
    (device, root_end, absolute, whole_unc)
}

fn win32_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return ".".into();
    }
    if len == 1 {
        return if is_posix_sep(bytes[0]) {
            "\\".into()
        } else {
            path.to_owned()
        };
    }
    let (device, root_end, absolute, whole_unc) = win32_root(bytes);
    if whole_unc {
        return format!("{}\\", text(&device));
    }
    let mut tail = if root_end < len {
        normalize_string(&bytes[root_end..], !absolute, b'\\', is_sep)
    } else {
        Vec::new()
    };
    if tail.is_empty() && !absolute {
        tail = b".".to_vec();
    }
    if !tail.is_empty() && is_sep(bytes[len - 1]) {
        tail.push(b'\\');
    }
    let tail = text(&tail);
    if device.is_empty() {
        return if absolute { format!("\\{tail}") } else { tail };
    }
    let device = text(&device);
    if absolute {
        format!("{device}\\{tail}")
    } else {
        format!("{device}{tail}")
    }
}

fn win32_is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    match b.len() {
        0 => false,
        _ if is_sep(b[0]) => true,
        n => n > 2 && is_drive_letter(b[0]) && b[1] == b':' && is_sep(b[2]),
    }
}

fn win32_join(paths: &[String]) -> String {
    let parts: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    let Some(first) = parts.first() else {
        return ".".into();
    };
    let mut joined = parts.join("\\");
    // A UNC root is kept only when the first part was one; `\\\\x` from joining is collapsed.
    let first = first.as_bytes();
    let mut needs_replace = true;
    let mut slash_count = 0;
    if is_sep(first[0]) {
        slash_count += 1;
        if first.len() > 1 && is_sep(first[1]) {
            slash_count += 1;
            if first.len() > 2 {
                if is_sep(first[2]) {
                    slash_count += 1;
                } else {
                    needs_replace = false;
                }
            }
        }
    }
    if needs_replace {
        let bytes = joined.as_bytes();
        while slash_count < bytes.len() && is_sep(bytes[slash_count]) {
            slash_count += 1;
        }
        if slash_count >= 2 {
            joined = format!("\\{}", &joined[slash_count..]);
        }
    }
    win32_normalize(&joined)
}

pub(crate) fn win32_resolve(paths: &[String], cwd: &str) -> String {
    let mut resolved_device: Vec<u8> = Vec::new();
    let mut resolved_tail: Vec<u8> = Vec::new();
    let mut resolved_absolute = false;
    let mut i = paths.len() as isize - 1;
    while i >= -1 {
        let path: Vec<u8> = if i >= 0 {
            let p = paths[i as usize].as_bytes().to_vec();
            if p.is_empty() {
                i -= 1;
                continue;
            }
            p
        } else if resolved_device.is_empty() {
            cwd.as_bytes().to_vec()
        } else {
            // The working folder of that drive; Node reads `=C:` from the environment, which
            // only `cmd.exe` keeps — the working folder itself, when it is on that drive.
            let c = cwd.as_bytes();
            if c.len() >= 3 && c[..2].eq_ignore_ascii_case(&resolved_device) && c[2] == b'\\' {
                c.to_vec()
            } else {
                [resolved_device.as_slice(), b"\\"].concat()
            }
        };
        i -= 1;
        let (device, root_end, absolute, _) = win32_root(&path);
        if !device.is_empty() {
            if !resolved_device.is_empty() {
                if !device.eq_ignore_ascii_case(&resolved_device) {
                    continue;
                }
            } else {
                resolved_device = device;
            }
        }
        if resolved_absolute {
            if !resolved_device.is_empty() {
                break;
            }
        } else {
            let mut next = path[root_end..].to_vec();
            next.push(b'\\');
            next.extend_from_slice(&resolved_tail);
            resolved_tail = next;
            resolved_absolute = absolute;
            if absolute && !resolved_device.is_empty() {
                break;
            }
        }
    }
    let tail = text(&normalize_string(
        &resolved_tail,
        !resolved_absolute,
        b'\\',
        is_sep,
    ));
    let device = text(&resolved_device);
    if resolved_absolute {
        format!("{device}\\{tail}")
    } else {
        let out = format!("{device}{tail}");
        if out.is_empty() { ".".into() } else { out }
    }
}

fn win32_relative(from: &str, to: &str, cwd: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_orig = win32_resolve(&[from.to_owned()], cwd);
    let to_orig = win32_resolve(&[to.to_owned()], cwd);
    if from_orig == to_orig {
        return String::new();
    }
    let from = from_orig.to_ascii_lowercase();
    let to = to_orig.to_ascii_lowercase();
    if from == to {
        return String::new();
    }
    let (fb, tb, to_orig_b) = (from.as_bytes(), to.as_bytes(), to_orig.as_bytes());
    let mut from_start = 0;
    while from_start < fb.len() && fb[from_start] == b'\\' {
        from_start += 1;
    }
    let mut from_end = fb.len();
    while from_end - 1 > from_start && fb[from_end - 1] == b'\\' {
        from_end -= 1;
    }
    let from_len = from_end - from_start;
    let mut to_start = 0;
    while to_start < tb.len() && tb[to_start] == b'\\' {
        to_start += 1;
    }
    let mut to_end = tb.len();
    while to_end - 1 > to_start && tb[to_end - 1] == b'\\' {
        to_end -= 1;
    }
    let to_len = to_end - to_start;
    let length = from_len.min(to_len);
    let mut last_common: isize = -1;
    let mut i = 0;
    while i < length {
        let c = fb[from_start + i];
        if c != tb[to_start + i] {
            break;
        } else if c == b'\\' {
            last_common = i as isize;
        }
        i += 1;
    }
    if i != length {
        if last_common == -1 {
            return to_orig;
        }
    } else {
        if to_len > length {
            if tb[to_start + i] == b'\\' {
                return text(&to_orig_b[to_start + i + 1..]);
            }
            if i == 2 {
                return text(&to_orig_b[to_start + i..]);
            }
        }
        if from_len > length {
            if fb[from_start + i] == b'\\' {
                last_common = i as isize;
            } else if i == 2 {
                last_common = 3;
            }
        }
        if last_common == -1 {
            last_common = 0;
        }
    }
    let mut out = String::new();
    let mut j = (from_start as isize + last_common + 1) as usize;
    while j <= from_end {
        if j == from_end || fb[j] == b'\\' {
            out.push_str(if out.is_empty() { ".." } else { "\\.." });
        }
        j += 1;
    }
    let mut to_start = (to_start as isize + last_common) as usize;
    if !out.is_empty() {
        return format!("{out}{}", text(&to_orig_b[to_start.min(to_end)..to_end]));
    }
    if to_orig_b.get(to_start) == Some(&b'\\') {
        to_start += 1;
    }
    text(&to_orig_b[to_start.min(to_end)..to_end])
}

fn win32_to_namespaced(path: &str, cwd: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let resolved = win32_resolve(&[path.to_owned()], cwd);
    let r = resolved.as_bytes();
    if r.len() <= 2 {
        return path.to_owned();
    }
    if r[0] == b'\\' {
        if r[1] == b'\\' && r[2] != b'?' && r[2] != b'.' {
            return format!("\\\\?\\UNC\\{}", &resolved[2..]);
        }
    } else if is_drive_letter(r[0]) && r[1] == b':' && r[2] == b'\\' {
        return format!("\\\\?\\{resolved}");
    }
    path.to_owned()
}

fn win32_dirname(path: &str) -> String {
    let b = path.as_bytes();
    let len = b.len();
    if len == 0 {
        return ".".into();
    }
    if len == 1 {
        return if is_sep(b[0]) {
            path.to_owned()
        } else {
            ".".into()
        };
    }
    let mut root_end: isize = -1;
    let mut offset = 0;
    if is_sep(b[0]) {
        root_end = 1;
        offset = 1;
        if is_sep(b[1]) {
            let mut j = 2;
            let mut last = j;
            while j < len && !is_sep(b[j]) {
                j += 1;
            }
            if j < len && j != last {
                last = j;
                while j < len && is_sep(b[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_sep(b[j]) {
                        j += 1;
                    }
                    if j == len {
                        return path.to_owned();
                    }
                    if j != last {
                        root_end = j as isize + 1;
                        offset = j + 1;
                    }
                }
            }
        }
    } else if is_drive_letter(b[0]) && b[1] == b':' {
        root_end = if len > 2 && is_sep(b[2]) { 3 } else { 2 };
        offset = root_end as usize;
    }
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = len as isize - 1;
    while i >= offset as isize {
        if is_sep(b[i as usize]) {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 {
        if root_end == -1 {
            return ".".into();
        }
        end = root_end;
    }
    text(&b[..end as usize])
}

fn drive_start(path: &str) -> usize {
    let b = path.as_bytes();
    if b.len() >= 2 && is_drive_letter(b[0]) && b[1] == b':' {
        2
    } else {
        0
    }
}

fn win32_parse(path: &str) -> Parsed {
    let mut out = Parsed::default();
    let b = path.as_bytes();
    let len = b.len();
    if len == 0 {
        return out;
    }
    if len == 1 {
        if is_sep(b[0]) {
            out.root = path.to_owned();
            out.dir = path.to_owned();
        } else {
            out.base = path.to_owned();
            out.name = path.to_owned();
        }
        return out;
    }
    let mut root_end = 0;
    if is_sep(b[0]) {
        root_end = 1;
        if is_sep(b[1]) {
            let mut j = 2;
            let mut last = j;
            while j < len && !is_sep(b[j]) {
                j += 1;
            }
            if j < len && j != last {
                last = j;
                while j < len && is_sep(b[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_sep(b[j]) {
                        j += 1;
                    }
                    if j == len {
                        root_end = j;
                    } else if j != last {
                        root_end = j + 1;
                    }
                }
            }
        }
    } else if is_drive_letter(b[0]) && b[1] == b':' {
        if len <= 2 {
            out.root = path.to_owned();
            out.dir = path.to_owned();
            return out;
        }
        root_end = 2;
        if is_sep(b[2]) {
            if len == 3 {
                out.root = path.to_owned();
                out.dir = path.to_owned();
                return out;
            }
            root_end = 3;
        }
    }
    if root_end > 0 {
        out.root = text(&b[..root_end]);
    }
    let (start_part, start_dot, end, pre) = find_ext(b, root_end, is_sep);
    if end != -1 {
        let end = end as usize;
        if no_ext(start_part, start_dot, end as isize, pre) {
            out.base = text(&b[start_part..end]);
            out.name = out.base.clone();
        } else {
            let dot = start_dot as usize;
            out.name = text(&b[start_part..dot]);
            out.base = text(&b[start_part..end]);
            out.ext = text(&b[dot..end]);
        }
    }
    if start_part > 0 && start_part != root_end {
        out.dir = text(&b[..start_part - 1]);
    } else {
        out.dir = out.root.clone();
    }
    out
}

// ─── The mixed flavour ──────────────────────────────────────────────────────────────────────

/// A path's drive (`C:`) and the rest, `\` made `/`.
fn mixed_split(path: &str) -> (String, String) {
    let slashed = path.replace('\\', "/");
    let start = drive_start(&slashed);
    (slashed[..start].to_owned(), slashed[start..].to_owned())
}

fn mixed_is_absolute(path: &str) -> bool {
    let (drive, rest) = mixed_split(path);
    !drive.is_empty() || rest.starts_with('/')
}

fn mixed_normalize(path: &str) -> String {
    let (drive, rest) = mixed_split(path);
    if drive.is_empty() {
        return posix_normalize(&rest);
    }
    // A drive's path is read from its root: `C:x` as `C:/x`.
    let rooted = format!("/{}", rest.trim_start_matches('/'));
    format!("{drive}{}", posix_normalize(&rooted))
}

fn mixed_join(paths: &[String]) -> String {
    let parts: Vec<&str> = paths
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return ".".into();
    }
    mixed_normalize(&parts.join("/"))
}

fn mixed_resolve(paths: &[String], cwd: &str) -> String {
    let mut out = String::new();
    let mut absolute = false;
    for part in paths.iter().rev().filter(|p| !p.is_empty()) {
        out = if out.is_empty() {
            part.clone()
        } else {
            format!("{part}/{out}")
        };
        if mixed_is_absolute(part) {
            absolute = true;
            break;
        }
    }
    if !absolute {
        out = if out.is_empty() {
            cwd.to_owned()
        } else {
            format!("{cwd}/{out}")
        };
    }
    let normalized = mixed_normalize(&out);
    let (drive, rest) = mixed_split(&normalized);
    let trimmed = rest.trim_end_matches('/');
    if trimmed.is_empty() {
        format!("{drive}/")
    } else {
        format!("{drive}{trimmed}")
    }
}

fn mixed_relative(from: &str, to: &str, cwd: &str) -> String {
    let (from_drive, from_rest) = mixed_split(&mixed_resolve(&[from.to_owned()], cwd));
    let (to_drive, to_rest) = mixed_split(&mixed_resolve(&[to.to_owned()], cwd));
    if !from_drive.eq_ignore_ascii_case(&to_drive) {
        return format!("{to_drive}{to_rest}");
    }
    posix_relative_resolved(&from_rest, &to_rest)
}

fn mixed_dirname(path: &str) -> String {
    let (drive, rest) = mixed_split(path);
    if rest.is_empty() {
        return path.replace('\\', "/");
    }
    let dir = posix_dirname(&rest);
    if !drive.is_empty() && dir == "." {
        format!("{drive}.")
    } else {
        format!("{drive}{dir}")
    }
}

fn mixed_parse(path: &str) -> Parsed {
    let (drive, rest) = mixed_split(path);
    let mut parsed = posix_parse(&rest);
    if !drive.is_empty() {
        parsed.root = format!("{drive}{}", parsed.root);
        parsed.dir = format!("{drive}{}", parsed.dir);
    }
    parsed
}

// ─── The functions ──────────────────────────────────────────────────────────────────────────

/// Every argument, each required to be a string, as Node requires.
fn strings(args: &[JsValue], name: &str, context: &mut Context) -> JsResult<Vec<String>> {
    args.iter()
        .map(|value| string(value, name, context))
        .collect()
}

fn string(value: &JsValue, name: &str, context: &mut Context) -> JsResult<String> {
    if !value.is_string() {
        let received = if value.is_undefined() {
            "undefined".to_owned()
        } else {
            format!("type {}", value.type_of())
        };
        return Err(coded_error(
            context,
            &format!("The \"{name}\" argument must be of type string. Received {received}"),
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    text_of(value, context)
}

fn parsed_object(parsed: Parsed, context: &mut Context) -> JsResult<JsValue> {
    let out = object(context);
    for (key, value) in [
        ("root", parsed.root),
        ("dir", parsed.dir),
        ("base", parsed.base),
        ("ext", parsed.ext),
        ("name", parsed.name),
    ] {
        out.set(JsString::from(key), js_text(&value), false, context)?;
    }
    Ok(out.into())
}

fn join<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let parts = strings(args, "path", cx)?;
    Ok(js_text(&match F {
        POSIX => posix_join(&parts),
        WIN32 => win32_join(&parts),
        _ => mixed_join(&parts),
    }))
}

fn resolve<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let parts = strings(args, "paths[0]", cx)?;
    let cwd = cwd(cx, F);
    Ok(js_text(&match F {
        POSIX => posix_resolve(&parts, &cwd),
        WIN32 => win32_resolve(&parts, &cwd),
        _ => mixed_resolve(&parts, &cwd),
    }))
}

fn normalize<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    Ok(js_text(&match F {
        POSIX => posix_normalize(&path),
        WIN32 => win32_normalize(&path),
        _ => mixed_normalize(&path),
    }))
}

fn is_absolute<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    Ok(match F {
        POSIX => path.starts_with('/'),
        WIN32 => win32_is_absolute(&path),
        _ => mixed_is_absolute(&path),
    }
    .into())
}

fn relative<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let from = string(&arg(args, 0), "from", cx)?;
    let to = string(&arg(args, 1), "to", cx)?;
    let cwd = cwd(cx, F);
    Ok(js_text(&match F {
        POSIX => {
            if from == to {
                String::new()
            } else {
                posix_relative_resolved(&posix_resolve(&[from], &cwd), &posix_resolve(&[to], &cwd))
            }
        }
        WIN32 => win32_relative(&from, &to, &cwd),
        _ => mixed_relative(&from, &to, &cwd),
    }))
}

fn dirname<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    Ok(js_text(&match F {
        POSIX => posix_dirname(&path),
        WIN32 => win32_dirname(&path),
        _ => mixed_dirname(&path),
    }))
}

fn basename<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    let suffix = match arg(args, 1) {
        s if s.is_undefined() => None,
        s => Some(string(&s, "suffix", cx)?),
    };
    Ok(js_text(&match F {
        POSIX => basename_with(&path, suffix.as_deref(), 0, is_posix_sep),
        WIN32 => basename_with(&path, suffix.as_deref(), drive_start(&path), is_sep),
        _ => basename_with(&path, suffix.as_deref(), drive_start(&path), is_sep),
    }))
}

fn extname<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    Ok(js_text(&match F {
        POSIX => extname_with(&path, 0, is_posix_sep),
        _ => extname_with(&path, drive_start(&path), is_sep),
    }))
}

fn parse<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    let parsed = match F {
        POSIX => posix_parse(&path),
        WIN32 => win32_parse(&path),
        _ => mixed_parse(&path),
    };
    parsed_object(parsed, cx)
}

fn format<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let parts = arg(args, 0);
    if !parts.is_object() {
        return Err(coded_error(
            cx,
            &format!(
                "The \"pathObject\" argument must be of type object. Received {}",
                parts.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let field = |name: &str, cx: &mut Context| -> JsResult<String> {
        let value = get(&parts, name, cx)?;
        if value.is_null_or_undefined() {
            Ok(String::new())
        } else {
            text_of(&value, cx)
        }
    };
    let root = field("root", cx)?;
    let dir = match field("dir", cx)? {
        d if d.is_empty() => root.clone(),
        d => d,
    };
    let base = match field("base", cx)? {
        b if !b.is_empty() => b,
        _ => {
            let ext = field("ext", cx)?;
            let ext = if !ext.is_empty() && !ext.starts_with('.') {
                format!(".{ext}")
            } else {
                ext
            };
            format!("{}{ext}", field("name", cx)?)
        }
    };
    let sep = if F == WIN32 { "\\" } else { "/" };
    Ok(js_text(&if dir.is_empty() {
        base
    } else if dir == root {
        format!("{dir}{base}")
    } else {
        format!("{dir}{sep}{base}")
    }))
}

fn to_namespaced<const F: u8>(
    _: &JsValue,
    args: &[JsValue],
    cx: &mut Context,
) -> JsResult<JsValue> {
    let value = arg(args, 0);
    if F != WIN32 || !value.is_string() {
        return Ok(value);
    }
    let path = text_of(&value, cx)?;
    let cwd = cwd(cx, F);
    Ok(js_text(&win32_to_namespaced(&path, &cwd)))
}

/// `path.matchesGlob(path, pattern)`: `*`, `**`, `?` and `[…]`, separators in the flavour's.
fn matches_glob<const F: u8>(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = string(&arg(args, 0), "path", cx)?;
    let pattern = string(&arg(args, 1), "pattern", cx)?;
    let (path, pattern) = if F == POSIX {
        (path, pattern)
    } else {
        (path.replace('\\', "/"), pattern.replace('\\', "/"))
    };
    Ok(glob(pattern.as_bytes(), path.as_bytes()).into())
}

fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') if pattern.get(1) == Some(&b'*') => {
            let rest = pattern[2..].strip_prefix(b"/").unwrap_or(&pattern[2..]);
            (0..=text.len()).any(|at| (at == 0 || text[at - 1] == b'/') && glob(rest, &text[at..]))
                || (0..=text.len())
                    .any(|at| glob(rest, &text[at..]) && !text[..at].is_empty() && rest.is_empty())
        }
        Some(b'*') => (0..=text.len())
            .take_while(|&at| at == 0 || text[at - 1] != b'/')
            .any(|at| glob(&pattern[1..], &text[at..])),
        Some(b'?') => text.first().is_some_and(|&c| c != b'/') && glob(&pattern[1..], &text[1..]),
        Some(b'[') => {
            let Some(close) = pattern.iter().position(|&c| c == b']') else {
                return text.first() == Some(&b'[') && glob(&pattern[1..], &text[1..]);
            };
            let Some(&c) = text.first() else { return false };
            let set = &pattern[1..close];
            let (negate, set) = match set.first() {
                Some(b'!') | Some(b'^') => (true, &set[1..]),
                _ => (false, set),
            };
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == b'-' {
                    hit |= set[i] <= c && c <= set[i + 2];
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != negate && glob(&pattern[close + 1..], &text[1..])
        }
        Some(&p) => text.first() == Some(&p) && glob(&pattern[1..], &text[1..]),
    }
}

const fn functions<const F: u8>() -> [(&'static str, usize, NativeFunctionPointer); 13] {
    [
        ("join", 0, join::<F>),
        ("resolve", 0, resolve::<F>),
        ("normalize", 1, normalize::<F>),
        ("isAbsolute", 1, is_absolute::<F>),
        ("relative", 2, relative::<F>),
        ("dirname", 1, dirname::<F>),
        ("basename", 2, basename::<F>),
        ("extname", 1, extname::<F>),
        ("parse", 1, parse::<F>),
        ("format", 1, format::<F>),
        ("toNamespacedPath", 1, to_namespaced::<F>),
        ("_makeLong", 1, to_namespaced::<F>),
        ("matchesGlob", 2, matches_glob::<F>),
    ]
}

fn flavour(
    context: &mut Context,
    list: &[(&str, usize, NativeFunctionPointer)],
    sep: &str,
    delimiter: &str,
) -> JsResult<boa_engine::JsObject> {
    let path = object(context);
    for (name, length, f) in list {
        let f = function(context, name, *length, *f);
        path.set(JsString::from(*name), f, false, context)?;
    }
    path.set(js_string!("sep"), js_text(sep), false, context)?;
    path.set(js_string!("delimiter"), js_text(delimiter), false, context)?;
    Ok(path)
}

/// `require('path')`, with its `posix` and `win32`; `path/posix` and `path/win32` are those two.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let posix = flavour(context, &functions::<POSIX>(), "/", ":")?;
    let win32 = flavour(context, &functions::<WIN32>(), "\\", ";")?;
    let own = if cfg!(windows) {
        flavour(context, &functions::<MIXED>(), "/", ";")?
    } else {
        posix.clone()
    };
    for path in [&posix, &win32, &own] {
        path.set(js_string!("posix"), posix.clone(), false, context)?;
        path.set(js_string!("win32"), win32.clone(), false, context)?;
    }
    Ok(own.into())
}

pub(crate) fn posix_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let path = crate::modules::node_module(context, "path")?;
    get(&path, "posix", context)
}

pub(crate) fn win32_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let path = crate::modules::node_module(context, "path")?;
    get(&path, "win32", context)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn posix_as_node() {
        assert_eq!(posix_normalize("a/b/../c/./d"), "a/c/d");
        assert_eq!(posix_normalize("/a//b/"), "/a/b/");
        assert_eq!(posix_normalize("../x"), "../x");
        assert_eq!(posix_normalize("./"), "./");
        assert_eq!(posix_normalize(""), ".");
        assert_eq!(posix_normalize("/.."), "/");
        assert_eq!(posix_normalize("a/../../b"), "../b");
        assert_eq!(posix_join(&v(&["/a", "b", "../c"])), "/a/c");
        assert_eq!(posix_join(&v(&["", ""])), ".");
        assert_eq!(
            posix_resolve(&v(&["/foo/bar", "./baz"]), "/cwd"),
            "/foo/bar/baz"
        );
        assert_eq!(
            posix_resolve(&v(&["/foo/bar", "/tmp/file/"]), "/cwd"),
            "/tmp/file"
        );
        assert_eq!(posix_resolve(&v(&["a", "../b"]), "/cwd"), "/cwd/b");
        assert_eq!(
            posix_relative_resolved("/data/orandea/test/aaa", "/data/orandea/impl/bbb"),
            "../../impl/bbb"
        );
        assert_eq!(posix_relative_resolved("/a/b", "/a/b/c/d"), "c/d");
        assert_eq!(posix_relative_resolved("/a/b/c", "/a"), "../..");
        assert_eq!(posix_dirname("/a/b/"), "/a");
        assert_eq!(posix_dirname("a"), ".");
        assert_eq!(posix_dirname("/"), "/");
        assert_eq!(
            basename_with("/a/b.html", Some(".html"), 0, is_posix_sep),
            "b"
        );
        assert_eq!(basename_with("/a/b/", None, 0, is_posix_sep), "b");
        assert_eq!(basename_with("aaa", Some("a"), 0, is_posix_sep), "aa");
        assert_eq!(extname_with("index.coffee.md", 0, is_posix_sep), ".md");
        assert_eq!(extname_with("index.", 0, is_posix_sep), ".");
        assert_eq!(extname_with(".index", 0, is_posix_sep), "");
        assert_eq!(extname_with("..", 0, is_posix_sep), "");
        let p = posix_parse("/home/user/dir/file.txt");
        assert_eq!(
            (
                p.root.as_str(),
                p.dir.as_str(),
                p.base.as_str(),
                p.ext.as_str(),
                p.name.as_str()
            ),
            ("/", "/home/user/dir", "file.txt", ".txt", "file")
        );
        let p = posix_parse("/file");
        assert_eq!(
            (p.root.as_str(), p.dir.as_str(), p.base.as_str()),
            ("/", "/", "file")
        );
        let p = posix_parse("a/b");
        assert_eq!(
            (p.root.as_str(), p.dir.as_str(), p.base.as_str()),
            ("", "a", "b")
        );
    }

    #[test]
    fn win32_as_node() {
        assert_eq!(
            win32_normalize("C:\\temp\\\\foo\\bar\\..\\"),
            "C:\\temp\\foo\\"
        );
        assert_eq!(
            win32_normalize("C:////temp\\\\/\\/\\/foo/bar"),
            "C:\\temp\\foo\\bar"
        );
        assert_eq!(win32_normalize("\\\\server\\share"), "\\\\server\\share\\");
        assert_eq!(win32_normalize("C:"), "C:.");
        assert_eq!(
            win32_join(&v(&["//server", "share"])),
            "\\\\server\\share\\"
        );
        assert_eq!(win32_join(&v(&["\\\\\\x", "y"])), "\\x\\y");
        assert_eq!(
            win32_join(&v(&["\\\\server\\share", "x"])),
            "\\\\server\\share\\x"
        );
        assert_eq!(win32_join(&v(&["a", "b"])), "a\\b");
        assert_eq!(
            win32_resolve(&v(&["C:\\foo", "bar"]), "D:\\cwd"),
            "C:\\foo\\bar"
        );
        assert_eq!(win32_resolve(&v(&["/foo"]), "D:\\cwd"), "D:\\foo");
        assert_eq!(win32_resolve(&v(&["a"]), "D:\\cwd"), "D:\\cwd\\a");
        assert_eq!(
            win32_relative("C:\\orandea\\test\\aaa", "C:\\orandea\\impl\\bbb", "C:\\"),
            "..\\..\\impl\\bbb"
        );
        assert_eq!(win32_relative("C:\\a", "D:\\b", "C:\\"), "D:\\b");
        assert_eq!(win32_dirname("C:\\a\\b"), "C:\\a");
        assert_eq!(win32_dirname("C:\\"), "C:\\");
        assert_eq!(win32_dirname("\\\\server\\share\\x"), "\\\\server\\share\\");
        assert!(win32_is_absolute("C:/x") && win32_is_absolute("\\x") && !win32_is_absolute("C:x"));
        let p = win32_parse("C:\\path\\dir\\file.txt");
        assert_eq!(
            (
                p.root.as_str(),
                p.dir.as_str(),
                p.base.as_str(),
                p.name.as_str()
            ),
            ("C:\\", "C:\\path\\dir", "file.txt", "file")
        );
        assert_eq!(win32_to_namespaced("C:\\x", "C:\\"), "\\\\?\\C:\\x");
    }

    #[test]
    fn mixed_reads_both_and_writes_slashes() {
        assert_eq!(
            mixed_normalize("C:\\Users\\me\\..\\vault"),
            "C:/Users/vault"
        );
        assert_eq!(mixed_join(&v(&["a", "b/../c", "d.md"])), "a/c/d.md");
        assert_eq!(
            mixed_resolve(&v(&["/vault", "notes", "../x.md"]), "C:/cwd"),
            "/vault/x.md"
        );
        assert_eq!(mixed_resolve(&v(&["notes"]), "C:/cwd"), "C:/cwd/notes");
        assert_eq!(mixed_resolve(&v(&["C:\\"]), "D:/cwd"), "C:/");
        assert_eq!(mixed_relative("C:/a/b", "C:/a/c/d", "C:/"), "../c/d");
        assert_eq!(mixed_dirname("C:\\a\\b.md"), "C:/a");
        assert!(mixed_is_absolute("C:\\x"));
        assert!(
            glob(b"**/*.md", b"a/b/c.md") && glob(b"*.md", b"c.md") && !glob(b"*.md", b"a/c.md")
        );
    }
}
