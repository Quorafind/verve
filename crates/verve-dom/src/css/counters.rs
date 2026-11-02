//! CSS counters (CSS Lists 3 §4): `counter-reset`, `counter-increment` and `counter-set` as a
//! walk of the document in order, and what `counter()` and `counters()` read from them.
//!
//! A counter an element resets is in scope for that element, its descendants, and its
//! following siblings with theirs. A following sibling that resets the same name takes the
//! counter over rather than nesting inside it; a descendant that does starts a nested one,
//! which `counters()` shows after the outer.

use std::collections::HashMap;
use std::sync::Arc;

use super::cascade::Style;
use super::pseudo::PseudoElement;
use crate::dom::{Dom, NodeId};

/// The counters in scope at a place in the walk.
#[derive(Default)]
pub(super) struct Counters {
    /// For each name, its counters from the outermost, each with the level of the element
    /// siblings that created it.
    stacks: HashMap<String, Vec<(usize, i32)>>,
}

/// What `counter-reset` and its family do to a counter.
#[derive(Clone, Copy)]
enum Change {
    Reset,
    Increment,
    Set,
}

impl Change {
    /// Does this to one counter's stack for an element whose siblings are at `level`.
    fn to(self, stack: &mut Vec<(usize, i32)>, level: usize, amount: i32) {
        if matches!(self, Change::Reset) {
            return reset(stack, level, amount);
        }
        // A counter the element can not see is made on the spot, at zero.
        if stack.is_empty() {
            reset(stack, level, 0);
        }
        let top = &mut stack.last_mut().expect("a counter was just made").1;
        match self {
            Change::Increment => *top += amount,
            _ => *top = amount,
        }
    }
}

impl Counters {
    /// Applies the `counter-reset`, `counter-increment` and `counter-set` of an element whose
    /// siblings are at `level`, in that order.
    pub(super) fn apply(&mut self, style: &Style, level: usize) {
        for (property, change) in [
            ("counter-reset", Change::Reset),
            ("counter-increment", Change::Increment),
            ("counter-set", Change::Set),
        ] {
            let Some(value) = style.get(property) else {
                continue;
            };
            for (name, amount) in declarations(value, change) {
                change.to(self.stacks.entry(name).or_default(), level, amount);
            }
        }
    }

    /// Takes away the counters made by the siblings at `level`: their parent is done.
    fn close(&mut self, level: usize) {
        self.stacks.retain(|_, stack| {
            while stack.last().is_some_and(|(at, _)| *at >= level) {
                stack.pop();
            }
            !stack.is_empty()
        });
    }

    /// The text `counter(name, style)` reads, or `counters(name, separator, style)` when there
    /// is a separator.
    pub(super) fn text(&self, name: &str, separator: Option<&str>, style: &str) -> String {
        let values = self.stacks.get(name).map_or(&[][..], Vec::as_slice);
        match separator {
            Some(separator) => values
                .iter()
                .map(|(_, value)| counter_text(*value, style))
                .collect::<Vec<_>>()
                .join(separator),
            None => counter_text(values.last().map_or(0, |(_, value)| *value), style),
        }
    }
}

/// A reset at `level`: the counter a preceding sibling made is taken over, any other is
/// nested inside.
fn reset(stack: &mut Vec<(usize, i32)>, level: usize, amount: i32) {
    match stack.last_mut() {
        Some(top) if top.0 == level => top.1 = amount,
        _ => stack.push((level, amount)),
    }
}

/// `item 2 page` as the pairs `counter-reset` takes: a number right after a name is that
/// counter's amount, which is one unless it is a reset's, which is zero.
fn declarations(value: &str, change: Change) -> Vec<(String, i32)> {
    let default = match change {
        Change::Reset => 0,
        Change::Increment | Change::Set => 1,
    };
    let mut out = Vec::new();
    let mut words = value.split_whitespace().peekable();
    while let Some(name) = words.next() {
        if name.eq_ignore_ascii_case("none") {
            continue;
        }
        let amount = match words.peek().and_then(|next| next.parse::<i32>().ok()) {
            Some(amount) => {
                words.next();
                amount
            }
            None => default,
        };
        out.push((name.to_owned(), amount));
    }
    out
}

/// Walks the elements of `dom` in document order, the way counters are counted, and calls
/// `at` for each box of an element that may count or read a counter: its `::marker` and
/// `::before` once the element's own counter properties are applied, its `::after` once its
/// children are done. A box is the element's first or last child, so `at` is given the level
/// its siblings are at, and applies the box's own counter properties there before reading.
/// An element that is not rendered counts nothing, nor do its children.
pub(super) fn walk(
    dom: &Dom,
    styles: &HashMap<NodeId, Arc<Style>>,
    mut at: impl FnMut(NodeId, PseudoElement, usize, &mut Counters),
) {
    enum Step {
        Enter(NodeId, usize),
        After(NodeId, usize),
        Leave(usize),
    }
    let mut counters = Counters::default();
    let mut stack = vec![Step::Enter(dom.document, 0)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(id, level) => {
                let style = styles
                    .get(&id)
                    .filter(|style| style.get("display").is_none_or(|display| display != "none"));
                if let Some(style) = style {
                    counters.apply(style, level);
                    at(id, PseudoElement::Marker, level + 1, &mut counters);
                    at(id, PseudoElement::Before, level + 1, &mut counters);
                } else if dom.get(id).is_some_and(|node| node.is_element()) {
                    continue;
                }
                stack.push(Step::Leave(level + 1));
                stack.push(Step::After(id, level + 1));
                stack.extend(
                    dom.flat_children(id)
                        .into_iter()
                        .rev()
                        .map(|child| Step::Enter(child, level + 1)),
                );
            }
            Step::After(id, level) => at(id, PseudoElement::After, level, &mut counters),
            Step::Leave(level) => counters.close(level),
        }
    }
}

/// A counter's value as `style` writes it.
fn counter_text(value: i32, style: &str) -> String {
    match style.trim().to_ascii_lowercase().as_str() {
        "lower-alpha" | "lower-latin" => alpha(value, false),
        "upper-alpha" | "upper-latin" => alpha(value, true),
        "lower-roman" => roman(value).to_lowercase(),
        "upper-roman" => roman(value),
        "decimal-leading-zero" => format!("{value:02}"),
        "none" => String::new(),
        _ => value.to_string(),
    }
}

/// `a`, `b`… `z`, `aa`… as the alphabetic list styles count; a number that is not positive
/// is written as it is.
fn alpha(value: i32, upper: bool) -> String {
    if value < 1 {
        return value.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    let mut rest = value;
    let mut out = Vec::new();
    while rest > 0 {
        rest -= 1;
        out.push(base + (rest % 26) as u8);
        rest /= 26;
    }
    out.reverse();
    String::from_utf8(out).expect("ASCII letters")
}

fn roman(value: i32) -> String {
    const STEPS: &[(i32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if !(1..=3999).contains(&value) {
        return value.to_string();
    }
    let mut left = value;
    let mut out = String::new();
    for (step, text) in STEPS {
        while left >= *step {
            out.push_str(text);
            left -= step;
        }
    }
    out
}
