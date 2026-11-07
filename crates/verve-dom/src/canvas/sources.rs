//! Which picture an `<img>` shows and which addresses a page's styles ask for: `srcset`, `sizes`
//! and `<picture>` (HTML, "updating the image data"), and the `url()`s of `background-image` and
//! `list-style-image`.
//!
//! The choice is made from the viewport and the pixel density the document was given, with no
//! layout, as the HTML algorithm has it. The rule for the density is the specification's: the
//! first candidate whose density reaches the screen's, else the densest one.

use crate::css::split_slices;
use crate::css::{Environment, MediaQuery, length_calc};
use crate::dom::NodeId;
use crate::state::State;

/// One entry of a `srcset`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub url: String,
    /// The `Nx` descriptor.
    pub density: Option<f32>,
    /// The `Nw` descriptor.
    pub width: Option<f32>,
}

/// The entries of `srcset`: a URL, then descriptors up to the next comma.
pub(crate) fn parse_srcset(text: &str) -> Vec<Candidate> {
    let mut found = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            return found;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (url, after) = rest.split_at(end);
        let trimmed = url.trim_end_matches(',');
        // A URL that ends in commas has no descriptors: the commas end the entry.
        let (descriptors, next) = if trimmed.len() < url.len() {
            ("", after)
        } else {
            let stop = comma_outside_parens(after).unwrap_or(after.len());
            (&after[..stop], &after[stop..])
        };
        rest = next;
        let mut candidate = Candidate {
            url: trimmed.to_owned(),
            density: None,
            width: None,
        };
        let mut valid = true;
        for token in descriptors.split_ascii_whitespace() {
            let (number, unit) = token.split_at(token.len() - 1);
            let value = number.parse::<f32>().ok().filter(|v| *v > 0.);
            match (unit, value) {
                ("x", Some(v)) if candidate.density.is_none() && candidate.width.is_none() => {
                    candidate.density = Some(v);
                }
                ("w", Some(v)) if candidate.density.is_none() && candidate.width.is_none() => {
                    candidate.width = Some(v);
                }
                _ => valid = false,
            }
        }
        if valid && !candidate.url.is_empty() {
            found.push(candidate);
        }
    }
}

fn comma_outside_parens(text: &str) -> Option<usize> {
    let mut depth = 0_i32;
    for (at, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => return Some(at),
            _ => {}
        }
    }
    None
}

/// The width the picture is laid out at for the `sizes` attribute: the first entry whose media
/// condition holds, `100vw` when none does.
fn slot_width(sizes: &str, env: &Environment) -> f32 {
    for entry in split_slices(sizes, |c| c == ',') {
        let entry = entry.trim();
        let pieces: Vec<&str> = split_slices(entry, char::is_whitespace)
            .into_iter()
            .filter(|piece| !piece.is_empty())
            .collect();
        let Some(length) = pieces.last().copied() else {
            continue;
        };
        let condition = &entry[..entry.len() - length.len()];
        let holds = condition.trim().is_empty() || MediaQuery::new(condition.trim()).matches(env);
        if !holds {
            continue;
        }
        if let Some(size) = length_calc(length, 16., 16., env) {
            return (size.px + size.percent * env.width / 100.).max(0.);
        }
    }
    env.width
}

/// The candidate to show: its address and the density it is shown at.
pub(crate) fn choose(
    fallback: Option<&str>,
    srcset: &str,
    sizes: &str,
    env: &Environment,
) -> Option<(String, f32)> {
    let list = parse_srcset(srcset);
    let slot = slot_width(sizes, env);
    let widths = list.iter().any(|c| c.width.is_some());
    let mut sized: Vec<(String, f32)> = list
        .into_iter()
        .map(|c| match (c.width, c.density) {
            (Some(width), _) if slot > 0. => (c.url, width / slot),
            (Some(_), _) => (c.url, 1.),
            (None, density) => (c.url, density.unwrap_or(1.)),
        })
        .collect();
    // The `src` is the 1x candidate when no entry is, and is not one beside width entries.
    if let Some(src) = fallback.map(str::trim).filter(|src| !src.is_empty())
        && !widths
        && !sized.iter().any(|(_, density)| *density == 1.)
    {
        sized.push((src.to_owned(), 1.));
    }
    sized.sort_by(|a, b| a.1.total_cmp(&b.1));
    let wanted = env.device_pixel_ratio;
    let at = sized
        .iter()
        .position(|(_, density)| *density >= wanted)
        .or_else(|| sized.len().checked_sub(1))?;
    Some(sized.swap_remove(at))
}

/// The image types a `<source type>` may name and this renderer can show.
fn supported_type(mime: &str) -> bool {
    matches!(
        mime.trim().to_ascii_lowercase().as_str(),
        "image/png"
            | "image/apng"
            | "image/jpeg"
            | "image/jpg"
            | "image/pjpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "image/svg+xml"
    )
}

