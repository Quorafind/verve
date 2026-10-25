//! The permission model: what a realm may reach outside itself.
//!
//! The network half (outbound rules, listening, DNS) lives with the sockets it governs, in
//! `network`; this is the rest, shaped as Node's `--permission` is so a host that confined a
//! Node process can confine a Verve realm with the same grants: filesystem reads and writes,
//! each a list of paths, and workers. A path grants itself and everything below it.
//!
//! An embedder that says nothing keeps the disk and workers it always had — Obsidian's plugins
//! read and write the vault — while the network stays denied until granted. `verve run` turns
//! the model on as Node's `--permission` does: nothing granted unless a flag grants it.
//!
//! A path is checked where it really is, not where it claims to be: made absolute against the
//! process's working directory (what the system opens a relative path against), `..` taken out,
//! and every symbolic link and junction along it followed — a dangling link at the end too, for
//! an operation that would follow it — so neither a `..` nor a link inside a granted folder
//! reaches out of it. Node itself checks the path as written and follows links unchecked; this
//! is stricter on purpose. Grants are resolved the same way when they are set.
//!
//! A refusal is Node's: an `Error` with `code: 'ERR_ACCESS_DENIED'`, `permission`
//! (`FileSystemRead`, `FileSystemWrite`, `WorkerThreads`) and `resource`.

use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use boa_engine::{Context, JsError, JsResult, JsValue, js_string};

use crate::native::{arg, coded_error, function, js_text, object, text_of};
use crate::network::{ListenPermission, NetworkRule};

/// What a realm may do to one side of the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsAccess {
    /// Everything: Node without `--permission`, and every embedder's default.
    All,
    /// These paths and everything below them; nothing when the list is empty.
    Only(Vec<PathBuf>),
}

/// A realm's capabilities. Network capabilities start denied; the filesystem and workers start
/// as they always were, unrestricted, unless the embedder says otherwise. `child_process` is
/// reserved: it does not enable execution, which Verve has none of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub outbound: Vec<NetworkRule>,
    pub listen: ListenPermission,
    pub dns: bool,
    pub child_process: bool,
    /// What may be read from disk: `fs`, and modules loaded from files.
    pub fs_read: FsAccess,
    /// What may be created, written, renamed or removed on disk.
    pub fs_write: FsAccess,
    /// Whether the realm may start workers: Node's `--allow-worker`.
    pub worker: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            outbound: Vec::new(),
            listen: ListenPermission::Deny,
            dns: false,
            child_process: false,
            fs_read: FsAccess::All,
            fs_write: FsAccess::All,
            worker: true,
        }
    }
}

impl Permissions {
    /// Node's `--permission` with no grants: no disk, no workers, no network.
    pub fn none() -> Self {
        Self {
            fs_read: FsAccess::Only(Vec::new()),
            fs_write: FsAccess::Only(Vec::new()),
            worker: false,
            ..Self::default()
        }
    }

    /// Everything a realm can be granted: `verve run -A`.
    pub fn all() -> Self {
        Self {
            outbound: vec![NetworkRule {
                host: None,
                port: None,
            }],
            listen: ListenPermission::Any,
            dns: true,
            ..Self::default()
        }
    }

    pub fn allows_outbound(&self, host: &str, port: u16) -> bool {
        self.outbound.iter().any(|rule| {
            rule.port.is_none_or(|p| p == port)
                && rule
                    .host
                    .as_ref()
                    .is_none_or(|h| h.eq_ignore_ascii_case(host))
        })
    }

    pub fn allows_listen(&self, address: std::net::IpAddr) -> bool {
        self.listen == ListenPermission::Any
            || (self.listen == ListenPermission::Loopback && address.is_loopback())
    }

    /// Whether the network half differs: what closes open sockets when a policy is replaced.
    pub(crate) fn same_network(&self, other: &Self) -> bool {
        self.outbound == other.outbound
            && self.listen == other.listen
            && self.dns == other.dns
            && self.child_process == other.child_process
    }
}

/// The filesystem grants as they are checked: resolved once, when they are set. `None` is all.
#[derive(Clone, Debug, Default)]
pub(crate) struct Grants {
    read: Option<Vec<PathBuf>>,
    write: Option<Vec<PathBuf>>,
}

impl Grants {
    pub(crate) fn of(permissions: &Permissions) -> Self {
        let resolve = |access: &FsAccess| match access {
            FsAccess::All => None,
            FsAccess::Only(paths) => Some(paths.iter().map(|p| real(p, true)).collect()),
        };
        Self {
            read: resolve(&permissions.fs_read),
            write: resolve(&permissions.fs_write),
        }
    }

    pub(crate) fn reads_all(&self) -> bool {
        self.read.is_none()
    }

    pub(crate) fn writes_all(&self) -> bool {
        self.write.is_none()
    }

    pub(crate) fn may_read(&self, path: &Path) -> bool {
        covered(self.read.as_deref(), path)
    }

