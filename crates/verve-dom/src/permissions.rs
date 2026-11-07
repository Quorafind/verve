//! What a page asks the embedder to allow: the permissions `Notification`, `navigator.permissions`
//! and the clipboard's `readText` need.

/// The embedder's answer to a page's use of a permission. A page that has none is refused every
/// permission.
pub trait PermissionBackend {
    /// Whether the page at `url` may use `permission` (the name an Electron handler knows it
    /// by: `notifications`, `clipboard-read`, `geolocation`, `media`...). `ask` is a number the
    /// page chose for the question. `Some` is the answer at once; `None` says it comes later,
    /// from [`crate::Document::settle_permission`] with the same `ask`. `check_only` is a
    /// question about what is allowed (`permissions.query`), not a request to use it.
    fn ask(&self, ask: u64, permission: &str, url: &str, check_only: bool) -> Option<bool>;

    /// What the page may do with `permission` without asking, if that is known (what
    /// `Notification.permission` reads); `None` when only asking tells.
    fn known(&self, permission: &str) -> Option<bool>;
}
