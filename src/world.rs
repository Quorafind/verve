//! Worlds: more than one realm of the one context, each with its own built-ins and its own
//! copy of the runtime's web globals, over the one event loop, timer queue and job queue.
//!
//! An embedder that runs trusted script beside a page's — Electron's preload, in the isolated
//! world of a page — makes a world with [`Runtime::create_world`], enters it to run that script
//! and enters the first one again after. What a realm keeps for the web classes (the event
//! classes, `fetch`'s) is kept in the realm itself, so a native function finds the classes of
//! the world it was made in, whichever world calls it.

use std::cell::RefCell;

use boa_engine::object::JsData;
use boa_engine::realm::Realm;
use boa_engine::{Context, Finalize, JsObject, JsResult, Trace};

use crate::{Runtime, globals};

/// A world: the first (the one every runtime starts with, a page's) is [`MAIN_WORLD`].
pub type WorldId = u32;

/// The world a runtime starts with.
pub const MAIN_WORLD: WorldId = 0;

/// The realms made, the first being the runtime's own.
struct Worlds(RefCell<Vec<(Realm, JsObject)>>);

/// What a realm keeps of the host's: a value the collector need not look into, since the
/// handles inside it are roots of their own.
#[derive(Finalize)]
pub(crate) struct Held<T>(pub T);

// SAFETY: a value held here is only ever a clone of handles that root themselves.
unsafe impl<T> Trace for Held<T> {
    boa_gc::empty_trace!();
}

impl<T> JsData for Held<T> {}

/// The worlds made, each with its global object; the first is the running realm's when none was
/// made.
fn list(context: &Context) -> Vec<(Realm, JsObject)> {
    match context.get_data::<Worlds>() {
        Some(worlds) => worlds.0.borrow().clone(),
        None => vec![(context.realm().clone(), context.global_object())],
    }
}

/// The world the running code belongs to: that of the function being run, whatever world called
/// it. [`MAIN_WORLD`] for a realm that is none of the worlds made.
pub fn current_world(context: &Context) -> WorldId {
    let Some(worlds) = context.get_data::<Worlds>() else {
        return MAIN_WORLD;
    };
    let worlds = worlds.0.borrow();
    worlds
        .iter()
        .position(|(realm, _)| realm == context.realm())
        .map_or(MAIN_WORLD, |at| at as WorldId)
}

/// The realm of `world`, if there is one.
pub fn world_realm(context: &Context, world: WorldId) -> Option<Realm> {
    list(context)
        .get(world as usize)
        .map(|(realm, _)| realm.clone())
}

/// The global object of `world`, if there is one.
pub fn world_global(context: &Context, world: WorldId) -> Option<JsObject> {
    list(context)
        .get(world as usize)
        .map(|(_, global)| global.clone())
}

/// How many worlds there are.
pub(crate) fn count(context: &Context) -> usize {
    list(context).len()
}

/// What the running world kept as a `T`.
pub(crate) fn realm_data<T: Clone + 'static>(context: &Context) -> Option<T> {
    realm_data_of(context.realm())
}

/// What `realm` kept as a `T`.
pub(crate) fn realm_data_of<T: Clone + 'static>(realm: &Realm) -> Option<T> {
    let kept = realm.host_defined();
    kept.get::<Held<T>>().map(|held| held.0.clone())
}

/// Keeps `value` in the running world, as what [`realm_data`] answers.
pub(crate) fn set_realm_data<T: 'static>(context: &Context, value: T) {
    context.realm().host_defined_mut().insert(Held(value));
}

/// What every world kept as a `T`, the first world's first.
pub(crate) fn every_world<T: Clone + 'static>(context: &Context) -> Vec<T> {
    list(context)
        .iter()
        .filter_map(|(realm, _)| realm_data_of::<T>(realm))
        .collect()
}

impl Runtime {
    /// Makes a world: a realm with its own built-ins (`Array.prototype` is its own) and the
    /// runtime's web globals, `console`, the timers, `fetch`, `URL` and the rest, made anew in
    /// it. The event loop, the timer queue and the job queue are the runtime's, shared by every
    /// world; so are the modules, which a world loads from the same cache.
    ///
    /// # Errors
    ///
    /// A global that could not be installed.
    pub fn create_world(&mut self) -> JsResult<WorldId> {
        if self.context.get_data::<Worlds>().is_none() {
            let first = (self.context.realm().clone(), self.context.global_object());
            self.context.insert_data(Worlds(RefCell::new(vec![first])));
        }
        let realm = self.context.create_realm()?;
        let previous = self.context.enter_realm(realm.clone());
        let installed = globals::install(&mut self.context);
        let global = self.context.global_object();
        self.context.enter_realm(previous);
        installed?;
        let worlds = self
            .context
            .get_data::<Worlds>()
            .expect("the worlds are kept");
        let mut worlds = worlds.0.borrow_mut();
        worlds.push((realm, global));
        Ok((worlds.len() - 1) as WorldId)
    }

    /// Makes `world` the one that runs from now on, answering the one that did, which
    /// `enter_world` takes back. Code a native function runs belongs to the function's own
    /// world, so this is for the host's own calls: evaluating, requiring, dispatching.
    ///
    /// # Panics
    ///
    /// `world` was not made by [`Runtime::create_world`].
    pub fn enter_world(&mut self, world: WorldId) -> WorldId {
        let realm = world_realm(&self.context, world).expect("a world that was made");
        let previous = current_world(&self.context);
        self.context.enter_realm(realm);
        previous
    }

    /// The global object of `world`.
    pub fn world_global(&self, world: WorldId) -> Option<JsObject> {
        world_global(&self.context, world)
    }
}
