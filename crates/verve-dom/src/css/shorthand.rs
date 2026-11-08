//! Shorthand properties opened into their longhands: `padding` into its four sides, `border`
//! into widths, styles and colors. What this engine reads of a value is its longhands; the
//! shorthand is kept too, as written, for `getComputedStyle` to answer.

use super::calc::{length, milliseconds};
use super::cascade::Style;
use super::color::color;
use super::easing::Easing;
use super::syntax::split_outside;
use super::values::ROOT_FONT_SIZE;

/// Opens one shorthand: writes its longhands and itself into the style.
type Expand = fn(&mut Style, &str, &str);

/// The expansion of the shorthand `key`, if it is one.
pub(super) fn expansion(key: &str) -> Option<Expand> {
    Some(match key {
        "padding" | "margin" | "inset" | "scroll-margin" | "scroll-padding" => sides,
        "border-width" | "border-style" | "border-color" => border_sides,
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            border
        }
        "border-radius" => radius,
        "background" => background,
        "gap" | "grid-gap" => gap,
        "flex" => flex,
        "flex-flow" => flex_flow,
        "overflow" => overflow,
        "place-items" | "place-content" | "place-self" => place,
        "transition" => transition,
        "animation" => animation,
        _ => return None,
    })
}

/// The properties a declaration of `name` sets: `name` itself, and what it opens into when it is
/// a shorthand.
pub(super) fn longhands(name: &str) -> Vec<String> {
    let mut opened = Style::new();
    if let Some(expand) = expansion(name) {
        expand(&mut opened, name, "0");
    }
    opened.insert(name.to_owned(), String::new());
    opened.into_keys().collect()
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn sides(style: &mut Style, key: &str, value: &str) {
    for (side, v) in SIDES.iter().zip(box_sides(value)) {
        let name = if key == "inset" {
            (*side).to_owned()
        } else {
            format!("{key}-{side}")
        };
        style.insert(name, v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn border_sides(style: &mut Style, key: &str, value: &str) {
    let part = key.trim_start_matches("border-");
    for (side, v) in SIDES.iter().zip(box_sides(value)) {
        style.insert(format!("border-{side}-{part}"), v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn border(style: &mut Style, key: &str, value: &str) {
    let (width, line, color) = border_parts(value);
    let targets: Vec<String> = match key {
        "border" => SIDES.iter().map(|s| format!("border-{s}")).collect(),
        other => vec![other.to_owned()],
    };
    for target in &targets {
        style.insert(format!("{target}-width"), width.clone());
        style.insert(format!("{target}-style"), line.clone());
        style.insert(format!("{target}-color"), color.clone());
        style.insert(target.clone(), value.to_owned());
    }
    if key == "border" {
        style.insert("border-width".into(), width);
        style.insert("border-style".into(), line);
        style.insert("border-color".into(), color);
        style.insert("border".into(), value.to_owned());
    }
}

fn radius(style: &mut Style, key: &str, value: &str) {
    let first = value.split('/').next().unwrap_or(value);
    let corners = ["top-left", "top-right", "bottom-right", "bottom-left"];
    for (corner, v) in corners.iter().zip(box_sides(first)) {
        style.insert(format!("border-{corner}-radius"), v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

/// One layer of a `background` shorthand, as the longhands it sets.
struct Layer {
    image: String,
    position: Vec<String>,
    size: Vec<String>,
    repeat: Vec<String>,
    boxes: Vec<String>,
}

/// Whether a word after the `/` of a `background` is part of its size.
fn is_size_word(word: &str) -> bool {
    matches!(word, "auto" | "cover" | "contain")
        || word.starts_with("calc(")
        || word.ends_with('%')
        || word.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == '-')
}

fn background_layer(text: &str) -> Layer {
    let mut layer = Layer {
        image: "none".into(),
        position: Vec::new(),
        size: Vec::new(),
        repeat: Vec::new(),
        boxes: Vec::new(),
    };
    let mut sizing = false;
    // `center/cover` is three words: a `/` outside a function stands alone.
    let words = split_outside(text, ' ').into_iter().flat_map(|word| {
        let word = word.trim().to_owned();
        if word.contains('(') || !word.contains('/') {
            return vec![word];
        }
        let mut pieces = Vec::new();
        for (at, piece) in word.split('/').enumerate() {
            if at > 0 {
                pieces.push("/".to_owned());
            }
            pieces.push(piece.to_owned());
        }
        pieces
    });
    for token in words {
        match token.as_str() {
            "" => {}
            "/" => sizing = true,
            "repeat" | "repeat-x" | "repeat-y" | "no-repeat" | "space" | "round" => {
                layer.repeat.push(token);
            }
            "border-box" | "padding-box" | "content-box" => layer.boxes.push(token),
            "scroll" | "fixed" | "local" => {}
            _ if token.starts_with("url(") || token.contains("gradient(") => layer.image = token,
            "none" => {}
            _ if sizing && is_size_word(&token) => {
                sizing = layer.size.is_empty() && token != "cover" && token != "contain";
                layer.size.push(token);
            }
            _ if color(&token).is_some() || token == "currentcolor" => {}
            _ => {
                sizing = false;
                layer.position.push(token);
            }
        }
    }
    layer
}

fn background(style: &mut Style, _: &str, value: &str) {
    const INITIAL: [&str; 7] = [
        "transparent",
        "none",
        "0% 0%",
        "auto",
        "repeat",
        "padding-box",
        "border-box",
    ];
    const LONGHANDS: [&str; 7] = [
        "background-color",
        "background-image",
        "background-position",
        "background-size",
        "background-repeat",
        "background-origin",
        "background-clip",
    ];
    style.insert("background".into(), value.to_owned());
    // A `var()` cannot be split before it is read: the longhands hold their initial values, marked
    // as a guess, until the cascade reads the variable and opens the shorthand again.
    let varied = value.contains("var(");
    for longhand in LONGHANDS {
        if varied {
            style.insert(pending(longhand), String::new());
        } else {
            style.remove(&pending(longhand));
        }
    }
    if varied {
        for (longhand, initial) in LONGHANDS.iter().zip(INITIAL) {
            style.insert((*longhand).into(), initial.into());
        }
        return;
    }
    let layers: Vec<Layer> = split_outside(value, ',')
        .iter()
        .map(|layer| background_layer(layer))
        .collect();
    // The color is what a drawer reads: the last word of the last layer that is one.
    let color = split_outside(value, ' ')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .rfind(|p| color(p).is_some() || p == "currentcolor")
        .unwrap_or_else(|| "transparent".to_owned());
    style.insert("background-color".into(), color);
    let join =
        |part: &dyn Fn(&Layer) -> String| layers.iter().map(part).collect::<Vec<_>>().join(", ");
    let or = |words: &[String], default: &str| {
        if words.is_empty() {
            default.to_owned()
        } else {
            words.join(" ")
        }
    };
    style.insert("background-image".into(), join(&|l| l.image.clone()));
    style.insert(
        "background-position".into(),
        join(&|l| or(&l.position, "0% 0%")),
    );
    style.insert("background-size".into(), join(&|l| or(&l.size, "auto")));
    style.insert(
        "background-repeat".into(),
        join(&|l| or(&l.repeat, "repeat")),
    );
    let origin = |l: &Layer| or(&l.boxes[..l.boxes.len().min(1)], "padding-box");
    style.insert("background-origin".into(), join(&origin));
    let clip = |l: &Layer| match l.boxes.as_slice() {
        [] => "border-box".to_owned(),
        [only] => only.clone(),
        [_, clip, ..] => clip.clone(),
    };
    style.insert("background-clip".into(), join(&clip));
}

fn gap(style: &mut Style, _: &str, value: &str) {
    let mut parts = value.split_whitespace();
    let row = parts.next().unwrap_or("0").to_owned();
    let column = parts.next().map_or_else(|| row.clone(), str::to_owned);
    style.insert("row-gap".into(), row);
    style.insert("column-gap".into(), column);
    // `grid-gap` is the old name of `gap`.
    style.insert("gap".into(), value.to_owned());
}

fn flex(style: &mut Style, key: &str, value: &str) {
    let (grow, shrink, basis) = flex_parts(value);
    style.insert("flex-grow".into(), grow);
    style.insert("flex-shrink".into(), shrink);
    style.insert("flex-basis".into(), basis);
    style.insert(key.to_owned(), value.to_owned());
}

fn flex_flow(style: &mut Style, key: &str, value: &str) {
    for part in value.split_whitespace() {
        if part.contains("wrap") {
            style.insert("flex-wrap".into(), part.to_owned());
        } else {
            style.insert("flex-direction".into(), part.to_owned());
        }
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn overflow(style: &mut Style, key: &str, value: &str) {
    let mut parts = value.split_whitespace();
    let x = parts.next().unwrap_or("visible").to_owned();
    let y = parts.next().map_or_else(|| x.clone(), str::to_owned);
    style.insert("overflow-x".into(), x);
    style.insert("overflow-y".into(), y);
    style.insert(key.to_owned(), value.to_owned());
}

fn place(style: &mut Style, key: &str, value: &str) {
    let part = key.trim_start_matches("place-");
    let mut parts = value.split_whitespace();
    let align = parts.next().unwrap_or("normal").to_owned();
    let justify = parts.next().map_or_else(|| align.clone(), str::to_owned);
    style.insert(format!("align-{part}"), align);
    style.insert(format!("justify-{part}"), justify);
    style.insert(key.to_owned(), value.to_owned());
}

fn border_parts(value: &str) -> (String, String, String) {
    let mut width = "medium".to_owned();
    let mut line = "none".to_owned();
    let mut color = "currentcolor".to_owned();
    for part in split_outside(value, ' ') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if length(part, ROOT_FONT_SIZE).is_some() || matches!(part, "thin" | "medium" | "thick") {
            width = part.to_owned();
        } else if matches!(
            part,
            "none"
                | "hidden"
                | "solid"
                | "dashed"
                | "dotted"
                | "double"
                | "groove"
                | "ridge"
                | "inset"
                | "outset"
        ) {
            line = part.to_owned();
        } else {
            color = part.to_owned();
        }
    }
    if line == "none" {
        width = "0".to_owned();
    }
    (width, line, color)
}

fn flex_parts(value: &str) -> (String, String, String) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["none"] => ("0".into(), "0".into(), "auto".into()),
        ["auto"] => ("1".into(), "1".into(), "auto".into()),
        [one] if one.parse::<f32>().is_ok() => ((*one).into(), "1".into(), "0%".into()),
        [one] => ("1".into(), "1".into(), (*one).into()),
        [a, b] if b.parse::<f32>().is_ok() => ((*a).into(), (*b).into(), "0%".into()),
        [a, b] => ((*a).into(), "1".into(), (*b).into()),
        [a, b, c, ..] => ((*a).into(), (*b).into(), (*c).into()),
        [] => ("0".into(), "1".into(), "auto".into()),
    }
}

fn box_sides(value: &str) -> [String; 4] {
    let parts: Vec<String> = split_outside(value, ' ')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    let pick = |i: usize| parts.get(i).cloned().unwrap_or_else(|| "0".to_owned());
    match parts.len() {
        1 => [pick(0), pick(0), pick(0), pick(0)],
        2 => [pick(0), pick(1), pick(0), pick(1)],
        3 => [pick(0), pick(1), pick(2), pick(1)],
        _ => [pick(0), pick(1), pick(2), pick(3)],
    }
}

/// One longhand of a shorthand that is a list of layers: which word of a layer fills it, and
/// what it is when none does. A word fills the first slot that takes it and is still empty, so
/// the first time is the duration and the second the delay.
struct Slot {
    longhand: &'static str,
    takes: fn(&str) -> bool,
    initial: &'static str,
}

const TRANSITION: &[Slot] = &[
    Slot {
        longhand: "transition-duration",
        takes: is_time,
        initial: "0s",
    },
    Slot {
        longhand: "transition-delay",
        takes: is_time,
        initial: "0s",
    },
    Slot {
        longhand: "transition-timing-function",
        takes: Easing::is_easing,
        initial: "ease",
    },
    Slot {
        longhand: "transition-property",
        takes: is_name,
        initial: "all",
    },
];

const ANIMATION: &[Slot] = &[
    Slot {
        longhand: "animation-duration",
        takes: is_time,
        initial: "0s",
    },
    Slot {
        longhand: "animation-delay",
        takes: is_time,
        initial: "0s",
    },
    Slot {
        longhand: "animation-timing-function",
        takes: Easing::is_easing,
        initial: "ease",
    },
    Slot {
        longhand: "animation-iteration-count",
        takes: |word| word == "infinite" || word.parse::<f32>().is_ok(),
        initial: "1",
    },
    Slot {
        longhand: "animation-direction",
        takes: |word| {
            matches!(
                word,
                "normal" | "reverse" | "alternate" | "alternate-reverse"
            )
        },
        initial: "normal",
    },
    Slot {
        longhand: "animation-fill-mode",
        takes: |word| matches!(word, "none" | "forwards" | "backwards" | "both"),
        initial: "none",
    },
    Slot {
        longhand: "animation-play-state",
        takes: |word| matches!(word, "running" | "paused"),
        initial: "running",
    },
    Slot {
        longhand: "animation-name",
        takes: is_name,
        initial: "none",
    },
];

/// Whether a word is a time: a number with `s` or `ms`. Not a name that ends in `s`.
fn is_time(word: &str) -> bool {
    milliseconds(word).is_some()
}

/// Whether a word can be a property or animation name; a `var()` is not yet known to be.
fn is_name(word: &str) -> bool {
    !word.contains('(')
}

fn transition(style: &mut Style, key: &str, value: &str) {
    layers(style, key, value, TRANSITION);
}

fn animation(style: &mut Style, key: &str, value: &str) {
    layers(style, key, value, ANIMATION);
}

/// `opacity 150ms ease 50ms, transform 1s`: each comma-separated layer's words into the slots,
/// and each slot's layers into the longhand's list, which line up by position.
fn layers(style: &mut Style, key: &str, value: &str, slots: &[Slot]) {
    let mut lists: Vec<Vec<String>> = vec![Vec::new(); slots.len()];
    for layer in split_outside(value, ',') {
        let mut filled: Vec<Option<String>> = vec![None; slots.len()];
        for word in split_outside(&layer, ' ') {
            let word = word.trim();
            let found = slots
                .iter()
                .zip(&filled)
                .position(|(slot, taken)| taken.is_none() && (slot.takes)(word));
            if let Some(at) = found.filter(|_| !word.is_empty()) {
                filled[at] = Some(word.to_owned());
            }
        }
        for ((list, slot), word) in lists.iter_mut().zip(slots).zip(filled) {
            list.push(word.unwrap_or_else(|| slot.initial.to_owned()));
        }
    }
    let varied = value.contains("var(");
    for (slot, list) in slots.iter().zip(lists) {
        if varied {
            style.insert(pending(slot.longhand), String::new());
        }
        style.insert(slot.longhand.to_owned(), list.join(", "));
    }
    style.insert(key.to_owned(), value.to_owned());
}

/// The key that marks `longhand` as still holding what a shorthand with a `var()` in it made of
/// it, which is a guess until the `var()` is read. A declaration of the longhand itself takes
/// the mark away, so that reading the shorthand later does not overwrite what won over it.
pub(super) fn pending(longhand: &str) -> String {
    format!("\u{0}pending:{longhand}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(key: &str, value: &str) -> Style {
        let mut style = Style::new();
        expansion(key).expect("a shorthand")(&mut style, key, value);
        style
    }

    #[test]
    fn a_background_shorthand_opens_into_its_layers() {
        let style = opened(
            "background",
            "url(a/b.png) no-repeat center/cover content-box, linear-gradient(red, blue) 10px 20px / 5px auto, #fff",
        );
        assert_eq!(
            style["background-image"],
            "url(a/b.png), linear-gradient(red, blue), none"
        );
        assert_eq!(style["background-position"], "center, 10px 20px, 0% 0%");
        assert_eq!(style["background-size"], "cover, 5px auto, auto");
        assert_eq!(style["background-repeat"], "no-repeat, repeat, repeat");
        assert_eq!(
            style["background-origin"],
            "content-box, padding-box, padding-box"
        );
        assert_eq!(style["background-color"], "#fff");
    }

    #[test]
    fn a_background_without_an_image_resets_every_longhand() {
        let style = opened("background", "red");
        assert_eq!(style["background-color"], "red");
        for (longhand, initial) in [
            ("background-image", "none"),
            ("background-position", "0% 0%"),
            ("background-size", "auto"),
            ("background-repeat", "repeat"),
            ("background-origin", "padding-box"),
            ("background-clip", "border-box"),
        ] {
            assert_eq!(style[longhand], initial, "{longhand}");
        }
        let style = opened("background", "var(--bg)");
        assert_eq!(style["background-repeat"], "repeat");
        assert!(style.contains_key(&pending("background-repeat")));
    }

    #[test]
    fn grid_gap_is_gap() {
        let style = opened("grid-gap", "4px 8px");
        assert_eq!(style["gap"], "4px 8px");
        assert_eq!(
            (style["row-gap"].as_str(), style["column-gap"].as_str()),
            ("4px", "8px")
        );
        assert!(!style.contains_key("grid-gap"));
    }

    #[test]
    fn transition_layers_line_up_by_position() {
        let style = opened("transition", "opacity 150ms ease-in 50ms, transform 1s");
        assert_eq!(style["transition-property"], "opacity, transform");
        assert_eq!(style["transition-duration"], "150ms, 1s");
        assert_eq!(style["transition-delay"], "50ms, 0s");
        assert_eq!(style["transition-timing-function"], "ease-in, ease");
        let style = opened("transition", "all 0.3s cubic-bezier(0.4, 0, 0.2, 1)");
        assert_eq!(style["transition-property"], "all");
        assert_eq!(
            style["transition-timing-function"],
            "cubic-bezier(0.4, 0, 0.2, 1)"
        );
    }

    #[test]
    fn a_name_that_ends_in_s_is_a_name_not_a_time() {
        // `spin-forms` ends in `s` and `forms` in `ms`; neither is a duration.
        let style = opened("animation", "spin-forms 2s ease-in-out 3 alternate both");
        assert_eq!(style["animation-name"], "spin-forms");
        assert_eq!(style["animation-duration"], "2s");
        assert_eq!(style["animation-iteration-count"], "3");
        assert_eq!(style["animation-direction"], "alternate");
        assert_eq!(style["animation-fill-mode"], "both");
        let style = opened("animation", "ms 1s, fade-in-s 1s infinite");
        assert_eq!(style["animation-name"], "ms, fade-in-s");
        assert_eq!(style["animation-iteration-count"], "1, infinite");
    }
}
