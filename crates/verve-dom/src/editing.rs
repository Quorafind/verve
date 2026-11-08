//! Editing an editable region the way a browser does: what the keys do, the events around each
//! change, the undo history of a host, and `execCommand`.
//!
//! Event order and fields are Electron 39.8.10's (see `tests/editing.rs`). A key that edits is
//! `keydown`, `keypress`, a cancelable `beforeinput` at the editing host with the ranges it will
//! change in `getTargetRanges()`, the change, `input`. `document.execCommand` from a script
//! fires `input` alone. The tree is changed by [`crate::dom`]'s editing functions; everything
//! here is the order things happen in and the bookkeeping around it.

use std::collections::HashMap;

use boa_engine::{Context, JsResult, JsValue, js_string};
use verve::native::{self, hidden, js_text};

use crate::bind::events::{fire_at, fire_with};
use crate::bind::node::{read_dom, with_dom, wrap};
use crate::bind::range::static_range;
use crate::dom::{Direction, Dom, Edit, NodeId, RangeData};
use crate::layout_provider::Boundary;
use crate::selection::Granularity;
use crate::{Document, Key};

/// One change of a host, to undo: the mutations it made and the selection around it.
pub(crate) struct Step {
    log: Vec<Edit>,
    before: Option<(RangeData, Direction)>,
    after: Option<(RangeData, Direction)>,
}

/// The most steps a history keeps; the oldest go first (Electron 39.8.10 allows 1000 undos).
const LIMIT: usize = 1000;

/// A host's undo and redo stacks, and the step that typing may still join.
#[derive(Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The input type of the last step and the selection's revision it left: the step stays
    /// open while the selection is where it left it (Chromium's "open typing" command).
    open: Option<(&'static str, u64)>,
}

/// Whether a change of `kind` goes on from an open step of kind `open`: typing joins any open
/// step, a delete only the same kind of delete.
fn joins(open: &str, kind: &str) -> bool {
    match kind {
        "insertText" | "insertParagraph" | "insertLineBreak" => true,
        "deleteContentBackward" | "deleteContentForward" => open == kind,
        _ => false,
    }
}

/// The kind of step a change of `input_type` is, for joining; empty for one that never joins.
fn step_kind(input_type: &str) -> &'static str {
    match input_type {
        "insertText" => "insertText",
        "insertParagraph" => "insertParagraph",
        "insertLineBreak" => "insertLineBreak",
        "deleteContentBackward" => "deleteContentBackward",
        "deleteContentForward" => "deleteContentForward",
        _ => "",
    }
}

/// What editing keeps between calls.
#[derive(Default)]
pub struct EditState {
    histories: HashMap<NodeId, History>,
    /// `defaultParagraphSeparator`: `div` unless a script chose `p`.
    separator: Option<String>,
    pub(crate) composition: Option<crate::ime::Composition>,
    /// The box last worked out for the input method's candidate window, and what it was for:
    /// the target, the selection's revision and the layout's key.
    pub(crate) caret: Option<CaretBox>,
    /// Text fields whose selection moved and have yet to be told so (`selectionchange`).
    pub(crate) field_changes: Vec<NodeId>,
}

/// What a caret box was worked out for, and the box.
pub(crate) type CaretBox = (CaretKey, Option<crate::Rect>);
/// The target, the selection's revision and the layout it was worked out for.
pub(crate) type CaretKey = (NodeId, u64, Option<(u64, (f32, f32))>);

fn separator(cx: &Context) -> String {
    crate::state::shared(cx)
        .borrow()
        .editing
        .separator
        .clone()
        .unwrap_or_else(|| "div".to_owned())
}

/// The input type a change is told as, and what it needs to know to run.
struct Change<'a> {
    input_type: &'a str,
    data: Option<&'a str>,
    /// Whether `beforeinput` is fired first (a person's change, not a script's command).
    user: bool,
    /// The ranges `getTargetRanges()` answers.
    targets: Vec<RangeData>,
    /// The `dataTransfer` the events carry (a paste), else `null`.
    transfer: Option<boa_engine::JsObject>,
}

fn selection_of(cx: &Context) -> Option<(RangeData, Direction)> {
    read_dom(cx, Dom::selection)
}

/// The editing host the selection is in, if it is in one.
fn selected_host(cx: &Context) -> Option<NodeId> {
    read_dom(cx, |dom| {
        let (range, _) = dom.selection()?;
        let host = dom.editing_host(range.start.0)?;
        (dom.editing_host(range.end.0) == Some(host)).then_some(host)
    })
}

