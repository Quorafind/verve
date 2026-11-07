//! Input-method composition: text an input method is still deciding on, shown in a text field or
//! an editable region, and the events around it.
//!
//! The order is Electron 39.8.10's, driven through `Input.imeSetComposition` and
//! `Input.insertText` (see `tests/ime.rs`). A field composes in its [`crate::Draft`]'s marked
//! text; an editable region composes into a text node at the caret (a text node already there is reused,
//! otherwise one is made), which scripts see; what is composed stays when the composition is
//! committed and goes when it is cancelled. A commit joins the host's undo history.

use std::ops::Range;

use boa_engine::{Context, JsResult, JsValue};
use verve::native::js_text;

use crate::bind::events::{fire_at, fire_with};
use crate::bind::node::{read_dom, with_dom, wrap};
use crate::dom::{Direction, Dom, Edit, NodeId, RangeData};
use crate::editing::{Step, push_step, set_target_ranges};
use crate::{Document, Key, Rect};

/// What is being composed and where.
pub(crate) struct Composition {
    /// The element the events go to: the text field, or the editing host.
    target: NodeId,
    /// The text node an editable region composes in, and the UTF-16 range of it that is the
    /// composition; none in a field.
    node: Option<NodeId>,
    start: usize,
    len: usize,
    /// The selection before the composition began, and the edits it made to the tree: a commit
    /// is one step of the host's undo history.
    before: Option<(RangeData, Direction)>,
    log: Vec<Edit>,
    /// The selection's revision when the composition began: the step the commit joins is the
    /// one open at that moment.
    started: u64,
}

fn composition_event(cx: &mut Context, target: NodeId, kind: &str, data: &str) -> JsResult<()> {
    let init = [("data", js_text(data)), ("cancelable", true.into())];
    fire_at(cx, target, "CompositionEvent", kind, &init, true)?;
    Ok(())
}

/// `beforeinput` and `input` for a composition step. `null` data is how a cancelled one is told.
fn input_event(
    cx: &mut Context,
    target: NodeId,
    kind: &str,
    data: Option<&str>,
    ranges: &[RangeData],
) -> JsResult<()> {
    let init = [
        ("inputType", js_text("insertCompositionText")),
        ("data", data.map_or(JsValue::null(), js_text)),
        ("isComposing", true.into()),
        ("cancelable", false.into()),
    ];
    let target = wrap(cx, target);
    fire_with(cx, &target, "InputEvent", kind, &init, true, |cx, event| {
        if kind == "beforeinput" {
            set_target_ranges(cx, event, ranges)?;
        }
        Ok(())
    })?;
    Ok(())
}

impl Document {
    /// The element a composition would go to: the text field with the keyboard, or the editable
    /// region it is in.
    fn composition_target(&self) -> Option<NodeId> {
        let target = self.key_target();
        if self.is_text_field(target) {
            return Some(target);
        }
        self.editable_host(target)
    }

    /// Whether an input method is composing.
    pub fn is_composing(&self) -> bool {
        self.state.borrow().editing.composition.is_some()
    }

    /// An input method is composing `text` at the caret, replacing what it composed before;
    /// `selection` is where its caret sits inside `text`, in UTF-16.
    pub fn compose(
        &self,
        runtime: &mut verve::Runtime,
        text: &str,
        selection: Option<Range<usize>>,
    ) {
        let cx = runtime.context();
        if let Err(error) = self.compose_in(cx, text, selection) {
            verve::report(cx, &error);
        }
        runtime.run_microtasks();
    }

    /// The element the running composition's events go to.
    fn composing_target(&self) -> Option<NodeId> {
        self.state
            .borrow()
            .editing
            .composition
            .as_ref()
            .map(|c| c.target)
    }

    /// The text node of a region's composition, and the UTF-16 range of it that is composed.
    fn held(&self) -> Option<(NodeId, usize, usize)> {
        let state = self.state.borrow();
        let c = state.editing.composition.as_ref()?;
        c.node.map(|n| (n, c.start, c.len))
    }

