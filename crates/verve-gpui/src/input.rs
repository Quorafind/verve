//! What a person does to a window, as the page under it is told: the pointer, the keys, and the
//! modifiers.
//!
//! GPUI's events are turned into [`ToRendererInput`] by plain functions ([`press`], [`release`],
//! [`moved`], [`key_event`]) that a test can call with GPUI's structs and no window. The handlers
//! ([`mouse_down`] and the rest) add the shell's part: the node under the pointer found in the
//! display list, the cursor and link reported to the main process, and the order a key takes
//! ([`KeyRoute`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Keystroke, ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, NavigationDirection, Pixels, Point, Window,
};
use verve_dom::{Key, NodeId, Pointer};
use verve_electron::Hub;
use verve_electron::ids::ContentsId;
use verve_electron::msg::{FromShell, PageEvent, PointerKind, ToRendererInput};

use crate::dom_view::DomState;
use crate::{cursor, keymap, link, popup};

/// What the shell remembers of a person's hands between events.
#[derive(Default)]
pub(crate) struct Input {
    /// The buttons held, as `MouseEvent.buttons` has them.
    buttons: u16,
    /// The modifiers held, to find which one a change is.
    modifiers: gpui::Modifiers,
    /// The cursor name last reported (`None`: the arrow, which is not reported).
    cursor: Option<&'static str>,
    /// The address under the pointer last reported.
    link: Option<String>,
    /// The node the pointer was last over, so that nothing is worked out for a move that stays
    /// on it.
    over: Option<NodeId>,
    /// A press began in a drag area of the window: the system moves the window and eats the
    /// release, so the page hears neither.
    area_press: bool,
    /// A press went to the pop-up of a drop-down: the page hears neither it nor its release.
    popup_press: bool,
    /// The pointer is in a drag area, where the page has been told it left.
    in_area: bool,
    /// The characters keys typed that the system will send again as text, oldest first: they are
    /// dropped when they come. The system's thread may handle more keys before the text of the
    /// first arrives (GPUI drains input before the posted `WM_CHAR`), so this is a queue that
    /// only text takes from.
    typed: VecDeque<String>,
    /// Where a key goes (see [`KeyRoute`]).
    pub keys: Option<Rc<dyn Fn(bool, Key)>>,
    /// Where what the pointer shows goes: the main process.
    pub report: Option<Rc<dyn Fn(PageEvent)>>,
}

/// The most characters waited for; a text that never comes would otherwise fill the queue.
const TYPED_LIMIT: usize = 32;

impl Input {
    /// A key typed `text`, which the system will send again as text.
    pub fn note_typed(&mut self, text: String) {
        if self.typed.len() == TYPED_LIMIT {
            self.typed.pop_front();
        }
        self.typed.push_back(text);
    }

    /// Whether `text` is a character a key typed: it is then taken, so that the one the system
    /// sends as text does not type it twice. Characters older than it that never came are
    /// forgotten with it.
    pub fn is_typed(&mut self, text: &str) -> bool {
        let Some(at) = self.typed.iter().position(|typed| typed == text) else {
            return false;
        };
        self.typed.drain(..=at);
        true
    }
}

/// The DOM's number for a GPUI button: `MouseEvent.button`.
pub(crate) fn button_number(button: MouseButton) -> i16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Navigate(NavigationDirection::Back) => 3,
        MouseButton::Navigate(NavigationDirection::Forward) => 4,
    }
}

/// The bit of `MouseEvent.buttons` a button is.
pub(crate) fn button_mask(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Navigate(NavigationDirection::Back) => 8,
        MouseButton::Navigate(NavigationDirection::Forward) => 16,
    }
}

/// A point of the window in the page's CSS pixels, `origin` being the page's top left corner.
pub(crate) fn local(position: Point<Pixels>, origin: Point<Pixels>) -> (f32, f32) {
    let at = position - origin;
    (f32::from(at.x), f32::from(at.y))
}

fn pointer_of(
    position: Point<Pixels>,
    origin: Point<Pixels>,
    modifiers: &gpui::Modifiers,
) -> Pointer {
    let (x, y) = local(position, origin);
    Pointer {
        x,
        y,
        modifiers: keymap::modifiers(modifiers),
        ..Pointer::default()
    }
}

