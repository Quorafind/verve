//! What a page's session says of the permissions the page uses: allowed without asking unless
//! the app set a handler, as Electron does, and asked of the main process when it did.

use std::rc::Rc;
use std::sync::Arc;

use verve_dom::PermissionBackend;

use crate::api::ipc_renderer::Outbox;
use crate::hub::Session;
use crate::msg::FromRenderer;

/// The permissions of a page, from the handlers its session has.
pub(crate) struct PagePermissions {
    session: Arc<Session>,
    outbox: Outbox,
    /// The isolate's number, which the questions it asks carry: the questions of each document
    /// are numbered from 1, and a late answer must not settle the next document's.
    generation: u64,
}

impl PagePermissions {
    pub(crate) fn new(session: Arc<Session>, outbox: &Outbox, generation: u64) -> Self {
        Self {
            session,
            outbox: Rc::clone(outbox),
            generation,
        }
    }

    /// The document's own number of the question `call` stands for, if this isolate asked it.
    pub(crate) fn ask_of(call: u64, generation: u64) -> Option<u64> {
        (call >> 32 == generation & 0xffff_ffff).then_some(call & 0xffff_ffff)
    }
}

impl PermissionBackend for PagePermissions {
    fn ask(&self, ask: u64, permission: &str, url: &str, check_only: bool) -> Option<bool> {
        if !self.session.has_permission_handler(check_only) {
            return Some(true);
        }
        self.outbox.borrow_mut().push(FromRenderer::Permission {
            call: ((self.generation & 0xffff_ffff) << 32) | ask,
            session: self.session.id(),
            permission: permission.to_owned(),
            url: url.to_owned(),
            check_only,
        });
        None
    }

    fn known(&self, _: &str) -> Option<bool> {
        (!self.session.has_permission_handler(true)).then_some(true)
    }
}