/// Puts the caret at the start of `host` when the selection is not in it.
pub(crate) fn ensure_selection(cx: &mut Context, host: NodeId) {
    let inside = read_dom(cx, |dom| {
        dom.selection()
            .is_some_and(|(r, _)| dom.contains(host, r.start.0) && dom.contains(host, r.end.0))
    });
    if !inside {
        with_dom(cx, |dom| {
            let at = dom.caret_in(host);
            dom.select_between(at, at);
        });
    }
}

/// Gives `event` the ranges its `getTargetRanges()` answers, in a hidden list.
pub(crate) fn set_target_ranges(
    cx: &mut Context,
    event: &boa_engine::JsObject,
    ranges: &[RangeData],
) -> JsResult<()> {
    let items = ranges
        .iter()
        .map(|r| static_range(cx, *r))
        .collect::<JsResult<Vec<_>>>()?;
    let list = native::array(cx, items);
    hidden(event, js_string!("__targetRanges"), list, cx)
}

fn fire_before(cx: &mut Context, host: NodeId, change: &Change<'_>) -> JsResult<bool> {
    let target = wrap(cx, host);
    let init = [
        ("inputType", js_text(change.input_type)),
        ("data", change.data.map_or(JsValue::null(), js_text)),
        (
            "dataTransfer",
            change.transfer.clone().map_or(JsValue::null(), Into::into),
        ),
    ];
    let (allowed, event) = fire_with(
        cx,
        &target,
        "InputEvent",
        "beforeinput",
        &init,
        true,
        |cx, event| set_target_ranges(cx, event, &change.targets),
    )?;
    // Once dispatched, the event no longer names ranges (Electron 39.8.10).
    set_target_ranges(cx, &event, &[])?;
    Ok(allowed)
}

fn fire_input(cx: &mut Context, host: NodeId, change: &Change<'_>) -> JsResult<()> {
    let target = wrap(cx, host);
    // `input` carries no `dataTransfer` (Electron 39.8.10), only `beforeinput` does.
    let init = [
        ("inputType", js_text(change.input_type)),
        ("data", change.data.map_or(JsValue::null(), js_text)),
    ];
    fire_with(cx, &target, "InputEvent", "input", &init, true, |_, _| {
        Ok(())
    })?;
    Ok(())
}

/// Puts a finished change in the host's history. A change that starts where the open step left
/// the selection (`started` is the selection's revision then) joins it, so one undo takes both
/// back; `close` ends the step after it (an input method's commit). Histories of hosts that left
/// the document are dropped.
pub(crate) fn push_step(
    cx: &Context,
    host: NodeId,
    kind: &'static str,
    (started, close): (u64, bool),
    step: Step,
) {
    let hosts: Vec<NodeId> = crate::state::shared(cx)
        .borrow()
        .editing
        .histories
        .keys()
        .copied()
        .collect();
    let (gone, revision) = read_dom(cx, |dom| {
        let gone: Vec<NodeId> = hosts
            .into_iter()
            .filter(|h| !dom.is_connected(*h))
            .collect();
        (gone, dom.ranges.revision)
    });
    let state = crate::state::shared(cx);
    let mut state = state.borrow_mut();
    for host in gone {
        state.editing.histories.remove(&host);
    }
    let history = state.editing.histories.entry(host).or_default();
    let joined = history
        .open
        .is_some_and(|(open, at)| at == started && joins(open, kind));
    history.redo.clear();
    match history.undo.last_mut() {
        Some(last) if joined => {
            last.log.extend(step.log);
            last.after = step.after;
        }
        _ => {
            history.undo.push(step);
            if history.undo.len() > LIMIT {
                history.undo.remove(0);
            }
        }
    }
    history.open = (!kind.is_empty() && !close).then_some((kind, revision));
}

impl Step {
    pub(crate) fn new(
        log: Vec<Edit>,
        before: Option<(RangeData, Direction)>,
        after: Option<(RangeData, Direction)>,
    ) -> Self {
        Self { log, before, after }
    }
}

/// Runs a change: `beforeinput` for a person's, then `run` on the tree with the range to change
/// (it answers where the selection goes), then the undo bookkeeping and `input`. `range` is the
/// one planned; a handler of `beforeinput` that moved the selection has the change follow it, as
/// Chromium re-reads the selection after dispatching. Answers whether the change was made.
fn perform(
    cx: &mut Context,
    host: NodeId,
    change: Change<'_>,
    range: RangeData,
    run: impl FnOnce(&mut Dom, RangeData) -> Option<(Boundary, Boundary)>,
) -> JsResult<bool> {
    perform_planned(cx, host, change, range, &|_, now| now, run)
}