/// A button went down over `target`, with `buttons` held after it.
pub(crate) fn press(
    event: &MouseDownEvent,
    origin: Point<Pixels>,
    target: Option<NodeId>,
    buttons: u16,
) -> ToRendererInput {
    let pointer = Pointer {
        button: button_number(event.button),
        count: event.click_count as u32,
        buttons,
        ..pointer_of(event.position, origin, &event.modifiers)
    };
    ToRendererInput::Pointer {
        kind: PointerKind::Down,
        pointer,
        target,
    }
}

/// A button came up over `target`, with `buttons` still held.
pub(crate) fn release(
    event: &MouseUpEvent,
    origin: Point<Pixels>,
    target: Option<NodeId>,
    buttons: u16,
) -> ToRendererInput {
    let pointer = Pointer {
        button: button_number(event.button),
        count: event.click_count as u32,
        buttons,
        ..pointer_of(event.position, origin, &event.modifiers)
    };
    ToRendererInput::Pointer {
        kind: PointerKind::Up,
        pointer,
        target,
    }
}

/// The pointer moved over `target`, with the button GPUI says is held.
pub(crate) fn moved(
    event: &MouseMoveEvent,
    origin: Point<Pixels>,
    target: Option<NodeId>,
) -> ToRendererInput {
    let pointer = Pointer {
        buttons: event.pressed_button.map_or(0, button_mask),
        ..pointer_of(event.position, origin, &event.modifiers)
    };
    ToRendererInput::Pointer {
        kind: PointerKind::Move,
        pointer,
        target,
    }
}

/// A keystroke as the page and the text that follows it have it.
pub(crate) struct Heard {
    /// The key the page hears.
    pub key: Key,
    /// The character the key types by itself, which the system will send again as text.
    pub types: Option<String>,
}

/// What a keystroke is to the page, with the modifiers `now` held in the window and whether GPUI
/// says the character is the system's to compose (`prefer_character_input`). A dead key (an
/// accent that waits for its letter) is `Dead` and types nothing: the composed letter comes as
/// its own key, and an accent with nothing to join comes as text. `None` for a key that means
/// nothing to a page.
pub(crate) fn key_event(
    stroke: &Keystroke,
    held: bool,
    now: &gpui::Modifiers,
    prefer_character_input: bool,
) -> Option<Heard> {
    let key = keymap::key_with(stroke, held, now)?;
    let by_itself = !(key.modifiers.ctrl || key.modifiers.alt || key.modifiers.meta);
    // A dead key then Space is how a lone accent is typed (US-International `'` or `"`): GPUI
    // sends the key "space" with the accent as its key_char (ToUnicode composes it without
    // consuming the dead key). The page hears the accent, as it does for the composed letter.
    if stroke.key == "space"
        && by_itself
        && let Some(accent) = (stroke.key_char.as_deref()).filter(|c| *c != " " && !c.is_empty())
    {
        let key = Key {
            key: accent.to_owned(),
            ..key
        };
        let types = types(&key);
        return Some(Heard { key, types });
    }
    if prefer_character_input && by_itself && stroke.key_char.is_some() {
        return Some(Heard {
            key: keymap::dead(key),
            types: None,
        });
    }
    // A dead key and a letter it cannot take: GPUI's key_char holds both ("´x"). The page hears
    // the letter's key; the accent and the letter come as text.
    if key_char_is_many(stroke) && by_itself {
        let bare = Keystroke {
            key_char: None,
            ..stroke.clone()
        };
        let key = keymap::key_with(&bare, held, now)?;
        return Some(Heard { key, types: None });
    }
    let types = types(&key);
    Some(Heard { key, types })
}

/// Whether the key's text is more than one character: a dead key's accent and the letter it
/// could not take, which Windows composes into two.
fn key_char_is_many(stroke: &Keystroke) -> bool {
    stroke
        .key_char
        .as_deref()
        .is_some_and(|text| text.chars().count() > 1)
}