    fn compose_in(
        &self,
        cx: &mut Context,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> JsResult<()> {
        let started = self.is_composing();
        let Some(target) = (if started {
            self.composing_target()
        } else {
            self.composition_target()
        }) else {
            return Ok(());
        };
        if text.is_empty() {
            return self.end_in(cx, target, "", false);
        }
        if !started {
            if !self.is_text_field(target) {
                crate::editing::ensure_selection(cx, target);
            }
            let before = read_dom(cx, Dom::selection);
            let started = read_dom(cx, |dom| dom.ranges.revision);
            self.state.borrow_mut().editing.composition = Some(Composition {
                target,
                node: None,
                start: 0,
                len: 0,
                before,
                log: Vec::new(),
                started,
            });
            composition_event(cx, target, "compositionstart", "")?;
        }
        composition_event(cx, target, "compositionupdate", text)?;
        let ranges = self.composition_range(cx);
        input_event(cx, target, "beforeinput", Some(text), &ranges)?;
        self.write_composition(cx, target, text, selection);
        input_event(cx, target, "input", Some(text), &[])?;
        if self.is_text_field(target) {
            announce_field(cx, target);
        }
        Ok(())
    }

    /// The range the next `beforeinput` of the composition tells: what is composed so far, or
    /// the caret. Nothing for a field.
    fn composition_range(&self, cx: &mut Context) -> Vec<RangeData> {
        if self
            .composing_target()
            .is_some_and(|t| self.is_text_field(t))
        {
            return Vec::new();
        }
        let held = self.held();
        read_dom(cx, |dom| match held {
            Some((n, start, len)) => vec![RangeData {
                start: (n, start),
                end: (n, start + len),
            }],
            None => dom.selection().map(|(r, _)| vec![r]).unwrap_or_default(),
        })
    }

    /// The composed text, written where it goes.
    fn write_composition(
        &self,
        cx: &mut Context,
        target: NodeId,
        text: &str,
        selection: Option<Range<usize>>,
    ) {
        if self.is_text_field(target) {
            with_dom(cx, |dom| {
                crate::bind::element::ensure_draft(dom, target);
                if let Some(draft) = dom.get_mut(target).and_then(|n| n.draft.as_mut()) {
                    draft.replace_and_mark_utf16(None, text, selection);
                }
            });
            return;
        }
        let held = self.held();
        let ((node, start, units), log) = with_dom(cx, |dom| {
            dom.recording(|dom| {
                let (node, start, len) = composition_spot(dom, held, target);
                dom.replace_data(node, start, len, text);
                let units = crate::dom::utf16_len(text);
                let chosen = selection.unwrap_or(units..units);
                let (from, to) = (chosen.start.min(units), chosen.end.min(units));
                dom.select_between((node, start + from), (node, start + to));
                (node, start, units)
            })
        });
        if let Some(c) = self.state.borrow_mut().editing.composition.as_mut() {
            c.node = Some(node);
            c.start = start;
            c.len = units;
            c.log.extend(log);
        }
    }

    /// The composition, committed as `text` (which may differ from the last composed text).
    pub fn commit_composition(&self, runtime: &mut verve::Runtime, text: &str) {
        let cx = runtime.context();
        let active = self.composing_target();
        let result = match active {
            Some(target) => self.end_in(cx, target, text, true),
            None => Ok(()),
        };
        if let Err(error) = result {
            verve::report(cx, &error);
        }
        if active.is_none() {
            self.insert_text(runtime, text);
        }
        runtime.run_microtasks();
    }

    /// The composition, abandoned (Escape, focus lost): what was composed is removed.
    pub fn cancel_composition(&self, runtime: &mut verve::Runtime) {
        let cx = runtime.context();
        let active = self.composing_target();
        if let Some(target) = active
            && let Err(error) = self.end_in(cx, target, "", false)
        {
            verve::report(cx, &error);
        }
        runtime.run_microtasks();
    }

    /// Ends the composition with `text`: the last update, the change, `compositionend`.
    fn end_in(
        &self,
        cx: &mut Context,
        target: NodeId,
        text: &str,
        committed: bool,
    ) -> JsResult<()> {
        if !self.is_composing() {
            return Ok(());
        }
        composition_event(cx, target, "compositionupdate", text)?;
        let ranges = self.composition_range(cx);
        input_event(cx, target, "beforeinput", Some(text), &ranges)?;
        let field = self.is_text_field(target);
        if field {
            with_dom(cx, |dom| {
                crate::bind::element::ensure_draft(dom, target);
                if let Some(draft) = dom.get_mut(target).and_then(|n| n.draft.as_mut()) {
                    draft.replace_utf16(None, text);
                }
            });
        } else if let Some((node, start, len)) = self.held() {
            let ((), log) = with_dom(cx, |dom| {
                dom.recording(|dom| finish_node(dom, node, (start, len), text))
            });
            if let Some(c) = self.state.borrow_mut().editing.composition.as_mut() {
                c.log.extend(log);
            }
        }
        let finished = self.state.borrow_mut().editing.composition.take();
        if let Some(c) = finished.filter(|_| committed && !field) {
            remember(cx, c);
        }
        input_event(
            cx,
            target,
            "input",
            (committed || !text.is_empty()).then_some(text),
            &[],
        )?;
        composition_event(cx, target, "compositionend", text)?;
        if field {
            announce_field(cx, target);
        }
        Ok(())
    }

    /// Where an input method puts its candidate window: the caret's box, in viewport
    /// coordinates. The box of what is composed or selected when the page's layout can say, the
    /// box of the field or region otherwise.
    pub fn caret_rect(&self) -> Option<Rect> {
        self.ensure_layout();
        let key = {
            let state = self.state.borrow();
            let target = self.composition_target_in(&state)?;
            (target, state.dom.ranges.revision, state.laid)
        };
        if let Some((known, rect)) = self.state.borrow().editing.caret
            && known == key
        {
            return rect;
        }
        let rect = self.caret_rect_of(key.0);
        self.state.borrow_mut().editing.caret = Some((key, rect));
        rect
    }

    fn caret_rect_of(&self, target: NodeId) -> Option<Rect> {
        let state = self.state.borrow();
        let range = state.dom.selection().map(|(r, _)| r);
        if let (Some(range), Some(hook)) = (range, state.range_hook.clone())
            && !state.dom.is_character_data(target)
            && state.dom.contains(target, range.start.0)
            && let Some(rect) = hook(range.start, range.end).into_iter().next()
        {
            return Some(rect);
        }
        state.rects.get(&target).copied()
    }

    fn composition_target_in(&self, state: &crate::state::State) -> Option<NodeId> {
        if let Some(c) = state.editing.composition.as_ref() {
            return Some(c.target);
        }
        let focused = state.dom.focused.filter(|f| state.dom.is_connected(*f))?;
        state.dom.editing_host(focused).or(Some(focused))
    }

    /// A key during a composition: `keydown` is `Process` with `keyCode` 229, both key events
    /// carry `isComposing`, and nothing else happens. `None` when nothing is composing.
    pub(crate) fn composing_key(
        &self,
        runtime: &mut verve::Runtime,
        key: &Key,
        down: bool,
    ) -> Option<bool> {
        let target = self.composing_target()?;
        let mut init = Self::key_init(key, 0);
        init.push(("isComposing", true.into()));
        if down {
            init.retain(|(k, _)| !matches!(*k, "key" | "keyCode" | "which"));
            init.push(("key", js_text("Process")));
            init.push(("keyCode", 229.into()));
            init.push(("which", 229.into()));
        }
        let kind = if down { "keydown" } else { "keyup" };
        let allowed = self.fire(runtime, target, "KeyboardEvent", kind, &init);
        runtime.run_microtasks();
        Some(allowed)
    }
}

/// An editable region's composed text, finished: `text` is what stays in place of the
/// composition, and the caret goes after it. A node left with nothing is removed.
fn finish_node(dom: &mut Dom, node: NodeId, (start, len): (usize, usize), text: &str) {
    if !dom.exists(node) {
        return;
    }
    dom.replace_data(node, start, len, text);
    let end = (node, start + crate::dom::utf16_len(text));
    if dom.node_length(node) == 0 {
        let spot = dom.before_node(node);
        dom.detach(node);
        if let Some(at) = spot {
            dom.select_between(at, at);
        }
        return;
    }
    dom.rebalance_spaces(node, end.1);
    dom.select_between(end, end);
}

/// Queues `selectionchange` at a text field whose caret a composition moved. Electron 39.8.10
/// fires it at the `<input>` or `<textarea>` itself (bubbling, not composed), queued after the
/// events of the step, so after the microtasks their listeners queued; the document hears nothing for a
/// field.
fn announce_field(cx: &mut Context, field: NodeId) {
    let state = crate::state::shared(cx);
    let first = {
        let mut state = state.borrow_mut();
        let first = state.editing.field_changes.is_empty();
        if !state.editing.field_changes.contains(&field) {
            state.editing.field_changes.push(field);
        }
        first
    };
    if first {
        crate::bind::queue_microtask(cx, "selectionchange", |_, _, cx| {
            let fields =
                std::mem::take(&mut crate::state::shared(cx).borrow_mut().editing.field_changes);
            for field in fields {
                fire_at(
                    cx,
                    field,
                    "Event",
                    "selectionchange",
                    &[("bubbles", true.into()), ("composed", false.into())],
                    true,
                )?;
            }
            Ok(JsValue::undefined())
        });
    }
}

/// Where the composed text goes: the text node and range of the composition so far, or, at its
/// start, the selection's place (what is selected is deleted first).
fn composition_spot(
    dom: &mut Dom,
    held: Option<(NodeId, usize, usize)>,
    target: NodeId,
) -> (NodeId, usize, usize) {
    if let Some(held) = held.filter(|(n, _, _)| dom.exists(*n)) {
        return held;
    }
    let at = match dom.selection().map(|(r, _)| r) {
        Some(r) if r.start != r.end => dom.delete_between(r),
        Some(r) => r.start,
        None => dom.caret_in(target),
    };
    let (node, start) = dom.text_spot(at);
    (node, start, 0)
}

/// A commit, as one step of the host's undo history, which typing before it may join.
fn remember(cx: &Context, composition: Composition) {
    let edited = read_dom(cx, |dom| {
        composition
            .log
            .iter()
            .any(|edit| dom.is_connected(edit.target()))
    });
    if edited {
        let after = read_dom(cx, Dom::selection);
        let step = Step::new(composition.log, composition.before, after);
        let started = (composition.started, true);
        push_step(cx, composition.target, "insertText", started, step);
    }
}

/// The focus is leaving `leaving` while an input method is composing in it: the composition ends
/// as it stands, with `compositionend` and nothing else (Electron 39.8.10, the p32-review2
/// `imeblur` probe: the composed text stays, `compositionend` comes before `blur`, no further
/// update and no `input`). Without this the composition would stay open and every later key would
/// be taken as a `Process` key of the field the focus left.
pub(crate) fn end_for_blur(cx: &mut Context, leaving: NodeId) -> JsResult<()> {
    let shared = crate::state::shared(cx);
    let open = {
        let state = shared.borrow();
        let c = state
            .editing
            .composition
            .as_ref()
            .filter(|c| c.target == leaving);
        c.map(|c| (c.node, c.start, c.len))
    };
    let Some((node, start, len)) = open else {
        return Ok(());
    };
    let text = with_dom(cx, |dom| match node {
        Some(node) => {
            let all: Vec<u16> = dom.text_content(node).encode_utf16().collect();
            let end = (start + len).min(all.len());
            String::from_utf16_lossy(&all[start.min(end)..end])
        }
        None => {
            let draft = dom.get_mut(leaving).and_then(|n| n.draft.as_mut());
            draft.map_or_else(String::new, |draft| {
                let spelled = draft.marked().map(|range| draft.text()[range].to_owned());
                draft.unmark();
                spelled.unwrap_or_default()
            })
        }
    });
    shared.borrow_mut().editing.composition = None;
    composition_event(cx, leaving, "compositionend", &text)
}
