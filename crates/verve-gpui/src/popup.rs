//! The pop-up of a drop-down `<select>`: drawn in the window over the page, from the options the
//! renderer published, and chosen from with the pointer and the keys. What a person chooses goes
//! back as [`ToRendererInput::Choose`](verve_electron::msg::ToRendererInput).
//!
//! The model ([`Popup`]) is plain data and functions, so that a test can press keys at it with no
//! window. The document decides when a pop-up opens (a press `mousedown` did not prevent, F4,
//! Alt+Up, Alt+Down, Space and Enter: [`verve_dom::Document::take_open_select`]) and the page
//! hears the opener's `keydown`, as in Electron 39.8.10 (`p32c-review/sel3-out.json`, hidden
//! windows, a fresh one for each case); this module opens it when its view says so, and keeps it
//! true to the page: it closes when its select is gone, disabled or not focused, when the page
//! navigates, when the window loses the focus or is resized, and follows the select's options and
//! place when they change. In a hidden window the pop-up is not shown, so what was observed is
//! what the page heard, and the following is not confirmed in a visible one: moving with the
//! keys fires nothing, Escape after moving, Enter and Alt+Down choose the highlighted option
//! (`input` and `change`; Chromium adds a `click` after Enter, which is not sent here). How the
//! pop-up looks (row height, colours) was not observed.

use std::sync::Arc;
use std::time::Instant;

use gpui::{FontWeight, IntoElement, ParentElement as _, Styled as _, deferred, div, px, rgb};
use verve_dom::{NodeId, Rect, TypeAhead};
use verve_electron::msg::ToRendererInput;
use verve_layout::{ControlKind, DisplayList, PaintItem, SelectOption};

use crate::dom_view::DomState;

/// A row of the pop-up, in CSS pixels.
pub(crate) const ROW: f32 = 20.;
/// The most rows shown at once.
const MOST_ROWS: usize = 20;
/// The border of the pop-up on each side.
pub(crate) const EDGE: f32 = 1.;

/// A line of the pop-up: the heading of an `<optgroup>` or one option (its index in the options).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    Group(String),
    Option(usize),
}

/// What a key or a press did to the pop-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// It stays open.
    Keep,
    /// It closes and nothing is chosen.
    Close,
    /// It closes and the option with this index is chosen.
    Choose(usize),
}

/// The pop-up of one select.
#[derive(Clone, Debug)]
pub(crate) struct Popup {
    pub select: NodeId,
    pub options: Arc<[SelectOption]>,
    /// The select's box in the page's pixels, which the pop-up hangs from.
    anchor: Rect,
    /// The pop-up's box, border included, in the page's pixels.
    pub rect: Rect,
    pub rows: Vec<Row>,
    /// The first row shown and how many.
    pub first: usize,
    pub shown: usize,
    /// The option under the pointer or the cursor.
    pub active: Option<usize>,
    /// Whether the keys moved the active option: Escape then chooses it.
    picked: bool,
    typing: TypeAhead,
}