/// [`perform`] for a change whose range comes from the selection by a rule, `plan`: a delete
/// extends a caret by its unit. When a `beforeinput` handler moved the selection, the rule is
/// applied to the new selection (Electron 39.8.10: a caret moved to 2 deletes the character
/// before 2, though the event said 3-4).
fn perform_planned(
    cx: &mut Context,
    host: NodeId,
    change: Change<'_>,
    range: RangeData,
    plan: &dyn Fn(&Dom, RangeData) -> RangeData,
    run: impl FnOnce(&mut Dom, RangeData) -> Option<(Boundary, Boundary)>,
) -> JsResult<bool> {
    let seen = selection_of(cx).map(|(r, _)| r);
    if change.user && !fire_before(cx, host, &change)? {
        return Ok(false);
    }
    let now = selection_of(cx).map(|(r, _)| r);
    let range = match now {
        Some(now) if Some(now) != seen => read_dom(cx, |dom| plan(dom, now)),
        _ => range,
    };
    let kind = step_kind(change.input_type);
    let started = read_dom(cx, |dom| dom.ranges.revision);
    let (log, before, after) = with_dom(cx, |dom| {
        let before = dom.selection();
        let (landing, log) = dom.recording(|dom| run(dom, range));
        if let Some((anchor, focus)) = landing {
            dom.select_between(anchor, focus);
        }
        (log, before, dom.selection())
    });
    // A change that only touched nodes outside the tree is no change.
    if !read_dom(cx, |dom| log.iter().any(|e| dom.is_connected(e.target()))) {
        return Ok(false);
    }
    push_step(
        cx,
        host,
        kind,
        (started, false),
        Step::new(log, before, after),
    );
    fire_input(cx, host, &change)?;
    Ok(true)
}

/// Undoes (or redoes) the host's last step.
fn travel(cx: &mut Context, host: NodeId, redo: bool, user: bool) -> JsResult<bool> {
    let state = crate::state::shared(cx);
    let input_type = if redo { "historyRedo" } else { "historyUndo" };
    let available = {
        let state = state.borrow();
        state.editing.histories.get(&host).is_some_and(|h| {
            if redo {
                !h.redo.is_empty()
            } else {
                !h.undo.is_empty()
            }
        })
    };
    if !available {
        return Ok(false);
    }
    let targets = selection_of(cx).map(|(r, _)| vec![r]).unwrap_or_default();
    let change = Change {
        input_type,
        data: None,
        user,
        targets,
        transfer: None,
    };
    if user && !fire_before(cx, host, &change)? {
        return Ok(false);
    }
    let step = {
        let mut state = state.borrow_mut();
        let history = state.editing.histories.entry(host).or_default();
        history.open = None;
        if redo {
            history.redo.pop()
        } else {
            history.undo.pop()
        }
    };
    let Some(step) = step else { return Ok(false) };
    let inverse = with_dom(cx, |dom| {
        let log = dom.revert(&step.log);
        if let Some((range, direction)) = step.before
            && dom.exists(range.start.0)
            && dom.exists(range.end.0)
        {
            dom.set_selection(range, direction);
        }
        Step {
            log,
            before: step.after,
            after: step.before,
        }
    });
    {
        let mut state = state.borrow_mut();
        let history = state.editing.histories.entry(host).or_default();
        let stack = if redo {
            &mut history.undo
        } else {
            &mut history.redo
        };
        stack.push(inverse);
        if stack.len() > LIMIT {
            stack.remove(0);
        }
    }
    fire_input(cx, host, &change)?;
    Ok(true)
}

// ─── The changes ────────────────────────────────────────────────────────────────────────────

/// The range a change replaces: the selection, which a caret makes empty.
fn replaced(cx: &Context) -> Option<RangeData> {
    selection_of(cx).map(|(r, _)| r)
}

fn insert_text(cx: &mut Context, host: NodeId, text: &str, user: bool) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    let plain = read_dom(cx, |dom| dom.is_plaintext_only(host));
    let targets = if plain { Vec::new() } else { vec![range] };
    let owned = text.to_owned();
    perform(
        cx,
        host,
        Change {
            input_type: "insertText",
            data: Some(text),
            user,
            targets,
            transfer: None,
        },
        range,
        move |dom, range| {
            let at = dom.delete_between(range);
            let caret = if plain {
                let (node, offset) = dom.insert_plain_at(at, &owned);
                (node, offset)
            } else {
                dom.insert_text_at(at, &owned)
            };
            Some((caret, caret))
        },
    )
}

fn insert_paragraph(cx: &mut Context, host: NodeId, user: bool) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    let plain = read_dom(cx, |dom| dom.is_plaintext_only(host));
    if plain {
        return insert_line_break(cx, host, user);
    }
    let separator = separator(cx);
    perform(
        cx,
        host,
        Change {
            input_type: "insertParagraph",
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            let at = dom.delete_between(range);
            let caret = dom.split_block_at(at, host, &separator);
            Some((caret, caret))
        },
    )
}