/// The text a key types by itself, which the system will send again as text: one character, with
/// neither Control, Alt nor the Windows key held (those are shortcuts, or AltGr, whose character
/// only the text carries).
pub(crate) fn types(key: &Key) -> Option<String> {
    let held = key.modifiers;
    let one = key.key.chars().count() == 1;
    (one && !held.ctrl && !held.alt && !held.meta).then(|| key.key.clone())
}

/// The topmost hitbox at `(x, y)` of the page: its node and the cursor it asks for.
pub(crate) fn hit(state: &mut DomState, x: f32, y: f32) -> Option<(NodeId, verve_layout::Cursor)> {
    let plan = state.plan()?;
    (plan.hitboxes.iter().rev())
        .find(|hitbox| hitbox.bounds.contains(x, y))
        .map(|hitbox| (hitbox.node, hitbox.cursor))
}

fn tell(state: &DomState, input: ToRendererInput) {
    if let Some(tell) = &state.tell {
        tell(input);
    }
}

/// Reports the cursor and the link under the pointer when they are not the ones last reported.
fn report_hover(state: &mut DomState, over: Option<(NodeId, verve_layout::Cursor)>) {
    let snapshot = state.snapshot.clone();
    let shown = over.map_or(verve_layout::Cursor::Default, |(node, asked)| {
        state.cursor_of(node, asked)
    });
    let name = cursor::name(shown);
    let url = over
        .and_then(|(node, _)| {
            snapshot
                .as_ref()
                .and_then(|snapshot| link::of(snapshot, node))
        })
        .unwrap_or_default();
    let mut events = Vec::new();
    if state.input.cursor.unwrap_or("pointer") != name {
        state.input.cursor = Some(name);
        events.push(PageEvent::CursorChanged(name.to_owned()));
    }
    if state.input.link.as_deref() != Some(&url) {
        state.input.link = Some(url.clone());
        events.push(PageEvent::UpdateTargetUrl(url));
    }
    if let Some(report) = &state.input.report {
        events.into_iter().for_each(|event| report(event));
    }
}

/// Whether `(x, y)` is in a part of the window that drags it (`-webkit-app-region: drag`). The
/// system takes presses there as a title bar's (`HTCAPTION`): it moves the window and the release
/// never comes to the app. Electron's frameless-window docs make controls `no-drag` to be
/// clickable, i.e. the page gets no mouse events in a drag region. From those docs and GPUI's
/// source (`gpui_windows` events.rs, the non-client press), not from observation: injected input
/// cannot reach a real title bar.
fn in_drag_area(state: &DomState, x: f32, y: f32) -> bool {
    state.drag_area.iter().any(|rect| {
        x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
    })
}

/// A button went down over the page.
pub(crate) fn mouse_down(
    state: &Rc<RefCell<DomState>>,
    event: &MouseDownEvent,
    origin: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut state = state.borrow_mut();
    if state.picking {
        return;
    }
    let (x, y) = local(event.position, origin);
    // An open pop-up takes the press: it chooses, or it closes and the page hears nothing. It
    // comes before the window's drag regions, so that a pop-up over one can be clicked.
    if popup::on_press(&mut state, x, y) {
        state.input.popup_press = true;
        window.refresh();
        return;
    }
    if in_drag_area(&state, x, y) {
        state.input.area_press = true;
        return;
    }
    // A click on the page puts the keyboard back on it, wherever GPUI had left it.
    if let Some(focus) = state.focus.clone() {
        window.focus(&focus, cx);
    }
    let target = hit(&mut state, x, y).map(|(node, _)| node);
    state.input.buttons |= button_mask(event.button);
    let buttons = state.input.buttons;
    tell(&state, press(event, origin, target, buttons));
}

/// A button came up over the page, or outside the window it was pressed in.
pub(crate) fn mouse_up(state: &Rc<RefCell<DomState>>, event: &MouseUpEvent, origin: Point<Pixels>) {
    let mut state = state.borrow_mut();
    if state.picking {
        return;
    }
    let (x, y) = local(event.position, origin);
    // The release of a press the pop-up took (it chose, or closed) is the pop-up's too.
    if std::mem::take(&mut state.input.popup_press) {
        return;
    }
    if std::mem::take(&mut state.input.area_press) {
        state.input.buttons &= !button_mask(event.button);
        return;
    }
    let target = hit(&mut state, x, y).map(|(node, _)| node);
    state.input.buttons &= !button_mask(event.button);
    let buttons = state.input.buttons;
    tell(&state, release(event, origin, target, buttons));
}

