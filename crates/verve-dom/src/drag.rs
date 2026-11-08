//! HTML5 drag and drop: a drag inside the page, and files (or text) dragged in from the system.
//!
//! A drag inside the page is a state machine on [`Document::pointer_down`], `pointer_move` and
//! `pointer_up`. A press on a draggable element arms it; once the pointer has moved 4 CSS pixels
//! `dragstart` fires, and if no listener cancels it the page hears no more `pointermove` or
//! `mousemove`: a `pointercancel` ends the pointer's events, and each further move is a `drag`
//! at the source with `dragenter`, `dragover` and `dragleave` at what is under the pointer. Letting
//! go fires `drop` at an element that accepted the drag (a canceled `dragover`), then `dragend` at
//! the source.
//!
//! A press on text that cannot be dragged selects it instead: the selection follows the pointer
//! through [`Document::text_position_at`] (so it needs a position provider, as the drawer's), from
//! the press, or from the old anchor with Shift held.
//!
//! A drag from the system has no source and no `drag` or `dragend`: the shell calls
//! [`Document::drag_enter`], [`Document::drag_over`], [`Document::drag_leave`] and
//! [`Document::drop`].
//!
//! The order and the fields of the events were observed on Electron 39.8.10 (a hidden window; a
//! drag driven by `webContents.debugger` with `Input.setInterceptDrags`, `Input.dispatchMouseEvent`
//! and `Input.dispatchDragEvent`; probe `p24/probe4`); `tests/drag.rs` freezes them:
//!
//! - the move that crosses the threshold is heard as `pointermove` and `mousemove`, then comes
//!   `dragstart` (at the press's coordinates, `buttons` 1, `effectAllowed` `uninitialized`, no
//!   types), then `pointercancel`; moves of 3 pixels start nothing and the press ends in a click;
//! - on a target's first `dragenter`, `dragover` follows at once; on a change of target the new
//!   one hears `dragenter` and then the old one `dragleave` (with `relatedTarget` each other), and
//!   no `dragover` (a drag from the system is told `dragenter` alone when it first comes in); `drag` is heard at the source before each of these; `dragend` and `dragleave`
//!   are not cancelable;
//! - `dragenter` and `dragover` list `types` and `items` but `files.length` is 0 until the `drop`;
//!   `dropEffect` reads `none` in every event but `dragend`, which has the effect the drag ended
//!   with; a drop that nobody accepted is a `drag`, a `dragleave`, then `dragend` with `none`;
//! - a drag from the system lists `text/plain` and `Files` in `types`, its string items first,
//!   then one `file` item per file; `File.path` does not exist; a `drop` is preceded by one more
//!   `dragover` at the same place.
//!
//! A drop of files that nothing accepted does not navigate: with `navigateOnDragDrop` at its
//! default of `false` Electron 39.8.10 opens no window and fires no `will-navigate`, and the drop
//! is the `dragleave` above (the review's probe, `p24-review3/out-navdrop.txt`; with the option
//! `true` the same drop asks `setWindowOpenHandler` for the file's address, never `will-navigate`,
//! `out-navdrop_on.txt`). `WebPreferences.navigate_on_drag_drop` is that option; nothing here
//! routes the `true` case yet.
//!
//! A drop with the effect left alone (the listener cancelled `dragover` and did not set
//! `dropEffect`) gets `copy` for an `effectAllowed` of `all` or `uninitialized`, and otherwise
//! `move` if it allows that, else `copy`, else `link` (`p24-review3/out-effects.txt`). An explicit
//! `none`, or an effect the source does not allow, is no drop: `drag`, `dragleave`, `dragend` with
//! `none`.

use std::path::PathBuf;

use boa_engine::{Context, JsObject, JsValue};

use crate::bind::data_transfer::{self, Entry, Mode};
use crate::bind::node::wrap;
use crate::dom::NodeId;
use crate::layout_provider::Boundary;
use crate::{Document, Pointer};

/// How far the pointer moves, along either axis, before a press is a drag.
const THRESHOLD: f32 = 4.;

/// What a drag would do if it were dropped where it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DropEffect {
    /// It would not be accepted.
    #[default]
    None,
    Copy,
    Move,
    Link,
}

