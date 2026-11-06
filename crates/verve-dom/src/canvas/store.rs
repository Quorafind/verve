//! Everything a document's canvases keep: the pixels, the gradients and patterns, the paths of
//! `Path2D`, the bitmaps, and the fonts text is drawn with.
//!
//! The JavaScript objects (`CanvasRenderingContext2D`, `CanvasGradient`, `Path2D`…) hold only the
//! number their thing is stored under, so the state stays on the renderer's side of the realm.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use boa_engine::object::WeakJsObject;
use boa_engine::{Context, JsError, JsObject, JsResult, JsValue};
use tiny_skia::{PathBuilder, Pixmap};
use verve::native::type_error;

use crate::bind::node::wrap;
use crate::bind::{Kind, handle_of, native_instance};
use crate::canvas::CanvasFrame;
use crate::canvas::context2d::{Canvas2d, CanvasOwner, Scene};
use crate::canvas::fonts::FontStack;
use crate::canvas::paint::{GradientSpec, PatternSpec};
use crate::dom::NodeId;
use crate::state::{State, shared};

/// The number a canvas, gradient, pattern, path or bitmap is stored under.
pub type CanvasId = u32;

/// A document's canvas state.
#[derive(Default)]
pub struct CanvasStore {
    pub canvases: HashMap<CanvasId, Canvas2d>,
    pub gradients: HashMap<CanvasId, GradientSpec>,
    pub patterns: HashMap<CanvasId, PatternSpec>,
    /// `Path2D`'s paths.
    pub paths: HashMap<CanvasId, PathBuilder>,
    /// `ImageBitmap`'s pixels.
    pub bitmaps: HashMap<CanvasId, Arc<Pixmap>>,
    /// The canvas each `<canvas>` element draws into, once it has a context.
    pub elements: HashMap<NodeId, CanvasId>,
    /// The script object last handed out for each number and class — a context, a gradient, a pattern, an
    /// `OffscreenCanvas` — so asking again answers the same one while a script holds it.
    objects: HashMap<(CanvasId, &'static str), WeakJsObject>,
    /// The renderer's own font stack, discovered when canvas text is first measured or drawn.
    pub fonts: Option<FontStack>,
    /// Whether the console was told a `<video>` is no image source.
    pub warned_video: bool,
    next: CanvasId,
}

impl CanvasStore {
    fn allocate(&mut self) -> CanvasId {
        self.next += 1;
        self.next
    }

    pub fn add_canvas(&mut self, canvas: Canvas2d) -> CanvasId {
        let id = self.allocate();
        self.canvases.insert(id, canvas);
        id
    }

    pub fn add_gradient(&mut self, gradient: GradientSpec) -> CanvasId {
        let id = self.allocate();
        self.gradients.insert(id, gradient);
        id
    }

    pub fn add_pattern(&mut self, pattern: PatternSpec) -> CanvasId {
        let id = self.allocate();
        self.patterns.insert(id, pattern);
        id
    }

    pub fn add_path(&mut self, path: PathBuilder) -> CanvasId {
        let id = self.allocate();
        self.paths.insert(id, path);
        id
    }

    pub fn add_bitmap(&mut self, pixmap: Arc<Pixmap>) -> CanvasId {
        let id = self.allocate();
        self.bitmaps.insert(id, pixmap);
        id
    }

    /// A copy of the path a command means: the `Path2D` given, or else the canvas's current one.
    pub fn path_of(&self, path: Option<CanvasId>, canvas: CanvasId) -> Option<PathBuilder> {
        match path {
            Some(id) => self.paths.get(&id).cloned(),
            None => self.canvases.get(&canvas).map(|canvas| canvas.path.clone()),
        }
    }

    /// One canvas with the stores a paint reads.
    pub fn scene(&mut self, id: CanvasId) -> Option<Scene<'_>> {
        let CanvasStore {
            canvases,
            gradients,
            patterns,
            fonts,
            ..
        } = self;
        let canvas = canvases.get_mut(&id)?;
        let fonts = fonts.get_or_insert_with(FontStack::system);
        // The scan is deferred to the first text drawn or measured, not startup.
        fonts.warm();
        Some(Scene {
            canvas,
            gradients,
            patterns,
            fonts,
        })
    }