/// The select `node` of the list, if it is a drop-down a person can open: its box in the window
/// and its options.
pub(crate) fn find(
    list: &DisplayList,
    node: NodeId,
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Option<(Rect, Arc<[SelectOption]>)> {
    list.placed(offset_of).into_iter().find_map(|placed| {
        let PaintItem::Control {
            node: found,
            bounds,
            kind:
                ControlKind::Select {
                    options,
                    list: None,
                    ..
                },
            look,
        } = placed.item
        else {
            return None;
        };
        (*found == node && !look.disabled).then(|| (placed.in_window(*bounds), Arc::clone(options)))
    })
}

impl Popup {
    /// The pop-up of `options` hanging from `anchor`, kept inside a page of `viewport` size: under
    /// the select, or over it when there is more room there, shorter and scrolling when neither
    /// has enough.
    pub(crate) fn open(
        select: NodeId,
        options: Arc<[SelectOption]>,
        anchor: Rect,
        viewport: (f32, f32),
    ) -> Self {
        let mut rows = Vec::new();
        let mut group: Option<&str> = None;
        for (index, option) in options.iter().enumerate() {
            if option.group.as_deref() != group {
                group = option.group.as_deref();
                rows.extend(group.map(|label| Row::Group(label.to_owned())));
            }
            rows.push(Row::Option(index));
        }
        let wanted = rows.len().min(MOST_ROWS);
        let fits = |room: f32| (((room - 2. * EDGE) / ROW).floor().max(1.) as usize).min(wanted);
        let below = viewport.1 - (anchor.y + anchor.height);
        let (shown, above) = if fits(below) == wanted || below >= anchor.y {
            (fits(below), false)
        } else {
            (fits(anchor.y), true)
        };
        let height = shown as f32 * ROW + 2. * EDGE;
        let width = (widest(&options) + 28.).max(anchor.width);
        let x = anchor.x.min(viewport.0 - width).max(0.);
        let y = if above {
            anchor.y - height
        } else {
            anchor.y + anchor.height
        };
        let active = options.iter().position(|option| option.selected);
        let mut popup = Self {
            select,
            options,
            anchor,
            rect: Rect::new(x, y.max(0.), width, height),
            rows,
            first: 0,
            shown,
            active,
            picked: false,
            typing: TypeAhead::default(),
        };
        popup.show_active();
        popup
    }

    /// Whether the page point `(x, y)` is on the pop-up.
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    /// The option row under the page point, if there is one.
    fn option_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.contains(x, y) {
            return None;
        }
        let row = self.first + ((y - self.rect.y - EDGE) / ROW).floor().max(0.) as usize;
        match self.rows.get(row)? {
            Row::Option(index) => Some(*index),
            Row::Group(_) => None,
        }
    }

    /// The pointer moved to the page point: the option under it, if a person can choose it,
    /// becomes the active one.
    pub(crate) fn hover(&mut self, x: f32, y: f32) {
        if let Some(index) = self.option_at(x, y)
            && !self.options[index].disabled
        {
            self.active = Some(index);
            self.picked = false;
        }
    }

    /// A press at the page point: on an option that can be chosen it chooses it, on the pop-up's
    /// border or a heading it does nothing, and anywhere else it closes the pop-up.
    pub(crate) fn press(&self, x: f32, y: f32) -> Outcome {
        if !self.contains(x, y) {
            return Outcome::Close;
        }
        match self.option_at(x, y) {
            Some(index) if !self.options[index].disabled => Outcome::Choose(index),
            _ => Outcome::Keep,
        }
    }

    /// The wheel turned by `lines` rows (positive scrolls down).
    pub(crate) fn scroll(&mut self, lines: i32) {
        let last = self.rows.len().saturating_sub(self.shown);
        self.first = (self.first as i32 + lines).clamp(0, last as i32) as usize;
    }

    /// Scrolls the least that shows the active option.
    fn show_active(&mut self) {
        let row = self
            .active
            .and_then(|a| self.rows.iter().position(|r| *r == Row::Option(a)));
        let Some(row) = row else {
            return;
        };
        if row < self.first {
            self.first = row;
        } else if row >= self.first + self.shown {
            self.first = row + 1 - self.shown;
        }
    }

    /// The enabled option `steps` away from the active one, stopping at the ends.
    fn step(&self, steps: i32) -> Option<usize> {
        let enabled: Vec<usize> = (0..self.options.len())
            .filter(|index| !self.options[*index].disabled)
            .collect();
        let last = enabled.len().checked_sub(1)? as i32;
        let at = self
            .active
            .and_then(|active| enabled.iter().position(|e| *e == active));
        let to = match at {
            Some(at) => (at as i32).saturating_add(steps).clamp(0, last),
            None if steps < 0 => last,
            None => 0,
        };
        enabled.get(to as usize).copied()
    }

    /// A key went down in the pop-up, named as the page names it (`key`), with Alt held or not.
    pub(crate) fn key(&mut self, key: &str, alt: bool, now: Instant) -> Outcome {
        let shown = self.shown as i32;
        let to = match key {
            "ArrowDown" if !alt => self.step(1),
            "ArrowUp" if !alt => self.step(-1),
            "PageDown" => self.step(shown - 1),
            "PageUp" => self.step(1 - shown),
            "Home" => self.step(i32::MIN),
            "End" => self.step(i32::MAX),
            "Escape" if !self.picked => return Outcome::Close,
            "Escape" | "F4" | "ArrowDown" | "ArrowUp" | "Enter" | "Tab" => {
                return self.active.map_or(Outcome::Close, Outcome::Choose);
            }
            " " if !self.typing.is_typing(now) => {
                return self.active.map_or(Outcome::Keep, Outcome::Choose);
            }
            text if text.chars().count() == 1 => self.type_ahead(text, now),
            _ => return Outcome::Keep,
        };
        if to.is_some() {
            self.active = to;
            self.picked = true;
            self.show_active();
        }
        Outcome::Keep
    }

    /// The enabled option a typed character leads to: the document's rule for a closed select
    /// ([`TypeAhead`]).
    fn type_ahead(&mut self, text: &str, now: Instant) -> Option<usize> {
        let options: Vec<(&str, bool)> = (self.options.iter())
            .map(|option| (option.label.as_str(), !option.disabled))
            .collect();
        self.typing.find(text, now, self.active, &options)
    }

    /// This pop-up after the page changed: the same one when the options and the select's place
    /// are, else one made for the new options and place that keeps the active option if it is
    /// still there.
    fn followed(self, options: Arc<[SelectOption]>, anchor: Rect, viewport: (f32, f32)) -> Self {
        if *self.options == *options && self.anchor == anchor {
            return self;
        }
        let mut now = Self::open(self.select, options, anchor, viewport);
        let keep = self
            .active
            .filter(|at| now.options.get(*at).is_some_and(|o| !o.disabled));
        if let Some(at) = keep {
            now.active = Some(at);
            now.picked = self.picked;
            now.show_active();
        }
        now
    }
}

