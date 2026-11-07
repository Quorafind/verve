//! The sessions of an app (`session.fromPartition`): what a page's network needs of the one it
//! belongs to, kept where every thread can read it.
//!
//! A session's JavaScript object lives in the main process's isolate; this is the part a page's
//! thread reads when it fetches: which schemes the main process answers, the user agent, where
//! the session keeps its files. The privileged schemes (`registerSchemesAsPrivileged`) are the
//! app's, not a session's.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::cookies::CookieStore;
use super::{Hub, lock};
use crate::api::match_pattern::Patterns;
use crate::msg::{SessionId, WebRequestStage};

/// What `registerSchemesAsPrivileged` grants a scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Privileges {
    pub standard: bool,
    pub secure: bool,
    pub bypass_csp: bool,
    pub allow_service_workers: bool,
    pub support_fetch_api: bool,
    pub cors_enabled: bool,
    pub stream: bool,
    pub code_cache: bool,
}

/// A scheme `registerSchemesAsPrivileged` named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Privileged {
    pub scheme: String,
    pub privileges: Privileges,
}

/// One session: an in-memory partition, a persistent one (`persist:name`), or the default.
pub struct Session {
    id: SessionId,
    partition: String,
    /// The folder `session.fromPath` named.
    path: Option<PathBuf>,
    state: Mutex<State>,
    /// The session's cookies, made the first time they are used: where the file is depends on
    /// `userData`, which the app may still move until then.
    cookies: OnceLock<Arc<CookieStore>>,
    /// The filter of the listener each `webRequest` stage has, if it has one.
    web_request: Mutex<[Option<Patterns>; 8]>,
    /// How many requests of this session's pages were sent to the main process for a listener.
    web_request_asks: AtomicU64,
    /// Which permission handlers the app set ([`Session::set_permission_handler`]).
    permission_handlers: AtomicU8,
}

#[derive(Default)]
struct State {
    user_agent: Option<String>,
    /// The schemes the main process answers: `protocol.handle` and the older `register*` and
    /// `intercept*` calls put them here. Case counts, as it does in Electron.
    handled: BTreeSet<String>,
}

impl Session {
    /// The session `partition` names; the default session's is the empty string.
    pub fn new(id: SessionId, partition: &str) -> Self {
        Self {
            id,
            partition: partition.to_owned(),
            path: None,
            state: Mutex::default(),
            cookies: OnceLock::new(),
            web_request: Mutex::default(),
            web_request_asks: AtomicU64::new(0),
            permission_handlers: AtomicU8::new(0),
        }
    }

    /// The session `fromPath` makes: persistent, in `path`.
    fn at(id: SessionId, path: &Path) -> Self {
        Self {
            path: Some(path.to_path_buf()),
            ..Self::new(id, &format!("path:{}", path.display()))
        }
    }

    /// The number the session goes by in messages and in the main process's tables.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// The partition's name as `fromPartition` was given it.
    pub fn partition(&self) -> &str {
        &self.partition
    }