fn insert_line_break(cx: &mut Context, host: NodeId, user: bool) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    let plain = read_dom(cx, |dom| dom.is_plaintext_only(host));
    let targets = if plain { Vec::new() } else { vec![range] };
    perform(
        cx,
        host,
        Change {
            input_type: "insertLineBreak",
            data: None,
            user,
            targets,
            transfer: None,
        },
        range,
        move |dom, range| {
            let at = dom.delete_between(range);
            let caret = if plain {
                dom.insert_plain_at(at, "\n")
            } else {
                dom.insert_break_at(at)
            };
            Some((caret, caret))
        },
    )
}

/// Backspace and Delete (and their word forms, and `delete` / `forwardDelete`).
fn delete(
    cx: &mut Context,
    host: NodeId,
    forward: bool,
    unit: Granularity,
    user: bool,
) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    // A caret is extended by the unit, in the direction of the key.
    let plan = move |dom: &Dom, range: RangeData| {
        if range.start != range.end {
            return range;
        }
        let to = dom.step_boundary(range.start, unit, forward, Some(host));
        if forward {
            RangeData {
                start: range.start,
                end: to,
            }
        } else {
            RangeData {
                start: to,
                end: range.start,
            }
        }
    };
    let range = read_dom(cx, |dom| plan(dom, range));
    if range.start == range.end {
        return Ok(false);
    }
    let input_type = match (forward, unit == Granularity::Word) {
        (false, false) => "deleteContentBackward",
        (false, true) => "deleteWordBackward",
        (true, false) => "deleteContentForward",
        (true, true) => "deleteWordForward",
    };
    let plain = read_dom(cx, |dom| dom.is_plaintext_only(host));
    let targets = if plain { Vec::new() } else { vec![range] };
    perform_planned(
        cx,
        host,
        Change {
            input_type,
            data: None,
            user,
            targets,
            transfer: None,
        },
        range,
        &plan,
        move |dom, range| {
            let caret = dom.delete_between(range);
            Some((caret, caret))
        },
    )
}

/// Wraps or unwraps the selected text in an inline element.
fn format(
    cx: &mut Context,
    host: NodeId,
    input_type: &str,
    tags: &'static [&'static str],
    attribute: Option<(String, String)>,
    user: bool,
) -> JsResult<bool> {
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    if range.start == range.end {
        return Ok(true);
    }
    let data = attribute.as_ref().map(|(_, v)| v.clone());
    perform(
        cx,
        host,
        Change {
            input_type,
            data: data.as_deref(),
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            let attr = attribute.as_ref().map(|(n, v)| (n.as_str(), v.as_str()));
            let done = dom.toggle_format(range, host, tags, attr)?;
            Some((done.start, done.end))
        },
    )
}

fn select_all(cx: &mut Context, host: NodeId) -> JsResult<bool> {
    // A handler that cancels `selectstart` leaves the selection as it is.
    if !fire_at(
        cx,
        host,
        "Event",
        "selectstart",
        &[("composed", false.into())],
        true,
    )? {
        return Ok(true);
    }
    ensure_selection(cx, host);
    with_dom(cx, |dom| {
        let end = (host, dom.node_length(host));
        dom.select_between((host, 0), end);
    });
    Ok(true)
}

// ─── execCommand ────────────────────────────────────────────────────────────────────────────

const BOLD: &[&str] = &["b", "strong"];
const ITALIC: &[&str] = &["i", "em"];
const UNDERLINE: &[&str] = &["u"];
const STRIKE: &[&str] = &["strike", "s"];
const SUB: &[&str] = &["sub"];
const SUP: &[&str] = &["sup"];
const LINK: &[&str] = &["a"];

/// The commands this document knows, for `queryCommandSupported`.
const SUPPORTED: &[&str] = &[
    "bold",
    "italic",
    "underline",
    "strikethrough",
    "subscript",
    "superscript",
    "createlink",
    "unlink",
    "formatblock",
    "insertorderedlist",
    "insertunorderedlist",
    "inserttext",
    "inserthtml",
    "insertparagraph",
    "insertlinebreak",
    "delete",
    "forwarddelete",
    "selectall",
    "undo",
    "redo",
    "copy",
    "cut",
    "removeformat",
    "stylewithcss",
    "defaultparagraphseparator",
];