impl DropEffect {
    /// The name `dropEffect` has it by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Link => "link",
        }
    }

    fn of(name: &str) -> Self {
        match name {
            "copy" => Self::Copy,
            "move" => Self::Move,
            "link" => Self::Link,
            _ => Self::None,
        }
    }
}

/// What a person drags in from outside the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalDrag {
    pub files: Vec<PathBuf>,
    pub text: Option<String>,
    pub uri_list: Vec<String>,
    pub html: Option<String>,
}

/// The drag the document is in the middle of, if any.
#[derive(Default)]
pub(crate) struct DragState {
    phase: Phase,
    /// Where a press that selects text began: the selection follows the pointer from here.
    anchor: Option<Boundary>,
    /// The text field a press selected in: the selection follows the pointer in it.
    pub(crate) field: Option<NodeId>,
    /// The word a double click took in that field (without the spaces after it): dragging on
    /// extends the selection by words.
    pub(crate) words: Option<std::ops::Range<usize>>,
}

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    /// The primary button went down on `source`, which can be dragged.
    Pressed(Press),
    Active(Box<Active>),
}

struct Press {
    source: NodeId,
    x: f32,
    y: f32,
}

/// A drag that has begun.
struct Active {
    /// The element dragged; none for a drag from the system.
    source: Option<NodeId>,
    entries: Vec<Entry>,
    effect_allowed: String,
    external: Option<ExternalDrag>,
    /// Where `setDragImage` put the image's hot spot: recorded, never drawn.
    image: Option<(i32, i32)>,
    /// The element that heard the last `dragenter`.
    over: Option<NodeId>,
    /// Whether the last `dragenter` or `dragover` was canceled: the element takes the drop.
    accepted: bool,
    effect: DropEffect,
}

/// Whether an `effectAllowed` permits `effect`.
fn permits(allowed: &str, effect: &str) -> bool {
    match allowed {
        "uninitialized" | "all" => true,
        "none" => false,
        other => {
            let lower = other.to_ascii_lowercase();
            lower.contains(effect)
        }
    }
}

/// The effect a drop gets: the one the listener chose when `effectAllowed` permits it, else (a
/// `dropEffect` left alone) copy for an unrestricted source, otherwise the first of move, copy
/// and link that it allows (Electron 39.8.10, `p24-review3/out-effects.txt`).
fn resolve(allowed: &str, chosen: Option<&str>) -> DropEffect {
    if let Some(chosen) = chosen {
        return if chosen != "none" && permits(allowed, chosen) {
            DropEffect::of(chosen)
        } else {
            DropEffect::None
        };
    }
    if matches!(allowed, "all" | "uninitialized") {
        return DropEffect::Copy;
    }
    ["move", "copy", "link"]
        .into_iter()
        .find(|e| permits(allowed, e))
        .map_or(DropEffect::None, DropEffect::of)
}

/// The items of a drag from the system. The files are real (read, with their paths) for a drop and
/// empty stand-ins, only to be listed, before it.
fn external_entries(cx: &mut Context, data: &ExternalDrag, real: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(text) = &data.text {
        entries.push(Entry::text("text/plain", text));
    }
    if !data.uri_list.is_empty() {
        entries.push(Entry::text("text/uri-list", &data.uri_list.join("\r\n")));
    }
    if let Some(html) = &data.html {
        entries.push(Entry::text("text/html", html));
    }
    let mut budget = crate::clipboard::TOTAL_LIMIT;
    for path in &data.files {
        let file = if real {
            crate::clipboard::file_object(cx, path, &mut budget)
        } else {
            crate::clipboard::stand_in(cx, path)
        };
        entries.extend(file.map(Entry::File));
    }
    entries
}