    pub(crate) fn may_write(&self, path: &Path) -> bool {
        covered(self.write.as_deref(), path)
    }
}

fn covered(grants: Option<&[PathBuf]>, path: &Path) -> bool {
    let Some(grants) = grants else {
        return true;
    };
    grants.iter().any(|grant| below(grant, path))
}

/// Whether `path` is `grant` or inside it, a whole component at a time; without regard to case
/// on Windows, as its filesystems compare names.
fn below(grant: &Path, path: &Path) -> bool {
    let mut path = path.components();
    for part in grant.components() {
        let Some(other) = path.next() else {
            return false;
        };
        let same = if cfg!(windows) {
            part.as_os_str()
                .to_string_lossy()
                .to_lowercase()
                .eq(&other.as_os_str().to_string_lossy().to_lowercase())
        } else {
            part == other
        };
        if !same {
            return false;
        }
    }
    true
}

/// `path` made absolute against the process's working directory, the way the system will open
/// it.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// `.` and `..` taken out without asking the disk: how Windows reads a path before opening it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A canonical Windows path written the way it is opened: `\\?\C:\x` as `C:\x`.
fn unverbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// Where `path` really is: absolute, every link along it followed (the last one too when
/// `follow` says so). What does not exist yet is kept as written below the part that does.
pub(crate) fn real(path: &Path, follow: bool) -> PathBuf {
    let path = absolute(path);
    // Windows takes `..` out before it opens anything; elsewhere a `..` after a link climbs out
    // of where the link points, which the walk below follows as the system does.
    let path = if cfg!(windows) { lexical(&path) } else { path };
    resolve(&path, follow, 0)
}

fn resolve(path: &Path, follow: bool, depth: usize) -> PathBuf {
    if follow && let Ok(found) = std::fs::canonicalize(path) {
        return unverbatim(found);
    }
    if depth > 40 {
        return lexical(path);
    }
    let Some(last) = path.components().next_back() else {
        return path.to_path_buf();
    };
    let parent = path.parent().unwrap_or(path);
    match last {
        Component::Normal(name) => {
            let at = resolve(parent, true, depth).join(name);
            match std::fs::symlink_metadata(&at) {
                // A link that leads nowhere yet: where it would lead is what is checked.
                Ok(meta) if follow && meta.file_type().is_symlink() => {
                    match std::fs::read_link(&at) {
                        Ok(target) => {
                            let target = if target.is_absolute() {
                                target
                            } else {
                                at.parent().unwrap_or(&at).join(target)
                            };
                            resolve(&target, true, depth + 1)
                        }
                        Err(_) => at,
                    }
                }
                _ => at,
            }
        }
        Component::ParentDir => {
            let above = resolve(parent, true, depth);
            above.parent().map(Path::to_path_buf).unwrap_or(above)
        }
        Component::CurDir => resolve(parent, follow, depth),
        Component::RootDir | Component::Prefix(_) => path.to_path_buf(),
    }
}

/// Node's `ERR_ACCESS_DENIED`.
pub(crate) fn denied(context: &mut Context, permission: &str, resource: &str) -> JsError {
    let error = coded_error(
        context,
        "Access to this API has been restricted",
        "ERR_ACCESS_DENIED",
    );
    if let Ok(value) = error.clone().into_opaque(context)
        && let Some(object) = value.as_object()
    {
        let _ = object.set(
            js_string!("permission"),
            js_text(permission),
            false,
            context,
        );
        let _ = object.set(js_string!("resource"), js_text(resource), false, context);
    }
    error
}

fn grants(context: &Context) -> Rc<Grants> {
    crate::shared(context).network.borrow().grants.clone()
}

/// Throws unless the realm may read `path` (following a link at its end).
pub(crate) fn read(context: &mut Context, path: impl AsRef<Path>) -> JsResult<()> {
    check(context, path.as_ref(), false, true)
}

/// Throws unless the realm may read `path` itself, a link at its end not followed: `lstat`,
/// `readlink`.
pub(crate) fn read_here(context: &mut Context, path: impl AsRef<Path>) -> JsResult<()> {
    check(context, path.as_ref(), false, false)
}

/// Throws unless the realm may write `path` (following a link at its end).
pub(crate) fn write(context: &mut Context, path: impl AsRef<Path>) -> JsResult<()> {
    check(context, path.as_ref(), true, true)
}

/// Throws unless the realm may write `path` itself: removing or renaming a link, or making one.
pub(crate) fn write_here(context: &mut Context, path: impl AsRef<Path>) -> JsResult<()> {
    check(context, path.as_ref(), true, false)
}

