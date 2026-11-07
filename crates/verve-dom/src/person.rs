//! What a person does to the page, the parts that are not the document's own: where the pointer
//! is (`:hover` and its events, in Chromium's order), the wheel, which input the page last had
//! (the focus ring), and whether a key was used by the page. Kept apart from `lib.rs`, whose
//! size is ratcheted.
//!
//! The fields are what Electron 39.8.10 shows (observed with `sendInputEvent`): `detail` is 0
//! on the pointer events, `mousemove`/`mouseover`/`mouseout`/`wheel` and `contextmenu`, and the
//! click count on `mousedown`/`mouseup`/`click`/`auxclick`; `button` is -1 on `pointermove`,
//! `pointerover` and `pointerout`, and 0 on their mouse twins.

use boa_engine::{Context, JsValue};

use crate::input::LastInput;
use crate::{Document, Key, NodeId, Pointer, bind};

/// The fields of a pointer event: where it is, which button, how many are held, the modifiers and
/// the `detail` the event class has.
pub(crate) fn fields(pointer: &Pointer, buttons: u16, detail: u32) -> Vec<(&'static str, JsValue)> {
    vec![
        ("clientX", f64::from(pointer.x).into()),
        ("clientY", f64::from(pointer.y).into()),
        ("screenX", f64::from(pointer.x).into()),
        ("screenY", f64::from(pointer.y).into()),
        ("pageX", f64::from(pointer.x).into()),
        ("pageY", f64::from(pointer.y).into()),
        ("button", pointer.button.into()),
        ("buttons", buttons.into()),
        ("ctrlKey", pointer.modifiers.ctrl.into()),
        ("shiftKey", pointer.modifiers.shift.into()),
        ("altKey", pointer.modifiers.alt.into()),
        ("metaKey", pointer.modifiers.meta.into()),
        ("detail", detail.into()),
    ]
}

/// The fields of the events a moving pointer makes: no `detail`, and no button for a pointer
/// event.
fn motion(pointer: &Pointer, pointer_event: bool) -> Vec<(&'static str, JsValue)> {
    let mut init = fields(pointer, pointer.buttons, 0);
    if pointer_event {
        init.iter_mut()
            .filter(|(name, _)| *name == "button")
            .for_each(|(_, value)| *value = (-1i16).into());
    }
    init
}

/// What a person did, which every trusted press, release and key says once, at its start.
pub(crate) enum Act<'a> {
    Press,
    Release,
    Key(&'a Key),
}

impl Document {
    /// The pointer moved over `target` (found by the caller): the elements it left and the ones it
    /// came onto hear their events, a `:hover` rule follows, and `target` hears `pointermove` and
    /// `mousemove`.
    pub fn pointer_move_on(&self, runtime: &mut verve::Runtime, target: NodeId, pointer: &Pointer) {
        if self.drag_moved(runtime, target, pointer) {
            return;
        }
        self.hover_node(runtime, Some(target), pointer);
        let init = motion(pointer, true);
        self.fire(runtime, target, "PointerEvent", "pointermove", &init);
        let init = motion(pointer, false);
        self.fire(runtime, target, "MouseEvent", "mousemove", &init);
        self.drag_may_start(runtime, pointer);
        runtime.run_microtasks();
    }

    /// The pointer is over `node` (or over nothing): out of the old element and each element left
    /// (innermost first), over the new and each element entered (outermost first), the pointer's
    /// events and then the mouse's. Chromium's order within each: `out`, `leave`s, `over`, `enter`s.
    pub(crate) fn hover_node(
        &self,
        runtime: &mut verve::Runtime,
        node: Option<NodeId>,
        pointer: &Pointer,
    ) {
        let previous = self.view.borrow().over;
        if previous == node {
            return;
        }
        self.view.borrow_mut().over = node;
        let path = path_of(&self.state.borrow().dom, node);
        let (left, entered) = self.state.borrow_mut().dom.set_hovered(path);
        let leaving = previous.filter(|p| self.state.borrow().dom.exists(*p));
        let related = |n: Option<NodeId>, cx: &mut Context| {
            n.map_or(JsValue::null(), |n| bind::node::wrap(cx, n))
        };
        for (class, out, leave, over, enter) in [
            (
                "PointerEvent",
                "pointerout",
                "pointerleave",
                "pointerover",
                "pointerenter",
            ),
            (
                "MouseEvent",
                "mouseout",
                "mouseleave",
                "mouseover",
                "mouseenter",
            ),
        ] {
            let mut init = motion(pointer, class == "PointerEvent");
            if let Some(previous) = leaving {
                let to = related(node, runtime.context());
                init.push(("relatedTarget", to));
                self.fire(runtime, previous, class, out, &init);
                for id in &left {
                    self.fire(runtime, *id, class, leave, &init);
                }
                init.pop();
            }
            if let Some(node) = node {
                let from = related(previous, runtime.context());
                init.push(("relatedTarget", from));
                self.fire(runtime, node, class, over, &init);
                for id in entered.iter().rev() {
                    self.fire(runtime, *id, class, enter, &init);
                }
            }
        }
    }