/// What the `<img>` `node` shows now: the address as written (not yet resolved against the
/// document) and its density. `None` when it names nothing.
pub(crate) fn current(state: &State, node: NodeId) -> Option<(String, f32)> {
    let dom = &state.dom;
    let env = &state.env;
    if let Some(parent) = dom
        .parent(node)
        .filter(|p| dom.get(*p).is_some_and(|n| n.tag() == Some("picture")))
    {
        for &sibling in dom.children(parent) {
            if sibling == node {
                break;
            }
            if dom.get(sibling).and_then(|n| n.tag()) != Some("source") {
                continue;
            }
            let attr = |name: &str| dom.get_attr(sibling, name);
            if attr("type").is_some_and(|t| !supported_type(&t)) {
                continue;
            }
            if attr("media").is_some_and(|m| !MediaQuery::new(&m).matches(env)) {
                continue;
            }
            let srcset = attr("srcset").unwrap_or_default();
            if srcset.trim().is_empty() {
                continue;
            }
            let sizes = attr("sizes").unwrap_or_default();
            if let Some(chosen) = choose(None, &srcset, &sizes, env) {
                return Some(chosen);
            }
        }
    }
    let srcset = dom.get_attr(node, "srcset").unwrap_or_default();
    let sizes = dom.get_attr(node, "sizes").unwrap_or_default();
    let src = dom.get_attr(node, "src");
    choose(src.as_deref(), &srcset, &sizes, env)
}

/// The raw address in one layer of a CSS image list: `url("a.png")` gives `a.png`; a gradient,
/// `none` and the like give `None`.
pub fn url_of_layer(layer: &str) -> Option<&str> {
    let layer = layer.trim();
    let head = layer.get(..4)?;
    if !head.eq_ignore_ascii_case("url(") || !layer.ends_with(')') {
        return None;
    }
    let inner = layer[4..layer.len() - 1].trim();
    let unquoted = inner
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            inner
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(inner);
    (!unquoted.is_empty()).then_some(unquoted)
}

/// Every address `value` (a `background-image` or `list-style-image`) names, resolved against
/// `base`.
pub fn css_urls(base: &str, value: &str) -> Vec<String> {
    if !value.contains("url(") {
        return Vec::new();
    }
    split_slices(value, |c| c == ',')
        .into_iter()
        .filter_map(url_of_layer)
        .filter_map(|raw| resolve_against(base, raw))
        .collect()
}

/// `raw` resolved against `base`, when it names something an image may be fetched from: the
/// rule of an `<img src>`, so an empty address names nothing.
pub fn resolve_against(base: &str, raw: &str) -> Option<String> {
    super::img::resolve(&url::Url::parse(base).ok()?, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(width: f32, dpr: f32) -> Environment {
        Environment {
            width,
            device_pixel_ratio: dpr,
            ..Environment::default()
        }
    }

    fn pick(srcset: &str, sizes: &str, width: f32, dpr: f32) -> String {
        choose(None, srcset, sizes, &env(width, dpr))
            .expect("a choice")
            .0
    }

    #[test]
    fn a_srcset_is_split_at_commas_that_end_an_entry() {
        let list = parse_srcset("a.png 1x, b.png 2x,c.png 300w");
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].density, Some(2.));
        assert_eq!(list[2].width, Some(300.));
        let commas = parse_srcset("data:image/png;base64,AAAA 1x");
        assert_eq!(commas[0].url, "data:image/png;base64,AAAA");
    }

    // Observed in Electron 39.8.10 (Chromium 142), `--force-device-scale-factor` 1 and 2, images
    // served over http: the first candidate that reaches the density wins, the densest otherwise.
    #[test]
    fn density_candidates_are_chosen_as_chromium_chose_them() {
        assert_eq!(pick("a 1x, b 2x, c 3x", "", 800., 1.), "a");
        assert_eq!(pick("a 1x, b 2x, c 3x", "", 800., 2.), "b");
        assert_eq!(pick("a 3x, b 2x, c 1x", "", 800., 1.), "c");
        assert_eq!(pick("a 1.5x, b 2.5x", "", 800., 1.), "a");
        assert_eq!(pick("a 1.5x, b 2.5x", "", 800., 2.), "b");
        assert_eq!(pick("a, b 2x", "", 800., 1.), "a");
        assert_eq!(pick("b 2x", "", 800., 1.), "b");
        let with_src = choose(Some("s"), "b 2x", "", &env(800., 1.)).expect("a choice");
        assert_eq!(with_src.0, "s");
        let with_src = choose(Some("s"), "b 2x", "", &env(800., 2.)).expect("a choice");
        assert_eq!(with_src, ("b".into(), 2.));
    }

    #[test]
    fn width_candidates_are_chosen_against_the_sizes_slot() {
        let set = "a 100w, b 200w, c 300w";
        assert_eq!(pick(set, "150px", 800., 1.), "b");
        assert_eq!(pick(set, "150px", 800., 2.), "c");
        assert_eq!(pick(set, "(max-width: 500px) 100px, 400px", 800., 1.), "c");
        assert_eq!(pick(set, "(max-width: 500px) 100px, 400px", 400., 1.), "a");
        assert_eq!(pick(set, "calc(50vw - 100px)", 800., 1.), "c");
        assert_eq!(pick(set, "", 800., 1.), "c");
        assert_eq!(pick(set, "", 200., 1.), "b");
    }

    #[test]
    fn a_css_url_layer_is_found_among_gradients() {
        assert_eq!(
            css_urls(
                "http://h/p/",
                "url(a.png), linear-gradient(red, blue), url('b.png')"
            ),
            ["http://h/p/a.png", "http://h/p/b.png"]
        );
        assert!(css_urls("http://h/", "none").is_empty());
    }
}
