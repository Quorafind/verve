//! More than one JavaScript world over the one document: the page's, and the isolated ones a
//! preload runs in. Each world has a window and a document object of its own over the same
//! tree, each node an object of its own in each world — so a script of one cannot see what
//! another set on its objects — and the one list of listeners a node has, which each world's
//! objects share. `Window.prototype`, `Node`, `Event` and the other classes are each world's own.

use boa_engine::Context;
use verve::{Runtime, WorldId};

use crate::state::{SheetOwner, shared};
use crate::{Document, Options, install_classes};

impl Document {
    /// Makes a world: a new realm of the runtime's context, given the window's classes and its
    /// own `window` and `document` over the same nodes. Answers its id; enter it with
    /// [`Runtime::enter_world`] to run script in it.
    ///
    /// # Errors
    ///
    /// A class or global that could not be installed.
    pub fn create_world(&self, runtime: &mut Runtime) -> Result<WorldId, String> {
        create_world(runtime.context())
    }
}

/// [`Document::create_world`], from a native function that has only the context.
///
/// # Errors
///
/// A class or global that could not be installed.
pub fn create_world(context: &mut Context) -> Result<WorldId, String> {
    let world = verve::make_world(context).map_err(|error| verve::describe(context, &error))?;
    shared(context).borrow_mut().worlds = world + 1;
    let realm = verve::world_realm(context, world).ok_or("the world is gone")?;
    let (user_agent, language) = shared(context).borrow().navigator.clone();
    let options = Options {
        user_agent,
        language,
        ..Options::default()
    };
    let installed = verve::in_realm(context, &realm, |cx| install_classes(cx, &options));
    installed.map_err(|error| verve::describe(context, &error))?;
    Ok(world)
}

/// Adds the sheet `name` to the cascade, the user's own when `user`, else the embedder's: what
/// `webFrame.insertCSS` does. The same name replaces it.
pub fn insert_stylesheet(context: &Context, name: &str, css: &str, user: bool) {
    let owner = if user {
        SheetOwner::User(name.to_owned())
    } else {
        SheetOwner::Embedder(name.to_owned())
    };
    shared(context).borrow_mut().put_sheet(owner, css);
}

/// Takes away the sheet [`insert_stylesheet`] added under `name`, whichever origin it had.
pub fn remove_stylesheet(context: &Context, name: &str) {
    let state = shared(context);
    let mut state = state.borrow_mut();
    for owner in [
        SheetOwner::Embedder(name.to_owned()),
        SheetOwner::User(name.to_owned()),
    ] {
        state.drop_sheet(&owner);
    }
}