fn inline_tags(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match name {
        "bold" => ("formatBold", BOLD),
        "italic" => ("formatItalic", ITALIC),
        "underline" => ("formatUnderline", UNDERLINE),
        "strikethrough" => ("formatStrikeThrough", STRIKE),
        "subscript" => ("formatSubscript", SUB),
        "superscript" => ("formatSuperscript", SUP),
        _ => return None,
    })
}

/// `document.execCommand(name, false, value)`; `user` when a key press asked for it (so
/// `beforeinput` is fired). Answers what `execCommand` returns.
pub(crate) fn exec_command(
    cx: &mut Context,
    name: &str,
    value: &str,
    user: bool,
) -> JsResult<bool> {
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "stylewithcss" => return Ok(true),
        "defaultparagraphseparator" => {
            let value = value.to_ascii_lowercase();
            if value == "div" || value == "p" {
                crate::state::shared(cx).borrow_mut().editing.separator = Some(value);
            }
            return Ok(true);
        }
        // A script needs transient activation to copy or cut; a menu or a key does not.
        "copy" | "cut" if !user && !activated(cx) => return Ok(false),
        "copy" => return crate::clipboard::copy_or_cut(cx, false, user),
        "cut" => return crate::clipboard::copy_or_cut(cx, true, user),
        _ => {}
    }
    let Some(host) = selected_host(cx).or_else(|| {
        // `selectAll` with no selection works on the focused host.
        read_dom(cx, |dom| dom.focused.and_then(|f| dom.editing_host(f)))
    }) else {
        return Ok(false);
    };
    if let Some((input_type, tags)) = inline_tags(&name) {
        return format(cx, host, input_type, tags, None, user);
    }
    match name.as_str() {
        "inserttext" => insert_text(cx, host, value, user),
        "insertparagraph" => insert_paragraph(cx, host, user),
        "insertlinebreak" => insert_line_break(cx, host, user),
        "delete" => delete(cx, host, false, Granularity::Character, user),
        "forwarddelete" => delete(cx, host, true, Granularity::Character, user),
        "selectall" => select_all(cx, host),
        "undo" => travel(cx, host, false, user),
        "redo" => travel(cx, host, true, user),
        "createlink" => format(
            cx,
            host,
            "insertLink",
            LINK,
            Some(("href".to_owned(), value.to_owned())),
            user,
        ),
        "unlink" => unwrap_tags(cx, host, "", LINK, user),
        "removeformat" => {
            const ALL: &[&str] = &["b", "strong", "i", "em", "u", "strike", "s", "sub", "sup"];
            unwrap_tags(cx, host, "formatRemove", ALL, user)
        }
        "inserthtml" => insert_html(cx, host, value, user),
        "formatblock" => format_block(cx, host, value, user),
        "insertorderedlist" => make_list(cx, host, "ol", "insertOrderedList", user),
        "insertunorderedlist" => make_list(cx, host, "ul", "insertUnorderedList", user),
        _ => Ok(false),
    }
}

/// What a cut does to the selection of `host` once the clipboard has it: `deleteByCut`.
pub(crate) fn cut_selection(cx: &mut Context, host: NodeId, user: bool) -> JsResult<bool> {
    let Some(range) = replaced(cx).filter(|r| r.start != r.end) else {
        return Ok(false);
    };
    perform(
        cx,
        host,
        Change {
            input_type: "deleteByCut",
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            let caret = dom.delete_between(range);
            Some((caret, caret))
        },
    )
}

fn unwrap_tags(
    cx: &mut Context,
    host: NodeId,
    input_type: &str,
    tags: &'static [&'static str],
    user: bool,
) -> JsResult<bool> {
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    perform(
        cx,
        host,
        Change {
            input_type,
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            let texts = dom.split_covered_text(range);
            for text in &texts {
                if let Some(element) = dom.inside_tag(*text, tags, host) {
                    dom.unwrap_element(element);
                }
            }
            let (first, last) = (*texts.first()?, *texts.last()?);
            Some(((first, 0), (last, dom.node_length(last))))
        },
    )
}

/// Replaces `range` with the nodes `markup` makes, answering where the selection goes; `clean`
/// takes out what could run script first (pasted markup is not the page's own).
fn insert_markup(
    dom: &mut Dom,
    range: RangeData,
    markup: &str,
    clean: bool,
) -> Option<(Boundary, Boundary)> {
    let at = dom.delete_between(range);
    let fragment = dom.create_fragment();
    crate::html::parse_into(dom, fragment, markup);
    if clean {
        strip_active(dom, fragment);
    }
    let kids = dom.children(fragment).to_vec();
    let last = *kids.last()?;
    let (parent, before) = dom.insertion_point(at);
    dom.insert_before(parent, fragment, before);
    let end = dom.after_node(last)?;
    Some((end, end))
}