    /// Drops what the nodes in `gone` drew into.
    pub fn forget_elements(&mut self, gone: &HashSet<NodeId>) {
        self.elements.retain(|node, _| !gone.contains(node));
        self.canvases.retain(|_, canvas| match canvas.owner {
            CanvasOwner::Element(node) => !gone.contains(&node),
            CanvasOwner::Offscreen => true,
        });
        self.objects.retain(|_, object| object.upgrade().is_some());
    }
}

/// The script object for `id`: the one handed out before while a script still holds it, else a
/// new `class` object.
pub fn canvas_object(
    context: &Context,
    id: CanvasId,
    (kind, class): (Kind, &'static str),
) -> JsObject {
    let state = shared(context);
    let known = state
        .borrow()
        .canvas
        .objects
        .get(&(id, class))
        .and_then(WeakJsObject::upgrade);
    if let Some(known) = known {
        return known;
    }
    let object = native_instance(context, class, kind, id);
    state
        .borrow_mut()
        .canvas
        .objects
        .insert((id, class), object.downgrade());
    object
}

/// `context.canvas`: the element a context draws into, or the `OffscreenCanvas`.
pub fn owner_of_context(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = handle_of(this, Kind::Context2d)
        .or_else(|| handle_of(this, Kind::BitmapRenderer))
        .ok_or_else(|| illegal_invocation("CanvasRenderingContext2D"))?;
    let owner = shared(context)
        .borrow()
        .canvas
        .canvases
        .get(&id)
        .map(|canvas| canvas.owner);
    Ok(match owner {
        Some(CanvasOwner::Element(node)) => wrap(context, node),
        Some(CanvasOwner::Offscreen) => {
            canvas_object(context, id, (Kind::Offscreen, "OffscreenCanvas")).into()
        }
        None => JsValue::null(),
    })
}

/// The number behind the `kind` object a method was called on, or the `TypeError` a browser
/// throws for a method called on anything else.
pub fn this_handle(this: &JsValue, kind: Kind, class: &str) -> JsResult<CanvasId> {
    handle_of(this, kind).ok_or_else(|| illegal_invocation(class))
}

/// The `TypeError` for a method called on something that is not a `class`.
pub fn illegal_invocation(class: &str) -> JsError {
    type_error(format!("Illegal invocation: not a {class}"))
}

/// Runs `look` on the canvas behind the `kind` object `this`; `None` when it has none.
pub fn read_canvas<R>(
    this: &JsValue,
    context: &Context,
    (kind, class): (Kind, &str),
    look: impl FnOnce(&Canvas2d) -> R,
) -> JsResult<Option<R>> {
    let id = this_handle(this, kind, class)?;
    let state = shared(context);
    let state = state.borrow();
    Ok(state.canvas.canvases.get(&id).map(look))
}

/// Every canvas whose pixels moved since the last frame: the nodes the drawer must repaint.
pub fn commit(state: &mut State) -> Vec<NodeId> {
    state
        .canvas
        .canvases
        .values_mut()
        .filter_map(Canvas2d::commit)
        .collect()
}

/// The frames the drawer paints, by the `<canvas>` they belong to.
pub fn snapshot_canvases(state: &State) -> HashMap<NodeId, CanvasFrame> {
    let attached = |node: &NodeId| state.dom.is_connected(*node);
    state
        .canvas
        .canvases
        .values()
        .filter_map(|canvas| match canvas.owner {
            CanvasOwner::Element(node) if attached(&node) => {
                canvas.frame.clone().map(|frame| (node, frame))
            }
            _ => None,
        })
        .collect()
}

/// Tells the console once that a `<video>` is no canvas image source: a page that draws one
/// every frame should not fill it.
pub fn warn_video_once(state: &mut State, context: &mut Context) {
    if !std::mem::replace(&mut state.canvas.warned_video, true) {
        let message = "a <video> is not a canvas image source in Verve".to_owned();
        verve::console_line(context, verve::Level::Warn, message);
    }
}
