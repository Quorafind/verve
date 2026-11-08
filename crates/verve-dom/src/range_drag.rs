//! `<input type=range>`: the value it holds, dragging its thumb and its keys.
//!
//! The value is sanitized as the HTML standard has it, which Electron 39.8.10 agrees with
//! (`p32c/rng`, `p32c-review/fld2-out.json`): a number, clamped to `min` and `max` (0 and 100 by
//! default), rounded to the nearest step from `min` (`step`, 1 by default, `any` for none), and the
//! middle of the range when there is none. What a person sets is kept apart from the `value`
//! attribute, which stays the default (a reset brings it back), as in Chromium. Dragging the
//! thumb sets the value from the pointer: `input` for every change while the button is down and
//! `change` when it comes up with the value different from the one at the press. The thumb is
//! 12px wide and its middle runs from half of that in to half of that from the other end, as the
//! shell draws it. Arrows move a step (a hundredth of the range for `step=any`), Page Up and
//! Page Down a tenth of the range (at least a step), Home and End go to the ends.

use crate::dom::{Dom, NodeId};
use crate::{Document, Pointer};

/// The width of the thumb, which the track's ends leave room for.
pub const THUMB: f32 = 12.;

/// `number` as JavaScript writes it (`0.5`, `1e21`), which is how a range's value is read back.
pub(crate) fn js_number(number: f64) -> String {
    boa_engine::JsString::from(number).to_std_string_escaped()
}

/// How many digits `number` has after the point, `None` when JavaScript writes it with an exponent.
fn decimals(number: f64) -> Option<i32> {
    let text = js_number(number);
    if text.contains('e') {
        return None;
    }
    Some(
        text.split_once('.')
            .map_or(0, |(_, tail)| tail.len() as i32),
    )
}

/// `value` rounded to `places` digits after the point: steps like 0.1 add up to
/// 0.30000000000000004, and what a person would write is kept. Near the end of what a double
/// holds exactly the value is left alone.
fn tidy(value: f64, places: i32) -> f64 {
    let scale = 10_f64.powi(places);
    if value.abs() * scale > 1e15 {
        return value;
    }
    (value * scale).round() / scale
}

impl Dom {
    /// The number an attribute of the range input `id` holds.
    fn range_number(&self, id: NodeId, name: &str) -> Option<f64> {
        let text = self.get_attr(id, name)?;
        text.trim().parse::<f64>().ok().filter(|n| n.is_finite())
    }

    /// `(min, max, step)` of the range input: `step` is `None` for `any`.
    pub fn range_limits(&self, id: NodeId) -> (f64, f64, Option<f64>) {
        let min = self.range_number(id, "min").unwrap_or(0.);
        let max = self.range_number(id, "max").unwrap_or(100.).max(min);
        let step = match self.get_attr(id, "step") {
            Some(text) if text.trim().eq_ignore_ascii_case("any") => None,
            _ => Some(
                self.range_number(id, "step")
                    .filter(|s| *s > 0.)
                    .unwrap_or(1.),
            ),
        };
        (min, max, step)
    }

    /// `wanted` as the range input `id` would hold it: clamped, and on a step.
    pub fn range_sanitized(&self, id: NodeId, wanted: f64) -> f64 {
        let (min, max, step) = self.range_limits(id);
        let clamped = wanted.clamp(min, max);
        let Some(step) = step else {
            return clamped;
        };
        let snapped = min + ((clamped - min) / step).round() * step;
        // The last step that fits when rounding went past the end.
        let value = if snapped > max {
            min + ((max - min) / step).floor() * step
        } else {
            snapped
        };
        let places = decimals(step).zip(decimals(min)).map(|(s, m)| s.max(m));
        places.map_or(value, |places| tidy(value, places))
    }

    /// The value of the range input `id`: what a person or a script set, or the `value` attribute,
    /// sanitized, or the middle of the range.
    pub fn range_value(&self, id: NodeId) -> String {
        let (min, max, _) = self.range_limits(id);
        let node = self.get(id);
        let typed = node
            .filter(|n| n.dirty_value)
            .and_then(|n| n.draft.as_ref())
            .map(|draft| draft.text().to_owned());
        let wanted = typed
            .or_else(|| node.and_then(|n| n.attr("value")).map(str::to_owned))
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .unwrap_or(min + (max - min) / 2.);
        js_number(self.range_sanitized(id, wanted))
    }

    /// Sets the value of the range input `id` to `wanted`, sanitized. Answers whether the value
    /// the page reads changed.
    fn set_range(&mut self, id: NodeId, wanted: f64) -> bool {
        let before = self.range_value(id);
        let text = js_number(self.range_sanitized(id, wanted));
        self.set_value(id, &text);
        self.range_value(id) != before
    }
}

/// The range input a press on `target` is on, if it is one that can be changed.
fn range_under(dom: &Dom, target: NodeId) -> Option<NodeId> {
    let found = dom.get(target)?;
    let is = found.tag() == Some("input")
        && crate::bind::element::input_type(dom, target) == "range"
        && found.attr("disabled").is_none();
    is.then_some(target)
}