/// The pointer moved over the page.
pub(crate) fn mouse_move(
    state: &Rc<RefCell<DomState>>,
    event: &MouseMoveEvent,
    origin: Point<Pixels>,
    window: &mut Window,
) {
    let mut state = state.borrow_mut();
    if state.picking {
        return;
    }
    state.input.buttons = event.pressed_button.map_or(0, button_mask);
    let (x, y) = local(event.position, origin);
    if popup::on_move(&mut state, x, y) {
        window.refresh();
        return;
    }
    if in_drag_area(&state, x, y) {
        // One `Leave`, not a stream of moves: the page's hover ends where the window is dragged.
        if !std::mem::replace(&mut state.input.in_area, true) {
            state.input.over = None;
            let leave = ToRendererInput::Pointer {
                kind: PointerKind::Leave,
                pointer: Pointer::default(),
                target: None,
            };
            tell(&state, leave);
            report_hover(&mut state, None);
        }
        return;
    }
    state.input.in_area = false;
    let over = hit(&mut state, x, y);
    tell(&state, moved(event, origin, over.map(|(node, _)| node)));
    let node = over.map(|(node, _)| node);
    if state.input.over != node {
        state.input.over = node;
        report_hover(&mut state, over);
    }
}

/// The pointer left the window.
pub(crate) fn mouse_exit(state: &Rc<RefCell<DomState>>, _: &MouseExitEvent) {
    let mut state = state.borrow_mut();
    if state.picking {
        return;
    }
    let leave = ToRendererInput::Pointer {
        kind: PointerKind::Leave,
        pointer: Pointer::default(),
        target: None,
    };
    tell(&state, leave);
    state.input.over = None;
    report_hover(&mut state, None);
}

/// A key went down or came up. Answers whether an open pop-up took it, which means that the
/// window is to be drawn again.
pub(crate) fn key(
    state: &Rc<RefCell<DomState>>,
    stroke: &Keystroke,
    (down, held, prefer_character_input): (bool, bool, bool),
    now: &gpui::Modifiers,
) -> bool {
    let Some(heard) = key_event(stroke, held, now, prefer_character_input) else {
        return false;
    };
    let route = {
        let mut state = state.borrow_mut();
        if popup::on_key(&mut state, &heard.key, down, std::time::Instant::now()) {
            return true;
        }
        // The system sends a typed character as text only when a field takes text.
        let takes_text = (state.snapshot.as_ref()).is_some_and(crate::ime::accepts_ime);
        if let (true, true, Some(text)) = (down, takes_text, heard.types) {
            state.input.note_typed(text);
        }
        state.input.keys.clone()
    };
    if let Some(route) = route {
        route(down, heard.key);
    }
    false
}

/// A modifier went down or came up: the page hears its key.
pub(crate) fn modifiers_changed(state: &Rc<RefCell<DomState>>, event: &ModifiersChangedEvent) {
    let (changes, route) = {
        let mut state = state.borrow_mut();
        let changes = keymap::modifier_changes(&state.input.modifiers, &event.modifiers);
        state.input.modifiers = event.modifiers;
        (changes, state.input.keys.clone())
    };
    if let Some(route) = route {
        changes.into_iter().for_each(|(down, key)| route(down, key));
    }
}

/// The way a key takes to the page: a page whose `webContents` listens to `before-input-event`
/// has the key sent to the main process first, which asks the listeners and passes the key on;
/// any other page gets it at once. The menu's accelerators are not looked up here: the page
/// hears the key first, and the main process looks the menu up only for a key it left alone
/// (Electron 39.8.10, P15's contract as P32 settled it).
pub(crate) struct KeyRoute {
    hub: Arc<Hub>,
    contents: ContentsId,
}

impl KeyRoute {
    pub fn new(hub: Arc<Hub>, contents: ContentsId) -> Self {
        Self { hub, contents }
    }