fn check(context: &mut Context, path: &Path, writing: bool, follow: bool) -> JsResult<()> {
    let grants = grants(context);
    if (writing && grants.writes_all()) || (!writing && grants.reads_all()) {
        return Ok(());
    }
    let resolved = real(path, follow);
    let allowed = if writing {
        grants.may_write(&resolved)
    } else {
        grants.may_read(&resolved)
    };
    if allowed {
        return Ok(());
    }
    let permission = if writing {
        "FileSystemWrite"
    } else {
        "FileSystemRead"
    };
    let shown = unverbatim(lexical(&absolute(path)));
    Err(denied(context, permission, &shown.to_string_lossy()))
}

/// Whether the realm may start a worker; Node's refusal when not.
pub(crate) fn worker(context: &mut Context) -> JsResult<()> {
    if crate::shared(context).network.borrow().permissions.worker {
        return Ok(());
    }
    Err(denied(context, "WorkerThreads", ""))
}

thread_local! {
    /// The grants of the realm resolving a module now; see [`Resolving`].
    static RESOLVING: RefCell<Vec<Rc<Grants>>> = const { RefCell::new(Vec::new()) };
}

/// Module resolution reads `package.json` files deep in code that has no realm to ask. While
/// one of these is held, it asks this one: a `package.json` the realm may not read is not read,
/// and resolution goes on as though it were not there — the module it leads to is still refused
/// when it is loaded.
pub(crate) struct Resolving;

impl Resolving {
    pub(crate) fn enter(context: &Context) -> Self {
        let grants = grants(context);
        RESOLVING.with(|stack| stack.borrow_mut().push(grants));
        Self
    }
}

impl Drop for Resolving {
    fn drop(&mut self) {
        RESOLVING.with(|stack| stack.borrow_mut().pop());
    }
}

/// Whether module resolution may read `path`, under the realm resolving now.
pub(crate) fn resolution_may_read(path: &Path) -> bool {
    RESOLVING.with(|stack| match stack.borrow().last() {
        Some(grants) if !grants.reads_all() => grants.may_read(&real(path, true)),
        _ => true,
    })
}

/// `process.permission`: `has(scope, reference)`, as Node answers it.
pub(crate) fn install(context: &mut Context, process: &boa_engine::JsObject) -> JsResult<()> {
    let permission = object(context);
    let has = function(context, "has", 2, |_, args, cx| {
        has(args, cx).map(Into::into)
    });
    permission.set(js_string!("has"), has, false, context)?;
    process.set(js_string!("permission"), permission, false, context)?;
    Ok(())
}

fn has(args: &[JsValue], context: &mut Context) -> JsResult<bool> {
    let scope = arg(args, 0);
    if !scope.is_string() {
        return Err(coded_error(
            context,
            "The \"scope\" argument must be of type string",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let scope = text_of(&scope, context)?;
    let reference = arg(args, 1);
    let reference = if reference.is_null_or_undefined() {
        None
    } else {
        Some(text_of(&reference, context)?)
    };
    let shared = crate::shared(context);
    let network = shared.network.borrow();
    let grants = network.grants.clone();
    let permissions = &network.permissions;
    Ok(match scope.as_str() {
        "fs" => grants.reads_all() && grants.writes_all(),
        "fs.read" => match reference {
            None => grants.reads_all(),
            Some(path) => grants.may_read(&real(Path::new(&path), true)),
        },
        "fs.write" => match reference {
            None => grants.writes_all(),
            Some(path) => grants.may_write(&real(Path::new(&path), true)),
        },
        "worker" => permissions.worker,
        // Verve's own: the outbound rules, for a `host` or `host:port`.
        "net" => match reference {
            None => permissions
                .outbound
                .iter()
                .any(|rule| rule.host.is_none() && rule.port.is_none()),
            Some(target) => {
                let (host, port) = match target.rsplit_once(':') {
                    Some((host, port)) if port.parse::<u16>().is_ok() => {
                        (host.to_owned(), port.parse().ok())
                    }
                    _ => (target.clone(), None),
                };
                match port {
                    Some(port) => permissions.allows_outbound(&host, port),
                    None => permissions.outbound.iter().any(|rule| {
                        rule.port.is_none()
                            && rule
                                .host
                                .as_ref()
                                .is_none_or(|h| h.eq_ignore_ascii_case(&host))
                    }),
                }
            }
        },
        // No processes, native addons or WASI here, ever.
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_covers_itself_and_below_whole_components_at_a_time() {
        let root = std::env::temp_dir().join("verve-permission-unit");
        let grants = Grants {
            read: Some(vec![root.join("a")]),
            write: None,
        };
        assert!(grants.may_read(&root.join("a")));
        assert!(grants.may_read(&root.join("a").join("b.txt")));
        assert!(!grants.may_read(&root.join("ab")));
        assert!(!grants.may_read(&root));
        assert!(grants.may_write(&root));
    }

    #[test]
    fn dot_dot_is_taken_out_before_the_check() {
        let root = std::env::temp_dir().join("verve-permission-dots");
        let resolved = real(&root.join("a").join("..").join("..").join("x"), true);
        assert!(!below(&real(&root.join("a"), true), &resolved));
    }
}