impl Document {
    fn phase(&self) -> std::cell::RefMut<'_, Phase> {
        std::cell::RefMut::map(self.view.borrow_mut(), |view| &mut view.drag.phase)
    }

    fn take_active(&self) -> Option<Box<Active>> {
        let mut phase = self.phase();
        match std::mem::take(&mut *phase) {
            Phase::Active(active) => Some(active),
            other => {
                *phase = other;
                None
            }
        }
    }

    /// Whether a drag is under way.
    pub fn dragging(&self) -> bool {
        matches!(*self.phase(), Phase::Active(_))
    }

    /// The hot spot the page gave `setDragImage` in this drag's `dragstart`. Drag images are not
    /// drawn (registered): this is only what the page asked for.
    pub fn drag_image_hot_spot(&self) -> Option<(i32, i32)> {
        match &*self.phase() {
            Phase::Active(active) => active.image,
            _ => None,
        }
    }

    /// The primary button went down on `target`: a draggable element arms the machine, and
    /// anything else starts a selection of text.
    pub(crate) fn drag_pressed(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        (pointer, placed): (&Pointer, bool),
    ) {
        self.arm(target, pointer);
        let armed = matches!(*self.phase(), Phase::Pressed(_));
        self.view.borrow_mut().drag.anchor = None;
        self.view.borrow_mut().drag.field = None;
        self.view.borrow_mut().drag.words = None;
        if !armed && pointer.button == 0 {
            self.begin_selecting(runtime, target, pointer, placed);
        }
    }

    /// A press on text selects: a caret at the press, or from the selection's anchor to it with
    /// Shift held, after a cancelable `selectstart` at the text. A press on a form control, which
    /// keeps its own selection (a field's is moved by the drawer that edits it), leaves the
    /// selection alone; so does a press that is not at `pointer` (`press(node)` has no place). One
    /// where there is no text collapses the selection into the element. Electron 39.8.10: a press
    /// on a button, a link or a draggable element leaves the selection, a press on text is
    /// `selectstart` then a caret, and a canceled `selectstart` keeps the old selection
    /// (`p24-review3/out-select.txt`).
    fn begin_selecting(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        pointer: &Pointer,
        placed: bool,
    ) {
        let in_control = {
            let state = self.state.borrow();
            let mut at = Some(target);
            let mut found = false;
            while let Some(node) = at {
                if matches!(
                    state.dom.get(node).and_then(|n| n.tag()),
                    Some("input" | "textarea" | "select" | "button" | "option")
                ) {
                    found = true;
                    break;
                }
                at = state.dom.parent(node);
            }
            found
        };
        if placed && pointer.button == 0 && self.press_in_field(runtime, target, pointer) {
            return;
        }
        if in_control || !placed {
            return;
        }
        let Some(position) = self.text_position_at(pointer.x, pointer.y) else {
            self.collapse_into(runtime, target);
            return;
        };
        let before = self.state.borrow().dom.selection_anchor();
        let anchor = match before {
            Some(anchor) if pointer.modifiers.shift => anchor,
            _ => {
                let cx = runtime.context();
                let started = crate::bind::events::fire_at(
                    cx,
                    position.0,
                    "Event",
                    "selectstart",
                    &[("composed", false.into())],
                    true,
                );
                if !started.unwrap_or(true) {
                    return;
                }
                position
            }
        };
        crate::bind::node::with_dom(runtime.context(), |dom| {
            dom.select_between(anchor, position)
        });
        self.view.borrow_mut().drag.anchor = Some(anchor);
    }

    /// A press where there is no text puts the caret in the element pressed.
    fn collapse_into(&self, runtime: &mut verve::Runtime, target: NodeId) {
        let at = {
            let state = self.state.borrow();
            let element = state.dom.get(target).is_some_and(|n| n.is_element());
            if element {
                Some(target)
            } else {
                state.dom.parent_element(target)
            }
        };
        if let Some(node) = at {
            crate::bind::node::with_dom(runtime.context(), |dom| {
                dom.select_between((node, 0), (node, 0));
            });
        }
    }

    /// The pointer moved with a selecting press down: the selection reaches to it.
    fn extend_selecting(&self, runtime: &mut verve::Runtime, pointer: &Pointer) {
        if self.drag_in_field(runtime, pointer) {
            return;
        }
        let Some(anchor) = self.view.borrow().drag.anchor else {
            return;
        };
        if let Some(position) = self.text_position_at(pointer.x, pointer.y) {
            crate::bind::node::with_dom(runtime.context(), |dom| {
                dom.select_between(anchor, position);
            });
        }
    }

    fn arm(&self, target: NodeId, pointer: &Pointer) {
        let source = {
            let state = self.state.borrow();
            let dom = &state.dom;
            let mut at = Some(target);
            let mut found = None;
            while let Some(node) = at {
                let n = dom.get(node);
                let attr = n
                    .and_then(|n| n.attr("draggable"))
                    .map(str::to_ascii_lowercase);
                let default = n.is_some_and(|n| {
                    n.tag() == Some("img") || (n.tag() == Some("a") && n.attr("href").is_some())
                });
                match attr.as_deref() {
                    Some("true") => found = Some(node),
                    Some("false") => break,
                    _ if default => found = Some(node),
                    _ => {}
                }
                if found.is_some() {
                    break;
                }
                at = dom.parent(node);
            }
            found
        };
        *self.phase() = match source {
            Some(source) if pointer.button == 0 && pointer.count <= 1 => Phase::Pressed(Press {
                source,
                x: pointer.x,
                y: pointer.y,
            }),
            _ => Phase::Idle,
        };
    }

    /// `drag`, at the source, if it is still in the document.
    fn drag_event(&self, runtime: &mut verve::Runtime, active: &Active, pointer: &Pointer) {
        if let Some(source) = active.source.filter(|s| self.state.borrow().dom.exists(*s)) {
            let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
            self.fire_drag(runtime, "drag", source, pointer, &transfer, None);
        }
    }

    /// The pointer moved during a drag: the drag moves, and nothing else hears of it. Answers
    /// whether it was one.
    pub(crate) fn drag_moved(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        pointer: &Pointer,
    ) -> bool {
        let Some(mut active) = self.take_active() else {
            return false;
        };
        self.drag_event(runtime, &active, pointer);
        self.aim(runtime, &mut active, target, pointer, true);
        *self.phase() = Phase::Active(active);
        runtime.run_microtasks();
        true
    }

    /// The pointer moved after a press on something draggable: a drag begins once it has gone far
    /// enough.
    pub(crate) fn drag_may_start(&self, runtime: &mut verve::Runtime, pointer: &Pointer) {
        self.extend_selecting(runtime, pointer);
        let (source, x, y) = match &*self.phase() {
            Phase::Pressed(p) => (p.source, p.x, p.y),
            _ => return,
        };
        let far = (pointer.x - x).abs() >= THRESHOLD || (pointer.y - y).abs() >= THRESHOLD;
        if !far {
            return;
        }
        *self.phase() = Phase::Idle;
        if !self.state.borrow().dom.exists(source) {
            return;
        }
        let at = Pointer { x, y, ..*pointer };
        let cx = runtime.context();
        let transfer =
            data_transfer::create(cx, Mode::Write, self.default_data(source), "uninitialized");
        let mut init = crate::person::fields(&at, 1, 0);
        init.push(("dataTransfer", transfer.clone().into()));
        init.push(("relatedTarget", JsValue::null()));
        if !self.fire(runtime, source, "DragEvent", "dragstart", &init) {
            return;
        }
        let active = Active {
            source: Some(source),
            entries: data_transfer::entries_of(&transfer),
            effect_allowed: data_transfer::effect_allowed(&transfer),
            external: None,
            image: data_transfer::image_hot_spot(&transfer),
            over: None,
            accepted: false,
            effect: DropEffect::None,
        };
        // The pointer's own events end here: it is the drag's now.
        {
            let mut state = self.state.borrow_mut();
            if let Some(active) = state.dom.active.take() {
                state.dom.mark(active);
            }
        }
        self.view.borrow_mut().pressed = None;
        self.fire(runtime, source, "PointerEvent", "pointercancel", &[]);
        *self.phase() = Phase::Active(Box::new(active));
        runtime.run_microtasks();
    }

    /// What a link or a picture carries when it is dragged and its `dragstart` adds nothing.
    fn default_data(&self, source: NodeId) -> Vec<Entry> {
        let state = self.state.borrow();
        let Some(node) = state.dom.get(source) else {
            return Vec::new();
        };
        let address = match node.tag() {
            Some("a") => node.attr("href"),
            Some("img") => node.attr("src"),
            _ => None,
        };
        address
            .map(|a| {
                vec![
                    Entry::text("text/uri-list", a),
                    Entry::text("text/plain", a),
                ]
            })
            .unwrap_or_default()
    }

    /// The button let go. During a drag it drops, or ends the drag; answers `Some(false)` then,
    /// since the press makes no click. After a press that never became a drag it answers `None`.
    pub(crate) fn drag_released(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        pointer: &Pointer,
    ) -> Option<bool> {
        let Some(mut active) = self.take_active() else {
            self.view.borrow_mut().drag.anchor = None;
            self.view.borrow_mut().drag.field = None;
            let mut phase = self.phase();
            if matches!(*phase, Phase::Pressed(_)) {
                *phase = Phase::Idle;
            }
            return None;
        };
        if active.over != Some(target) {
            self.aim(runtime, &mut active, target, pointer, true);
        }
        // Chromium hears the drag move once more at the place it is let go (`p24-review3`): a
        // `drag`, and a `dragover` where the element took it, before the `drop`; where nothing
        // did there is only the `drag`.
        self.drag_event(runtime, &active, pointer);
        if let Some(over) = active.over.filter(|_| active.accepted) {
            self.over(runtime, &mut active, over, pointer);
        }
        self.conclude(runtime, &mut active, pointer);
        runtime.run_microtasks();
        Some(false)
    }

    /// Escape, or the system took the drag away: it ends where it is, dropped nowhere.
    pub fn drag_cancel(&self, runtime: &mut verve::Runtime) -> bool {
        let Some(mut active) = self.take_active() else {
            return false;
        };
        let pointer = Pointer::default();
        self.drag_event(runtime, &active, &pointer);
        if let Some(over) = active.over {
            let transfer = self.transfer_for(runtime, &active, Mode::Protected, false);
            self.fire_drag(runtime, "dragleave", over, &pointer, &transfer, None);
        }
        active.accepted = false;
        self.finish(runtime, &active, &pointer, DropEffect::None);
        runtime.run_microtasks();
        true
    }

    /// The transfer an event of the drag carries.
    fn transfer_for(
        &self,
        runtime: &mut verve::Runtime,
        active: &Active,
        mode: Mode,
        real_files: bool,
    ) -> JsObject {
        let cx = runtime.context();
        let entries = match &active.external {
            Some(data) => external_entries(cx, data, real_files),
            None => active.entries.clone(),
        };
        data_transfer::create(cx, mode, entries, &active.effect_allowed)
    }

    fn fire_drag(
        &self,
        runtime: &mut verve::Runtime,
        kind: &str,
        target: NodeId,
        pointer: &Pointer,
        transfer: &JsObject,
        related: Option<NodeId>,
    ) -> bool {
        let related = {
            let live = related.filter(|n| self.state.borrow().dom.exists(*n));
            live.map_or(JsValue::null(), |n| wrap(runtime.context(), n))
        };
        let mut init = crate::person::fields(pointer, 0, 0);
        init.push(("dataTransfer", transfer.clone().into()));
        init.push(("relatedTarget", related));
        self.fire(runtime, target, "DragEvent", kind, &init)
    }

    /// The drag is over `target` now: `dragenter` where it has come to and `dragleave` where it
    /// has left, or `dragover` where it stays.
    fn aim(
        &self,
        runtime: &mut verve::Runtime,
        active: &mut Active,
        target: NodeId,
        pointer: &Pointer,
        then_over: bool,
    ) {
        let previous = active.over.filter(|n| self.state.borrow().dom.exists(*n));
        if previous == Some(target) {
            self.over(runtime, active, target, pointer);
            return;
        }
        let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
        let canceled = !self.fire_drag(runtime, "dragenter", target, pointer, &transfer, previous);
        if let Some(old) = previous {
            let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
            self.fire_drag(runtime, "dragleave", old, pointer, &transfer, Some(target));
        }
        active.over = Some(target);
        active.accepted = canceled;
        active.effect = if canceled {
            resolve(
                &active.effect_allowed,
                data_transfer::drop_effect(&transfer).as_deref(),
            )
        } else {
            DropEffect::None
        };
        // A drag that moves into the window is told `dragover` at once; one that is dragged in
        // from the system is first told only `dragenter` (Electron 39.8.10).
        if previous.is_none() && then_over {
            self.over(runtime, active, target, pointer);
        }
    }

    fn over(
        &self,
        runtime: &mut verve::Runtime,
        active: &mut Active,
        target: NodeId,
        pointer: &Pointer,
    ) {
        let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
        let allowed = self.fire_drag(runtime, "dragover", target, pointer, &transfer, None);
        let chosen = data_transfer::drop_effect(&transfer);
        active.effect = if allowed {
            DropEffect::None
        } else {
            resolve(&active.effect_allowed, chosen.as_deref())
        };
        active.accepted = !allowed && active.effect != DropEffect::None;
    }

    /// The drag ends over `active.over`: it drops there when that element took it, else it
    /// leaves. A drop nothing took does not navigate (see the header).
    fn conclude(
        &self,
        runtime: &mut verve::Runtime,
        active: &mut Active,
        pointer: &Pointer,
    ) -> DropEffect {
        let over = active.over.filter(|n| self.state.borrow().dom.exists(*n));
        let mut effect = DropEffect::None;
        match over {
            Some(over) if active.accepted && active.effect != DropEffect::None => {
                effect = active.effect;
                let transfer = self.transfer_for(runtime, active, Mode::Read, true);
                self.fire_drag(runtime, "drop", over, pointer, &transfer, None);
            }
            Some(over) => {
                let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
                self.fire_drag(runtime, "dragleave", over, pointer, &transfer, None);
            }
            None => {}
        }
        self.finish(runtime, active, pointer, effect);
        effect
    }

    /// The drag is over: `dragend` at the source, with the effect it ended in.
    fn finish(
        &self,
        runtime: &mut verve::Runtime,
        active: &Active,
        pointer: &Pointer,
        effect: DropEffect,
    ) {
        let Some(source) = active.source.filter(|s| self.state.borrow().dom.exists(*s)) else {
            return;
        };
        let transfer = self.transfer_for(runtime, active, Mode::Protected, false);
        data_transfer::put_drop_effect(&transfer, effect.as_str());
        self.fire_drag(runtime, "dragend", source, pointer, &transfer, None);
    }

    /// Files, text or markup are dragged into the window and the pointer is at `pointer`: the
    /// element under it hears `dragenter` and `dragover`. Answers what a drop there would do.
    pub fn drag_enter(
        &self,
        runtime: &mut verve::Runtime,
        data: ExternalDrag,
        pointer: &Pointer,
    ) -> DropEffect {
        let _ = self.take_active();
        let mut active = Box::new(Active {
            source: None,
            entries: Vec::new(),
            effect_allowed: "all".to_owned(),
            external: Some(data),
            image: None,
            over: None,
            accepted: false,
            effect: DropEffect::None,
        });
        let target = self.pointer_target(pointer);
        self.aim(runtime, &mut active, target, pointer, false);
        let effect = active.effect;
        *self.phase() = Phase::Active(active);
        runtime.run_microtasks();
        effect
    }

    /// The pointer moved on during a drag from the system.
    pub fn drag_over(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> DropEffect {
        let Some(mut active) = self.take_active() else {
            return DropEffect::None;
        };
        let target = self.pointer_target(pointer);
        self.aim(runtime, &mut active, target, pointer, true);
        let effect = active.effect;
        *self.phase() = Phase::Active(active);
        runtime.run_microtasks();
        effect
    }

    /// The drag from the system left the window.
    pub fn drag_leave(&self, runtime: &mut verve::Runtime) {
        let Some(active) = self.take_active() else {
            return;
        };
        if let Some(over) = active.over.filter(|n| self.state.borrow().dom.exists(*n)) {
            let transfer = self.transfer_for(runtime, &active, Mode::Protected, false);
            self.fire_drag(
                runtime,
                "dragleave",
                over,
                &Pointer::default(),
                &transfer,
                None,
            );
        }
        runtime.run_microtasks();
    }

    /// The drag from the system was let go at `pointer`. A page that took it hears `drop`, with
    /// its files readable; one that did not hears `dragleave`, and a window dropped files on
    /// goes to the first of them. Answers what the drop did.
    pub fn drop(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> DropEffect {
        let Some(mut active) = self.take_active() else {
            return DropEffect::None;
        };
        let target = self.pointer_target(pointer);
        // Chromium asks once more, where the drop is, whether it is taken.
        self.aim(runtime, &mut active, target, pointer, false);
        let effect = self.conclude(runtime, &mut active, pointer);
        runtime.run_microtasks();
        effect
    }
}