/// How wide the widest label is, near enough: the pop-up has no font to measure with.
fn widest(options: &[SelectOption]) -> f32 {
    let longest = options.iter().map(|o| o.label.chars().count()).max();
    longest.unwrap_or(0) as f32 * 7.4
}

// ─── The shell's part: the pointer and the keys, and the layer that draws it ─────────────────

/// Opens the pop-up of the select `node` if it is a drop-down a person can open and has the
/// focus. Answers whether it opened.
pub(crate) fn open_under(state: &mut DomState, node: NodeId) -> bool {
    let focused = (state.snapshot.as_ref()).and_then(|snapshot| snapshot.dom.focused);
    let Some(list) = state.list.clone() else {
        return false;
    };
    let predicted = &state.predicted;
    let found = find(&list, node, &|n| predicted.get(&n).copied());
    match found {
        Some((anchor, options)) if focused == Some(node) && !options.is_empty() => {
            state.popup = Some(Popup::open(node, options, anchor, state.viewport));
            true
        }
        _ => false,
    }
}

/// The page changed under an open pop-up: it follows its select (the options and the place), and
/// closes when the select is gone, disabled, no longer a drop-down or without the focus.
pub(crate) fn sync(state: &mut DomState) {
    let Some(popup) = state.popup.take() else {
        return;
    };
    let focused = (state.snapshot.as_ref()).and_then(|snapshot| snapshot.dom.focused);
    let Some(list) = state.list.clone() else {
        return;
    };
    let predicted = &state.predicted;
    let found = find(&list, popup.select, &|n| predicted.get(&n).copied());
    if let Some((anchor, options)) = found
        && focused == Some(popup.select)
        && !options.is_empty()
    {
        state.popup = Some(popup.followed(options, anchor, state.viewport));
    }
}

/// Applies what a person did to the pop-up: closes it, and tells the page what was chosen.
fn settle(state: &mut DomState, outcome: Outcome) {
    match outcome {
        Outcome::Keep => {}
        Outcome::Close => state.popup = None,
        Outcome::Choose(index) => {
            if let Some(popup) = state.popup.take()
                && let Some(tell) = &state.tell
            {
                tell(ToRendererInput::Choose {
                    select: popup.select,
                    index,
                });
            }
        }
    }
}

/// A press at the page point `(x, y)` while a pop-up is open: the pop-up takes it. Answers
/// whether one was open (then the window needs drawing again).
pub(crate) fn on_press(state: &mut DomState, x: f32, y: f32) -> bool {
    let Some(popup) = &state.popup else {
        return false;
    };
    let outcome = popup.press(x, y);
    settle(state, outcome);
    true
}