    /// The wheel turned over `target`: `wheel` there. Answers whether nothing prevented it.
    pub fn wheel_on(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        pointer: &Pointer,
        (delta_x, delta_y): (f32, f32),
    ) -> bool {
        let mut init = fields(pointer, pointer.buttons, 0);
        init.push(("deltaX", f64::from(delta_x).into()));
        init.push(("deltaY", f64::from(delta_y).into()));
        let allowed = self.fire(runtime, target, "WheelEvent", "wheel", &init);
        runtime.run_microtasks();
        allowed
    }

    /// The one entry point of a person's input. It gives transient activation (a script may copy
    /// and cut for a few seconds, [`Document::activate`]) and says which input the page last had
    /// (the focus ring, [`LastInput`]): two pieces of state that mean different things, input
    /// modality and a time-limited permission, with different filters (a lone modifier is not
    /// keyboard use for the ring but is a press for activation).
    pub(crate) fn person(&self, act: Act) {
        self.activate();
        match act {
            Act::Press => self.state.borrow_mut().dom.last_input = LastInput::Pointer,
            Act::Release => {}
            Act::Key(key) => self.keyboard_used(key),
        }
    }

    /// A pointer was pressed on nothing that takes the focus: Chromium then shows the ring of the
    /// next focus a script asks for, as it does before any input.
    pub(crate) fn pointer_missed(&self) {
        self.state.borrow_mut().dom.last_input = LastInput::Nothing;
    }

    /// A key was pressed. Chromium's rule (observed from a button the mouse focused): the ring
    /// shows after Shift alone, Shift+A, F5, Escape and a plain letter, and not after Control or
    /// Alt alone, nor with Control, Alt or Meta held. The ring that shows is the focused element's,
    /// and a focus the page asks for next shows it too.
    pub(crate) fn keyboard_used(&self, key: &Key) {
        let held = key.modifiers;
        if held.ctrl || held.alt || held.meta {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.dom.last_input = LastInput::Keyboard;
        if let Some(focused) = state.dom.focused
            && !state.dom.focus_visible
        {
            state.dom.focus_visible = true;
            state.dom.mark(focused);
        }
    }

    /// [`Document::key_down`], and whether the page left the key alone: neither prevented it nor
    /// ran an editing or navigation command on the keydown itself. A menu accelerator acts on such a
    /// key. What ran is read from the action ([`Document::acted`]), and "ran" means the command
    /// was enabled, not that it changed anything. Electron 39.8.10 (menu items on keys, the
    /// coordinator's `probe-keys` `keys`, `keys2` and `keys3` probes):
    ///
    /// - handled: a delete (also at the end of a field, where it deletes nothing), a caret move
    ///   (Up in an input or a textarea's first line too), select all, an undo or redo that was
    ///   enabled, Tab that moved the focus, Enter on a link, copy, cut and paste;
    /// - unhandled: typing a character, Enter (a line break, a form's submit, a button's press: they
    ///   come from `keypress`), Space (typing, or a button's press on `keyup`), Ctrl+Enter, Escape,
    ///   Backspace in a read-only field or on a checkbox, Ctrl+Z with nothing to undo, Ctrl+A on
    ///   the body (verve-dom selects nothing there).
    ///
    /// A key taken by an input method (`Process`) is handled: no accelerator matches it.
    pub fn key_down_unhandled(&self, runtime: &mut verve::Runtime, key: &Key) -> bool {
        let composing = self.is_composing();
        self.state.borrow_mut().key_acted = false;
        let allowed = self.key_down(runtime, key);
        let acted = self.state.borrow().key_acted;
        allowed && !acted && !composing
    }

    /// The keydown's default action ran a command: see [`Document::key_down_unhandled`].
    pub(crate) fn acted(&self, did: bool) {
        if did {
            self.state.borrow_mut().key_acted = true;
        }
    }
}

/// `node` and the elements it is inside, innermost first: what the pointer is over.
fn path_of(dom: &crate::Dom, node: Option<NodeId>) -> Vec<NodeId> {
    let mut path = Vec::new();
    let mut at = node;
    while let Some(id) = at {
        path.push(id);
        at = dom.parent_element(id);
    }
    path
}