/// Takes the elements that run or load code out of `root`, and the event attributes and
/// `javascript:` addresses off the rest.
fn strip_active(dom: &mut Dom, root: NodeId) {
    const ACTIVE: &[&str] = &[
        "script",
        "style",
        "iframe",
        "frame",
        "object",
        "embed",
        "applet",
        "meta",
        "link",
        "base",
        "title",
        "noscript",
        "template",
        "animate",
        "set",
        "animatetransform",
        "animatemotion",
    ];
    for id in dom.descendants(root) {
        if !dom.exists(id) {
            continue;
        }
        if dom
            .get(id)
            .and_then(|n| n.tag())
            .is_some_and(|tag| ACTIVE.contains(&tag))
        {
            dom.detach(id);
            continue;
        }
        for (name, value) in dom.attributes(id) {
            let lower = name.to_ascii_lowercase();
            // Browsers drop tabs, newlines and leading control characters before they read the
            // scheme, so `java&#9;script:` is `javascript:`.
            let scheme: String = value
                .chars()
                .filter(|c| !c.is_control() && !c.is_whitespace())
                .collect::<String>()
                .to_ascii_lowercase();
            let links = ["href", "xlink:href"].contains(&lower.as_str());
            let loads = links || ["src", "action", "formaction"].contains(&lower.as_str());
            let script_url = loads
                && (scheme.starts_with("javascript:") || (links && scheme.starts_with("data:")));
            if lower.starts_with("on") || script_url {
                dom.remove_attr(id, &name);
            }
        }
    }
}

fn insert_html(cx: &mut Context, host: NodeId, markup: &str, user: bool) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    let markup = markup.to_owned();
    perform(
        cx,
        host,
        Change {
            input_type: "",
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| insert_markup(dom, range, &markup, false),
    )
}

/// The text a paste puts in: the plain text the transfer holds, else the text of its markup.
pub(crate) fn pasted_text(cx: &mut Context, plain: Option<String>, html: Option<&str>) -> String {
    if let Some(plain) = plain {
        return plain;
    }
    let Some(html) = html else {
        return String::new();
    };
    with_dom(cx, |dom| {
        let fragment = dom.create_fragment();
        crate::html::parse_into(dom, fragment, html);
        strip_active(dom, fragment);
        let text = dom.text_content(fragment);
        dom.detach(fragment);
        text
    })
}

/// A paste into `host`: `insertFromPaste` with what the transfer holds. Markup goes in cleaned
/// of script; `match_style`, and a `plaintext-only` host, take the text alone.
pub(crate) fn paste(
    cx: &mut Context,
    host: NodeId,
    transfer: &boa_engine::JsObject,
    match_style: bool,
) -> JsResult<bool> {
    ensure_selection(cx, host);
    let Some(range) = replaced(cx) else {
        return Ok(false);
    };
    let entries = crate::bind::data_transfer::entries_of(transfer);
    let find = |wanted: &str| {
        entries.iter().find_map(|entry| match entry {
            crate::bind::data_transfer::Entry::Text { kind, data } if kind == wanted => {
                Some(data.clone())
            }
            _ => None,
        })
    };
    let (html, text) = (find("text/html"), find("text/plain"));
    let plain_host = read_dom(cx, |dom| dom.is_plaintext_only(host));
    let markup = html.clone().filter(|_| !match_style && !plain_host);
    let plain = pasted_text(cx, text, html.as_deref());
    let data = plain_host.then_some(plain.as_str());
    let targets = if plain_host { Vec::new() } else { vec![range] };
    let owned = plain.clone();
    let separator = separator(cx);
    perform(
        cx,
        host,
        Change {
            input_type: "insertFromPaste",
            data,
            user: true,
            targets,
            // A `plaintext-only` host is told the text in `data` and no transfer (Electron 39.8.10).
            transfer: (!plain_host).then(|| transfer.clone()),
        },
        range,
        move |dom, range| match &markup {
            _ if plain_host => {
                let at = dom.delete_between(range);
                let caret = dom.insert_plain_at(at, &owned);
                Some((caret, caret))
            }
            Some(markup) => insert_markup(dom, range, markup, true),
            // Plain text is paragraphs, as Chromium makes them: a line break splits the block.
            None => {
                let mut at = dom.delete_between(range);
                for (line, text) in owned.replace("\r\n", "\n").split('\n').enumerate() {
                    if line > 0 {
                        at = dom.split_block_at(at, host, &separator);
                    }
                    if !text.is_empty() {
                        at = dom.insert_text_at(at, text);
                    }
                }
                Some((at, at))
            }
        },
    )
}

