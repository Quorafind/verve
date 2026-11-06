//! The scripts the main process has asked a page to run and not had an answer to: each is a
//! promise (`executeJavaScript`, a frame's `executeJavaScript`) that the page's
//! [`FromRenderer::ExecuteJavaScriptReply`] settles, or that fails once the page is gone.
//!
//! [`FromRenderer::ExecuteJavaScriptReply`]: crate::msg::FromRenderer::ExecuteJavaScriptReply

use std::cell::RefCell;
use std::collections::BTreeMap;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsResult, JsValue};
use verve::clone::Cloned;

use crate::ids::ContentsId;
use crate::ipc;
use crate::msg::ToRenderer;

use super::web_frame::SCRIPT_FAILED;

/// The promises waiting for a page, by the number of the call.
#[derive(Default)]
pub(crate) struct Calls {
    waiting: RefCell<BTreeMap<u64, (ContentsId, ResolvingFunctions)>>,
}

impl Calls {
    /// Asks page `contents` to run `code` in `world`, answering the promise of its result.
    pub(crate) fn execute(
        &self,
        context: &mut Context,
        contents: ContentsId,
        (code, world): (String, i32),
    ) -> JsResult<JsValue> {
        let state = super::shared(context);
        if !state.hub.has_renderer(contents) {
            let error = ipc::error_value(SCRIPT_FAILED, context)?;
            return verve::native::settled(Err(boa_engine::JsError::from_opaque(error)), context);
        }
        let call = state.windows.next_call();
        let (promise, settle) = JsPromise::new_pending(context);
        self.waiting.borrow_mut().insert(call, (contents, settle));
        state.hub.to_renderer(
            contents,
            ToRenderer::ExecuteJavaScript {
                call,
                code,
                user_gesture: false,
                world,
            },
        );
        Ok(promise.into())
    }

    /// The page answered `call`.
    pub(crate) fn answer(&self, context: &mut Context, call: u64, result: Result<Cloned, Cloned>) {
        let Some((_, settle)) = self.waiting.borrow_mut().remove(&call) else {
            return;
        };
        let (function, value) = match result {
            Ok(value) => (&settle.resolve, value.to_value(context)),
            Err(error) => (&settle.reject, error.to_value(context)),
        };
        let value = value.unwrap_or_default();
        let called = function.call(&JsValue::undefined(), &[value], context);
        super::define::report(context, called);
    }

    /// Page `contents` is gone: what it was asked fails.
    pub(crate) fn fail(&self, context: &mut Context, contents: ContentsId) {
        let gone: Vec<u64> = {
            let waiting = self.waiting.borrow();
            let own = waiting.iter().filter(|(_, (page, _))| *page == contents);
            own.map(|(call, _)| *call).collect()
        };
        for call in gone {
            let failure = ipc::error_value(SCRIPT_FAILED, context).and_then(|error| {
                Cloned::from_value(&error, context).map_err(|_| ipc::clone_error(context))
            });
            if let Ok(error) = failure {
                self.answer(context, call, Err(error));
            }
        }
    }
}