impl Document {
    /// The primary button went down on `target`: if it is a range input, the thumb goes to the
    /// pointer, and moving the pointer drags it. Answers whether it was one.
    pub(crate) fn range_pressed(
        &self,
        runtime: &mut verve::Runtime,
        target: NodeId,
        pointer: &Pointer,
    ) -> bool {
        let Some(range) = range_under(&self.state.borrow().dom, target) else {
            return false;
        };
        let start = self.state.borrow().dom.value(range);
        self.view.borrow_mut().drag.range = Some((range, start));
        self.range_to(runtime, range, pointer.x);
        true
    }

    /// The pointer moved with a range thumb held. Answers whether one was.
    pub(crate) fn range_moved(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> bool {
        let held = self.view.borrow().drag.range.clone();
        let Some((range, _)) = held else {
            return false;
        };
        if pointer.buttons & 1 == 0 {
            return false;
        }
        self.range_to(runtime, range, pointer.x);
        true
    }

    /// The button came up: the value is final. `change` follows the `mouseup`
    /// ([`Document::range_changed`]) if it is not the one the press found.
    pub(crate) fn range_released(&self) {
        let held = self.view.borrow_mut().drag.range.take();
        let Some((range, start)) = held else {
            return;
        };
        if self.state.borrow().dom.value(range) != start {
            self.view.borrow_mut().drag.changed = Some(range);
        }
    }

    /// Fires the `change` a released thumb owes. Electron 39.8.10 (`p32c/rng`): `input` at the
    /// press, then `mouseup`, then `change`, then `click`.
    pub(crate) fn range_changed(&self, runtime: &mut verve::Runtime) {
        let owed = self.view.borrow_mut().drag.changed.take();
        if let Some(range) = owed {
            self.fire(runtime, range, "Event", "change", &[]);
        }
    }

    /// Whether `node` is a range input that can be changed.
    pub(crate) fn is_range(&self, node: NodeId) -> bool {
        range_under(&self.state.borrow().dom, node).is_some()
    }

    /// A key on the focused range input. Electron 39.8.10 (`p32c/rng`, and `p32c-review/fld2-out.json`
    /// for `step=any`): Right and Up add a step, Left and Down take one away, Page Up and Page Down
    /// move a tenth of the range (at least a step), Home and End go to the ends; each change is
    /// `input` and then `change`. With `step=any` a step is a hundredth of the range (0.51 after
    /// 0.5 of 0 to 1; 10.5 and 11 from 10 of 0 to 50). Answers whether the key is one of these.
    pub(crate) fn range_key(&self, runtime: &mut verve::Runtime, range: NodeId, key: &str) -> bool {
        let wanted = {
            let state = self.state.borrow();
            let dom = &state.dom;
            let (min, max, step) = dom.range_limits(range);
            let span = max - min;
            let each = step.unwrap_or(span / 100.);
            let big = (span / 10.).max(each);
            let now = dom.range_value(range).parse::<f64>().unwrap_or(min);
            match key {
                "ArrowRight" | "ArrowUp" => now + each,
                "ArrowLeft" | "ArrowDown" => now - each,
                "PageUp" => now + big,
                "PageDown" => now - big,
                "Home" => min,
                "End" => max,
                _ => return false,
            }
        };
        let changed =
            crate::bind::node::with_dom(runtime.context(), |dom| dom.set_range(range, wanted));
        if changed {
            self.fire(runtime, range, "Event", "input", &[]);
            self.fire(runtime, range, "Event", "change", &[]);
        }
        runtime.run_microtasks();
        true
    }

    /// Puts the thumb at the page's `x`, firing `input` if that changes the value.
    fn range_to(&self, runtime: &mut verve::Runtime, range: NodeId, x: f32) {
        let Some(rect) = self.rect(range) else {
            return;
        };
        let wanted = {
            let state = self.state.borrow();
            let (min, max, _) = state.dom.range_limits(range);
            let thumb = THUMB.min(rect.height);
            let run = (rect.width - thumb).max(1.);
            let share = f64::from(((x - rect.x - thumb / 2.) / run).clamp(0., 1.));
            min + share * (max - min)
        };
        let changed =
            crate::bind::node::with_dom(runtime.context(), |dom| dom.set_range(range, wanted));
        if changed {
            self.fire(runtime, range, "Event", "input", &[]);
        }
        runtime.run_microtasks();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_as_javascript_does_and_steps_leave_no_noise() {
        assert_eq!(js_number(0.5), "0.5");
        assert_eq!(js_number(1.), "1");
        assert_eq!(js_number(1e21), "1e+21");
        assert_eq!(tidy(0.1 + 0.2, 1), 0.3);
        // Near what a double holds exactly nothing is rounded, and nothing overflows.
        assert_eq!(tidy(1e300, 9), 1e300);
        assert_eq!(decimals(0.25), Some(2));
        assert_eq!(decimals(3.), Some(0));
        assert_eq!(decimals(1e-7), None);
    }
}