fn format_block(cx: &mut Context, host: NodeId, value: &str, user: bool) -> JsResult<bool> {
    let tag = value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase();
    let Some((range, direction)) = selection_of(cx) else {
        return Ok(false);
    };
    if tag.is_empty() {
        return Ok(false);
    }
    perform(
        cx,
        host,
        Change {
            input_type: "",
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            dom.change_block(range.start, host, &tag)?;
            let (a, b) = match direction {
                Direction::Forward => (range.start, range.end),
                Direction::Backward => (range.end, range.start),
            };
            Some((a, b))
        },
    )
}

fn make_list(
    cx: &mut Context,
    host: NodeId,
    tag: &'static str,
    input_type: &str,
    user: bool,
) -> JsResult<bool> {
    let Some((range, direction)) = selection_of(cx) else {
        return Ok(false);
    };
    perform(
        cx,
        host,
        Change {
            input_type,
            data: None,
            user,
            targets: vec![range],
            transfer: None,
        },
        range,
        move |dom, range| {
            dom.wrap_block_in_list(range.start, tag)?;
            let (a, b) = match direction {
                Direction::Forward => (range.start, range.end),
                Direction::Backward => (range.end, range.start),
            };
            Some((a, b))
        },
    )
}

// ─── queryCommand* ──────────────────────────────────────────────────────────────────────────

/// `document.queryCommandSupported(name)`.
pub(crate) fn supported(name: &str) -> bool {
    SUPPORTED.contains(&name.to_ascii_lowercase().as_str())
}

/// `document.queryCommandEnabled(name)`.
fn activated(cx: &Context) -> bool {
    crate::state::shared(cx).borrow().has_activation()
}

pub(crate) fn enabled(cx: &Context, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if !supported(&name) {
        return false;
    }
    let host = selected_host(cx);
    let state = crate::state::shared(cx);
    match name.as_str() {
        "selectall" | "stylewithcss" | "defaultparagraphseparator" => true,
        // Electron 39.8.10: `queryCommandEnabled('copy')` is false until a trusted press or key.
        "copy" => activated(cx),
        "cut" => {
            activated(cx)
                && host.is_some()
                && read_dom(cx, |d| d.selection().is_some_and(|(r, _)| r.start != r.end))
        }
        "undo" => host.is_some_and(|h| {
            state
                .borrow()
                .editing
                .histories
                .get(&h)
                .is_some_and(|x| !x.undo.is_empty())
        }),
        "redo" => host.is_some_and(|h| {
            state
                .borrow()
                .editing
                .histories
                .get(&h)
                .is_some_and(|x| !x.redo.is_empty())
        }),
        _ => host.is_some(),
    }
}

/// `document.queryCommandState(name)`: whether the selected text has the format.
pub(crate) fn command_state(cx: &Context, name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let Some((_, tags)) = inline_tags(&name) else {
        return false;
    };
    read_dom(cx, |dom| {
        let Some((range, _)) = dom.selection() else {
            return false;
        };
        let host = dom.editing_host(range.start.0).unwrap_or(dom.body);
        let start = if dom.is_character_data(range.start.0) {
            range.start.0
        } else {
            dom.children(range.start.0)
                .get(range.start.1)
                .copied()
                .unwrap_or(range.start.0)
        };
        dom.inside_tag(start, tags, host).is_some()
    })
}

/// `document.queryCommandValue(name)`.
pub(crate) fn command_value(cx: &Context, name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "defaultparagraphseparator" => separator(cx),
        "formatblock" => read_dom(cx, |dom| {
            dom.selection()
                .and_then(|(r, _)| {
                    dom.get(dom.block_of(r.start.0))
                        .and_then(|n| n.tag().map(str::to_owned))
                })
                .unwrap_or_default()
        }),
        "bold" | "italic" | "underline" | "strikethrough" => if command_state(cx, name) {
            "true"
        } else {
            "false"
        }
        .to_owned(),
        _ => String::new(),
    }
}

/// `InputEvent.prototype.getTargetRanges`.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let prototype = crate::state::class_prototype(context, "InputEvent");
    let methods: &[crate::bind::Method] = &[("getTargetRanges", 0, |this, _, cx| {
        let known = native::get(this, "__targetRanges", cx)?;
        let items = native::list_of(&known, cx)?;
        Ok(native::array(cx, items))
    })];
    crate::bind::extend_prototype(context, &prototype, methods, &[])
}

// ─── The keys ───────────────────────────────────────────────────────────────────────────────