    /// Whether it keeps its files: the default session and `persist:` partitions do.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some() || self.partition.is_empty() || self.partition.starts_with("persist:")
    }

    /// The folder it keeps its files in under `user_data`; `None` for an in-memory partition.
    pub fn storage_path(&self, user_data: &Path) -> Option<PathBuf> {
        if let Some(path) = &self.path {
            return Some(path.clone());
        }
        if self.partition.is_empty() {
            return Some(user_data.to_path_buf());
        }
        let name = self.partition.strip_prefix("persist:")?;
        Some(user_data.join("Partitions").join(name))
    }

    /// Whether the main process answers `scheme` for this session's pages.
    pub fn handles(&self, scheme: &str) -> bool {
        lock(&self.state).handled.contains(scheme)
    }

    /// Makes `scheme` one the main process answers, or stops it.
    pub fn set_handled(&self, scheme: &str, on: bool) {
        let mut state = lock(&self.state);
        if on {
            state.handled.insert(scheme.to_owned());
        } else {
            state.handled.remove(scheme);
        }
    }

    /// Whether a listener of `stage` wants a request for `url` of the type `kind` (a
    /// `resourceType` name): the question every request asks, which costs one lock and no message.
    pub fn web_request_wants(&self, stage: WebRequestStage, url: &url::Url, kind: &str) -> bool {
        let filters = lock(&self.web_request);
        filters[stage.index()]
            .as_ref()
            .is_some_and(|filter| filter.wants(url, kind))
    }

    /// A page's request was sent to the main process for a `webRequest` listener.
    pub fn note_web_request(&self) {
        self.web_request_asks.fetch_add(1, Ordering::Relaxed);
    }

    /// How many `FromRenderer::WebRequest` messages the pages of this session have sent: what
    /// a test counts to see that a request nobody wants costs none.
    pub fn web_request_messages(&self) -> u64 {
        self.web_request_asks.load(Ordering::Relaxed)
    }

    /// Puts a listener's filter in place of the stage's last, or takes it away with `None`.
    pub fn set_web_request(&self, stage: WebRequestStage, filter: Option<Patterns>) {
        lock(&self.web_request)[stage.index()] = filter;
    }

    /// Whether the app set the handler for permission requests (`check` false) or for
    /// permission checks (`check` true): what a page reads to know that asking the main process
    /// has any point.
    pub fn has_permission_handler(&self, check: bool) -> bool {
        self.permission_handlers.load(Ordering::Relaxed) & Self::handler_bit(check) != 0
    }

    /// Records that the app set (`on`) or took away the handler of requests or of checks.
    pub fn set_permission_handler(&self, check: bool, on: bool) {
        let bit = Self::handler_bit(check);
        if on {
            self.permission_handlers.fetch_or(bit, Ordering::Relaxed);
        } else {
            self.permission_handlers.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    fn handler_bit(check: bool) -> u8 {
        if check { 2 } else { 1 }
    }

    /// The user agent `setUserAgent` gave, if any.
    pub fn user_agent(&self) -> Option<String> {
        lock(&self.state).user_agent.clone()
    }

    /// Makes the session's requests send `agent` as `user-agent` (`None`: the app's own).
    pub fn set_user_agent(&self, agent: Option<String>) {
        lock(&self.state).user_agent = agent;
    }
}

/// The sessions made so far, by partition, and `fromPath`'s by their folder (a partition may be
/// named like a path without being the session of it).
#[derive(Default)]
pub(super) struct Registry {
    by_partition: HashMap<String, Arc<Session>>,
    by_path: HashMap<PathBuf, Arc<Session>>,
    next: SessionId,
    privileged: Vec<Privileged>,
    user_data: Option<PathBuf>,
}

impl Registry {
    /// Every session made.
    fn all(&self) -> impl Iterator<Item = &Arc<Session>> {
        self.by_partition.values().chain(self.by_path.values())
    }
}

impl Hub {
    /// The session of `partition` (`None` and `""` are the default session), made the first time.
    pub fn session_for(&self, partition: Option<&str>) -> Arc<Session> {
        let name = partition.unwrap_or_default();
        let mut registry = lock(&self.sessions);
        if let Some(found) = registry.by_partition.get(name) {
            return Arc::clone(found);
        }
        registry.next += 1;
        let made = Arc::new(Session::new(registry.next, name));
        registry
            .by_partition
            .insert(name.to_owned(), Arc::clone(&made));
        made
    }

    /// The session that keeps its files in `path` (`session.fromPath`), made the first time.
    pub fn session_at(&self, path: &Path) -> Arc<Session> {
        let mut registry = lock(&self.sessions);
        if let Some(found) = registry.by_path.get(path) {
            return Arc::clone(found);
        }
        registry.next += 1;
        let made = Arc::new(Session::at(registry.next, path));
        registry
            .by_path
            .insert(path.to_path_buf(), Arc::clone(&made));
        made
    }

    /// The session numbered `id`, if one was made.
    pub fn session_by_id(&self, id: SessionId) -> Option<Arc<Session>> {
        let registry = lock(&self.sessions);
        registry.all().find(|s| s.id == id).cloned()
    }

    /// The session page `contents` belongs to: its `webPreferences.partition`.
    pub fn session_of(&self, contents: crate::ids::ContentsId) -> Option<Arc<Session>> {
        let prefs = self.preferences_of(contents)?;
        Some(self.session_for(prefs.partition.as_deref()))
    }

    /// Records what `registerSchemesAsPrivileged` granted. Before the first page is made.
    pub fn set_privileged(&self, schemes: Vec<Privileged>) {
        lock(&self.sessions).privileged = schemes;
    }

    /// The schemes `registerSchemesAsPrivileged` named.
    pub fn privileged(&self) -> Vec<Privileged> {
        lock(&self.sessions).privileged.clone()
    }

    /// Where the app keeps its profile (`userData`), which the sessions' files hang off. The
    /// main process tells it when the app starts and whenever `app.setPath` moves it.
    pub fn set_user_data(&self, path: PathBuf) {
        lock(&self.sessions).user_data = Some(path);
    }

    /// The cookies of `session`, read from its file the first time.
    pub fn cookies_of(&self, session: &Session) -> Arc<CookieStore> {
        let user_data = lock(&self.sessions).user_data.clone();
        let store = session.cookies.get_or_init(|| {
            let folder = user_data.and_then(|dir| session.storage_path(&dir));
            Arc::new(CookieStore::open(
                folder.map(|dir| dir.join("Cookies.json")),
            ))
        });
        Arc::clone(store)
    }

    /// The app is ending: the cookies of every session are written if the profile is the app's
    /// to keep (`keep`), and not touched again either way. An app nobody launched has a scratch
    /// profile that is removed with it.
    pub(super) fn end_cookies(&self, keep: bool) {
        let sessions: Vec<_> = lock(&self.sessions).all().cloned().collect();
        for session in sessions {
            if let Some(store) = session.cookies.get() {
                store.close(keep);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partition_is_one_session_and_persist_says_where_it_keeps_its_files() {
        let session = |name: &str| Session::new(1, name);
        let data = Path::new("/data");
        assert_eq!(session("").storage_path(data), Some(data.to_path_buf()));
        assert_eq!(
            session("persist:a").storage_path(data),
            Some(data.join("Partitions").join("a"))
        );
        assert_eq!(session("memory").storage_path(data), None);
        assert!(session("persist:a").is_persistent() && !session("memory").is_persistent());
    }

    #[test]
    fn handled_schemes_are_per_session_and_case_sensitive() {
        let session = Session::new(1, "");
        session.set_handled("App", true);
        assert!(session.handles("App") && !session.handles("app"));
        session.set_handled("App", false);
        assert!(!session.handles("App"));
    }
}