/// The pointer moved while a pop-up is open: the option under it is the active one. Answers
/// whether one was open (then the window needs drawing again).
pub(crate) fn on_move(state: &mut DomState, x: f32, y: f32) -> bool {
    let Some(popup) = &mut state.popup else {
        return false;
    };
    popup.hover(x, y);
    true
}

/// A key while a pop-up is open goes to it. Answers whether the pop-up took the key (the page
/// then hears nothing of it), so that the window is drawn again.
pub(crate) fn on_key(state: &mut DomState, key: &verve_dom::Key, down: bool, now: Instant) -> bool {
    let Some(popup) = &mut state.popup else {
        return false;
    };
    if down {
        let outcome = popup.key(&key.key, key.modifiers.alt, now);
        settle(state, outcome);
    }
    true
}

/// The wheel turned over the page point `(x, y)`: an open pop-up scrolls its rows when the point
/// is on it. Answers whether it did.
pub(crate) fn on_wheel(state: &mut DomState, (x, y): (f32, f32), lines: i32) -> bool {
    let Some(popup) = &mut state.popup else {
        return false;
    };
    if !popup.contains(x, y) {
        return false;
    }
    popup.scroll(lines);
    true
}

const WHITE: u32 = 0x00ff_ffff;
const INK: u32 = 0x0000_0000;
const GREY: u32 = 0x0080_8080;
const BORDER: u32 = 0x0076_7676;
const HIGHLIGHT: u32 = 0x0000_78d7;