    /// Sends a key where it goes.
    pub fn send(&self, down: bool, key: Key) {
        let contents = self.contents;
        if self.hub.intercepts_keys(contents) {
            self.hub.from_shell(FromShell::KeyForContents {
                contents,
                down,
                key,
            });
        } else {
            self.hub.input(contents, ToRendererInput::Key { down, key });
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    fn at(x: f32, y: f32) -> Point<Pixels> {
        point(px(x), px(y))
    }

    fn down(button: MouseButton, count: usize, modifiers: gpui::Modifiers) -> MouseDownEvent {
        MouseDownEvent {
            button,
            position: at(110., 70.),
            modifiers,
            click_count: count,
            first_mouse: false,
        }
    }

    fn pointer_of_input(input: ToRendererInput) -> (PointerKind, Pointer, Option<NodeId>) {
        match input {
            ToRendererInput::Pointer {
                kind,
                pointer,
                target,
            } => (kind, pointer, target),
            other => panic!("not a pointer: {other:?}"),
        }
    }

    #[test]
    fn the_buttons_are_the_doms_numbers_and_masks() {
        let back = MouseButton::Navigate(NavigationDirection::Back);
        let forward = MouseButton::Navigate(NavigationDirection::Forward);
        let numbers = [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            back,
            forward,
        ]
        .map(button_number);
        assert_eq!(numbers, [0, 1, 2, 3, 4]);
        let masks = [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            back,
            forward,
        ]
        .map(button_mask);
        assert_eq!(masks, [1, 2, 4, 8, 16]);
    }

    #[test]
    fn a_press_is_where_it_is_in_the_page_with_its_click_count_and_modifiers() {
        let held = gpui::Modifiers {
            shift: true,
            platform: true,
            ..gpui::Modifiers::default()
        };
        let event = down(MouseButton::Left, 2, held);
        let (kind, pointer, target) = pointer_of_input(press(&event, at(100., 50.), Some(9), 1));
        assert_eq!(kind, PointerKind::Down);
        assert_eq!((pointer.x, pointer.y), (10., 20.));
        assert_eq!((pointer.button, pointer.count, pointer.buttons), (0, 2, 1));
        assert!(pointer.modifiers.shift && pointer.modifiers.meta);
        assert!(!pointer.modifiers.ctrl && !pointer.modifiers.alt);
        assert_eq!(target, Some(9));
    }

    #[test]
    fn a_release_keeps_the_buttons_that_are_still_down() {
        let event = MouseUpEvent {
            button: MouseButton::Right,
            position: at(0., 0.),
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        };
        let (kind, pointer, target) = pointer_of_input(release(&event, at(0., 0.), None, 1));
        assert_eq!(kind, PointerKind::Up);
        assert_eq!((pointer.button, pointer.buttons), (2, 1));
        assert_eq!(target, None);
    }

    #[test]
    fn a_move_carries_the_button_held_as_a_mask() {
        let mut event = MouseMoveEvent {
            position: at(5., 6.),
            pressed_button: Some(MouseButton::Middle),
            modifiers: gpui::Modifiers::default(),
        };
        let (kind, pointer, _) = pointer_of_input(moved(&event, at(0., 0.), Some(3)));
        assert_eq!((kind, pointer.buttons), (PointerKind::Move, 4));
        event.pressed_button = None;
        let (_, pointer, _) = pointer_of_input(moved(&event, at(0., 0.), None));
        assert_eq!(pointer.buttons, 0);
    }

    fn stroke(key: &str, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    fn heard(stroke: &Keystroke, now: &gpui::Modifiers, prefer: bool) -> Heard {
        key_event(stroke, false, now, prefer).expect("a key a page hears")
    }

    #[test]
    fn a_key_that_types_a_character_says_so_unless_a_shortcut_or_the_system_composes_it() {
        let none = gpui::Modifiers::default();
        let ctrl = gpui::Modifiers {
            control: true,
            ..none
        };
        assert_eq!(
            heard(&stroke("a", Some("a")), &none, false)
                .types
                .as_deref(),
            Some("a")
        );
        assert_eq!(heard(&stroke("a", None), &ctrl, false).types, None);
        assert_eq!(heard(&stroke("enter", None), &none, false).types, None);
        // Shift is held in the window though GPUI cleared it from the keystroke.
        let shift = gpui::Modifiers {
            shift: true,
            ..none
        };
        let bang = heard(&stroke("!", Some("!")), &shift, false);
        assert!(bang.key.modifiers.shift);
        assert_eq!(bang.types.as_deref(), Some("!"));
    }

    #[test]
    fn a_dead_key_and_a_letter_it_cannot_take_is_the_letters_key_and_both_come_as_text() {
        let none = gpui::Modifiers::default();
        let pair = heard(&stroke("x", Some("´x")), &none, false);
        assert_eq!(
            (pair.key.key.as_str(), pair.key.code.as_str()),
            ("x", "KeyX")
        );
        assert_eq!(pair.types, None);
    }

    #[test]
    fn a_drag_area_is_where_the_page_hears_nothing() {
        let mut state = DomState::default();
        state.drag_area = vec![verve_electron::msg::Rect {
            x: 0.,
            y: 0.,
            width: 100.,
            height: 30.,
        }];
        assert!(in_drag_area(&state, 50., 10.));
        assert!(!in_drag_area(&state, 50., 30.));
        assert!(!in_drag_area(&state, 100., 10.));
    }

    #[test]
    fn a_dead_key_then_space_types_the_accent_once() {
        let none = gpui::Modifiers::default();
        // The shape gpui_windows sends for the second key: "space" carrying the accent.
        let accent = heard(&stroke("space", Some("´")), &none, true);
        assert_eq!(accent.key.key, "´");
        assert_eq!(accent.key.code, "Space");
        assert_eq!(accent.types.as_deref(), Some("´"));
        // A plain Space is still a space.
        let plain = heard(&stroke("space", Some(" ")), &none, false);
        assert_eq!(plain.key.key, " ");
        assert_eq!(plain.types.as_deref(), Some(" "));
    }

    #[test]
    fn a_dead_key_is_dead_for_the_page_and_types_nothing_but_the_letter_after_it_does() {
        let none = gpui::Modifiers::default();
        // GPUI on Windows: the accent is the key_char and the character is the system's to compose.
        let accent = heard(&stroke("'", Some("'")), &none, true);
        assert_eq!(
            (accent.key.key.as_str(), accent.key.code.as_str()),
            ("Dead", "Quote")
        );
        assert_eq!(accent.types, None);
        // The next key arrives composed: "é" on the code of E, typed by the page, and its text
        // is dropped like any other typed character.
        let composed = heard(&stroke("e", Some("é")), &none, false);
        assert_eq!(composed.key.key, "é");
        assert_eq!(composed.types.as_deref(), Some("é"));
        // AltGr (Control and Alt) is a shortcut to the page: the text carries the character.
        let altgr = gpui::Modifiers {
            control: true,
            alt: true,
            ..none
        };
        let at = heard(&stroke("q", Some("@")), &altgr, true);
        assert_eq!(at.key.key, "@");
        assert_eq!(at.types, None);
    }

    #[test]
    fn text_that_comes_late_is_still_dropped_once_per_typed_character() {
        // GPUI drains the keys before the posted WM_CHAR: down a, down b, up a, text a, text b.
        let mut input = Input::default();
        input.note_typed("a".to_owned());
        input.note_typed("b".to_owned());
        // (the key ups change nothing)
        assert!(input.is_typed("a"));
        assert!(input.is_typed("b"));
        // An input method's commit of the same letter is not a typed one.
        assert!(!input.is_typed("a"));
    }

    #[test]
    fn text_that_never_came_is_forgotten_when_a_later_one_arrives() {
        let mut input = Input::default();
        input.note_typed("a".to_owned());
        input.note_typed("b".to_owned());
        // The text of "a" was lost: "b" arrives, and "a" goes with it.
        assert!(input.is_typed("b"));
        assert!(!input.is_typed("a"));
    }

    #[test]
    fn a_stroke_with_no_key_a_page_knows_is_not_sent() {
        let none = gpui::Modifiers::default();
        assert!(key_event(&stroke("shift", None), false, &none, false).is_none());
        let held = key_event(&stroke("enter", None), true, &none, false).expect("a key");
        assert!(held.key.repeat);
        assert_eq!(held.key.key, "Enter");
    }
}