/// The `keypress` of a key that types (a character, or Enter) and the change it makes. Answers
/// whether the key was handled.
fn typed_key(cx: &mut Context, host: NodeId, key: &Key, printable: bool) -> JsResult<bool> {
    let code = if printable {
        key.key.chars().next().map_or(0, |c| c as u32)
    } else {
        13
    };
    let init = [
        ("key", js_text(&key.key)),
        ("code", js_text(&key.code)),
        ("keyCode", code.into()),
        ("which", code.into()),
        ("charCode", code.into()),
        ("shiftKey", key.modifiers.shift.into()),
        ("ctrlKey", key.modifiers.ctrl.into()),
        ("altKey", key.modifiers.alt.into()),
        ("metaKey", key.modifiers.meta.into()),
    ];
    if !fire_at(cx, host, "KeyboardEvent", "keypress", &init, true)? {
        return Ok(true);
    }
    if printable {
        insert_text(cx, host, &key.key, true)?;
    } else if key.modifiers.shift {
        insert_line_break(cx, host, true)?;
    } else {
        insert_paragraph(cx, host, true)?;
    }
    Ok(true)
}

/// A caret key: the selection moves, or grows when Shift is held.
fn move_caret(cx: &mut Context, host: NodeId, key: &Key, word: Granularity) {
    ensure_selection(cx, host);
    let chord = key.modifiers.ctrl || key.modifiers.meta;
    let (unit, forward) = match key.key.as_str() {
        "ArrowLeft" => (word, false),
        "ArrowRight" => (word, true),
        "Home" if chord => (Granularity::Document, false),
        "End" if chord => (Granularity::Document, true),
        "Home" => (Granularity::LineBoundary, false),
        "End" => (Granularity::LineBoundary, true),
        "ArrowUp" => (Granularity::Paragraph, false),
        _ => (Granularity::Paragraph, true),
    };
    let extend = key.modifiers.shift;
    with_dom(cx, |dom| dom.modify_selection(extend, forward, unit));
}

/// What a key does in `host`; whether it was one editing handles.
fn key_action(cx: &mut Context, host: NodeId, key: &Key) -> JsResult<bool> {
    let chord = key.modifiers.ctrl || key.modifiers.meta;
    let word = if key.modifiers.ctrl || key.modifiers.alt {
        Granularity::Word
    } else {
        Granularity::Character
    };
    let printable = key.key.chars().count() == 1 && !chord && !key.modifiers.alt;
    if printable || key.key == "Enter" {
        return typed_key(cx, host, key, printable);
    }
    let lower = key.key.to_ascii_lowercase();
    match key.key.as_str() {
        "Backspace" => delete(cx, host, false, word, true).map(|_| true),
        "Delete" => delete(cx, host, true, word, true).map(|_| true),
        "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" | "Home" | "End" => {
            move_caret(cx, host, key, word);
            Ok(true)
        }
        _ if chord && lower == "a" => select_all(cx, host),
        _ if chord && lower == "z" => travel(cx, host, key.modifiers.shift, true),
        _ if chord && lower == "y" => travel(cx, host, true, true),
        _ if chord && lower == "b" => exec_command(cx, "bold", "", true),
        _ if chord && lower == "i" => exec_command(cx, "italic", "", true),
        _ if chord && lower == "u" => exec_command(cx, "underline", "", true),
        _ => Ok(false),
    }
}

impl Document {
    /// The editing host `target` is in, when keys there edit.
    pub(crate) fn editable_host(&self, target: NodeId) -> Option<NodeId> {
        self.state.borrow().dom.editing_host(target)
    }

    /// What a key does in an editable region, after `keydown`: answers whether it was one the
    /// editing handled (Tab is left to the caller).
    pub(crate) fn edit_key(&self, runtime: &mut verve::Runtime, host: NodeId, key: &Key) -> bool {
        match key_action(runtime.context(), host, key) {
            Ok(handled) => handled,
            Err(error) => {
                verve::report(runtime.context(), &error);
                true
            }
        }
    }

    /// Text put into the editable region at once (an input method's commit, a paste).
    pub(crate) fn insert_into_host(&self, runtime: &mut verve::Runtime, host: NodeId, text: &str) {
        let cx = runtime.context();
        if let Err(error) = insert_text(cx, host, text, true) {
            verve::report(cx, &error);
        }
    }

    /// `document.execCommand` as a person's menu or shortcut asks for it: `beforeinput` is
    /// fired first. Answers what `execCommand` returns.
    pub fn exec_command(&self, runtime: &mut verve::Runtime, name: &str, value: &str) -> bool {
        let cx = runtime.context();
        let done = match exec_command(cx, name, value, true) {
            Ok(done) => done,
            Err(error) => {
                verve::report(cx, &error);
                false
            }
        };
        runtime.run_microtasks();
        done
    }
}