/// The layer that draws the pop-up over the page: a bordered box of rows, the active one in the
/// accent colour with white text.
pub(crate) fn layer(popup: &Popup) -> impl IntoElement + use<> {
    let rows: Vec<_> = (popup.rows.iter())
        .skip(popup.first)
        .take(popup.shown)
        .map(|row| match row {
            Row::Group(label) => div()
                .h(px(ROW))
                .px(px(6.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(INK))
                .child(label.clone()),
            Row::Option(index) => {
                let option = &popup.options[*index];
                let on = popup.active == Some(*index) && !option.disabled;
                let indent = if option.group.is_some() { 16. } else { 6. };
                let (back, ink) = match (on, option.disabled) {
                    (true, _) => (HIGHLIGHT, WHITE),
                    (_, true) => (WHITE, GREY),
                    _ => (WHITE, INK),
                };
                div()
                    .h(px(ROW))
                    .pl(px(indent))
                    .bg(rgb(back))
                    .text_color(rgb(ink))
                    .child(option.label.clone())
            }
        })
        .collect();
    let boxed = div()
        .absolute()
        .left(px(popup.rect.x))
        .top(px(popup.rect.y))
        .w(px(popup.rect.width))
        .h(px(popup.rect.height))
        .bg(rgb(WHITE))
        .border_1()
        .border_color(rgb(BORDER))
        .text_size(px(13.3333))
        .line_height(px(ROW))
        .overflow_hidden()
        .children(rows);
    deferred(boxed).with_priority(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(label: &str, selected: bool, disabled: bool) -> SelectOption {
        SelectOption {
            value: label.to_owned(),
            label: label.to_owned(),
            selected,
            disabled,
            group: None,
        }
    }

    fn fruit() -> Popup {
        let options: Arc<[SelectOption]> = Arc::from(vec![
            option("Apple", true, false),
            option("Banana", false, false),
            option("Blueberry", false, true),
            option("Cherry", false, false),
            option("Date", false, false),
        ]);
        Popup::open(7, options, Rect::new(10., 10., 80., 20.), (400., 300.))
    }

    fn press(popup: &mut Popup, key: &str) -> Outcome {
        popup.key(key, false, Instant::now())
    }

    #[test]
    fn the_arrows_skip_disabled_options_and_stop_at_the_ends() {
        let mut popup = fruit();
        assert_eq!(popup.active, Some(0));
        press(&mut popup, "ArrowDown");
        assert_eq!(popup.active, Some(1));
        press(&mut popup, "ArrowDown");
        assert_eq!(popup.active, Some(3));
        press(&mut popup, "End");
        press(&mut popup, "ArrowDown");
        assert_eq!(popup.active, Some(4));
        press(&mut popup, "Home");
        press(&mut popup, "ArrowUp");
        assert_eq!(popup.active, Some(0));
    }

    #[test]
    fn enter_chooses_the_active_option_and_escape_does_after_the_keys_moved_it() {
        let mut popup = fruit();
        // Escape before anything moved chooses nothing; after the keys moved it, the option (a
        // hidden window of Electron 39.8.10 fired `input` and `change` for it).
        assert_eq!(press(&mut popup, "Escape"), Outcome::Close);
        press(&mut popup, "ArrowDown");
        assert_eq!(press(&mut popup, "Enter"), Outcome::Choose(1));
        assert_eq!(press(&mut popup, "Escape"), Outcome::Choose(1));
        let alt_down = popup.key("ArrowDown", true, Instant::now());
        assert_eq!(alt_down, Outcome::Choose(1));
        // The pointer moving the highlight is not the keys moving it.
        popup.hover(popup.rect.x + 5., popup.rect.y + EDGE + 0.5 * ROW);
        assert_eq!(press(&mut popup, "Escape"), Outcome::Close);
    }

    #[test]
    fn a_pop_up_follows_its_select_when_the_options_change() {
        let popup = fruit();
        let same = popup
            .clone()
            .followed(popup.options.clone(), popup.anchor, (400., 300.));
        assert_eq!(same.rect, popup.rect);
        let fewer: Arc<[SelectOption]> = Arc::from(vec![
            option("Apple", true, false),
            option("Banana", false, false),
        ]);
        let mut moved = fruit();
        press(&mut moved, "ArrowDown");
        let moved = moved.followed(fewer, popup.anchor, (400., 300.));
        assert_eq!((moved.options.len(), moved.active), (2, Some(1)));
    }

    #[test]
    fn a_letter_jumps_to_the_next_option_that_starts_with_it_and_letters_in_a_row_make_a_prefix() {
        let now = Instant::now();
        let mut popup = fruit();
        popup.key("b", false, now);
        assert_eq!(popup.active, Some(1));
        // The same letter again goes on to the next B, skipping the disabled one, and round.
        popup.key("b", false, now + std::time::Duration::from_millis(100));
        assert_eq!(popup.active, Some(1));
        let mut popup = fruit();
        popup.key("c", false, now);
        assert_eq!(popup.active, Some(3));
        // A second later it is a new word.
        popup.key("d", false, now + std::time::Duration::from_secs(2));
        assert_eq!(popup.active, Some(4));
    }

    #[test]
    fn a_press_chooses_an_option_closes_outside_and_ignores_a_disabled_one() {
        let popup = fruit();
        let (x, top) = (popup.rect.x + 5., popup.rect.y + EDGE);
        assert_eq!(popup.press(x, top + 1.5 * ROW), Outcome::Choose(1));
        assert_eq!(popup.press(x, top + 2.5 * ROW), Outcome::Keep);
        assert_eq!(popup.press(0., 0.), Outcome::Close);
    }

    #[test]
    fn the_popup_hangs_below_flips_above_and_scrolls_when_it_does_not_fit() {
        let many: Arc<[SelectOption]> = (0..30)
            .map(|i| option(&format!("item {i}"), i == 25, false))
            .collect();
        let anchor = Rect::new(10., 10., 80., 20.);
        let below = Popup::open(1, many.clone(), anchor, (400., 300.));
        assert_eq!((below.rect.y, below.shown), (30., 13));
        assert!(below.first <= 25 && 25 < below.first + below.shown);
        let low = Rect::new(10., 270., 80., 20.);
        let above = Popup::open(1, many, low, (400., 300.));
        assert_eq!(above.rect.y + above.rect.height, 270.);
        let few: Arc<[SelectOption]> = Arc::from(vec![option("a", true, false)]);
        let edge = Popup::open(1, few, Rect::new(380., 10., 20., 20.), (400., 300.));
        assert!(edge.rect.x + edge.rect.width <= 400.);
    }
}
