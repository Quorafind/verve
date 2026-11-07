//! Which fonts a `font-family` list names: the generic families, the fallback chain and the
//! faces a page registered with `@font-face`.
//!
//! This is data and rules, no fonts: [`stack`] turns the families a page declared into the list of
//! concrete family names [`crate::TextMeasurer::resolve`] is given, and the measurer takes the
//! first one the system (or a registered face) has. A face the page registered comes first under
//! its own name, even over a system font of the same name or a generic family.

use std::collections::HashMap;

/// The operating system the family names are those of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    /// The platform this is built for.
    pub const fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Linux
        }
    }
}

/// The faces a page registered, by family name.
#[derive(Clone, Debug, Default)]
pub struct FontRegistry {
    families: HashMap<String, Vec<Face>>,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Face {
    weight: f32,
    italic: bool,
}

impl FontRegistry {
    /// Notes a face. Answers whether it is new: a face registered twice changes nothing.
    pub fn register(&mut self, family: &str, weight: f32, italic: bool) -> bool {
        let faces = self.families.entry(key(family)).or_default();
        let face = Face { weight, italic };
        if faces.contains(&face) {
            return false;
        }
        faces.push(face);
        self.generation += 1;
        true
    }

    /// Whether any face of `family` is registered.
    pub fn contains(&self, family: &str) -> bool {
        self.families.contains_key(&key(family))
    }

    /// How many faces were ever registered: a layout made at another number is out of date.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The weight and slant of the registered face of `family` that best fits a request, by
    /// CSS's font matching: a slant that matches, then the weight in CSS Fonts 4 §5.2's order.
    /// Below 400 the lighter faces come first, nearest first, then the heavier; above 500 the
    /// heavier, then the lighter; from 400 to 500 the faces up to 500 that are not lighter than
    /// the request, then the lighter ones, then those above 500. `None` when the family has no face.
    pub fn best(&self, family: &str, weight: f32, italic: bool) -> Option<(f32, bool)> {
        let faces = self.families.get(&key(family))?;
        faces
            .iter()
            .min_by(|a, b| rank(a, weight, italic).total_cmp(&rank(b, weight, italic)))
            .map(|face| (face.weight, face.italic))
    }
}

/// How far a face is from a request: any slant mismatch outweighs every weight difference, and
/// the weights come in the order CSS tries them (see [`FontRegistry::best`]).
fn rank(face: &Face, weight: f32, italic: bool) -> f32 {
    let slant = if face.italic == italic { 0. } else { 10_000. };
    let at = face.weight;
    let order = if weight < 400. {
        if at <= weight {
            weight - at
        } else {
            1_000. + at - weight
        }
    } else if weight > 500. {
        if at >= weight {
            at - weight
        } else {
            1_000. + weight - at
        }
    } else if at >= weight && at <= 500. {
        at - weight
    } else if at < weight {
        1_000. + weight - at
    } else {
        2_000. + at - 500.
    };
    slant + order
}

fn key(family: &str) -> String {
    family.trim().to_ascii_lowercase()
}

/// A `font-family` list read into names: split at commas outside quotes, quotes taken off.
pub fn split_families(text: &str) -> Vec<String> {
    crate::value::split_outside(text, ',')
        .into_iter()
        .map(unquote)
        .filter(|name| !name.is_empty())
        .collect()
}

fn unquote(name: &str) -> String {
    verve_dom::css::unquote(name).unwrap_or_else(|| name.trim().to_owned())
}

/// The concrete names for one generic family on `platform`; `None` for a name that is not one.
/// A browser's default `serif` on every platform is Times New Roman (or the platform's twin).
fn generic(platform: Platform, name: &str) -> Option<&'static [&'static str]> {
    let lower = name.to_ascii_lowercase();
    Some(match (platform, lower.as_str()) {
        (Platform::Windows, "serif" | "ui-serif") => &["Times New Roman"],
        (
            Platform::Windows,
            "sans-serif" | "ui-sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont"
            | "ui-rounded" | "-webkit-standard",
        ) => &["Segoe UI"],
        (Platform::Windows, "monospace" | "ui-monospace") => &["Consolas"],
        (Platform::Windows, "cursive") => &["Comic Sans MS"],
        (Platform::Windows, "fantasy") => &["Impact"],
        (Platform::Windows, "emoji") => &["Segoe UI Emoji"],
        (Platform::Windows, "math") => &["Cambria Math"],
        (Platform::Mac, "serif" | "ui-serif") => &["Times"],
        (Platform::Mac, "sans-serif" | "ui-sans-serif") => &["Helvetica"],
        (
            Platform::Mac,
            "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-rounded"
            | "-webkit-standard",
        ) => &[".SystemUIFont"],
        (Platform::Mac, "monospace" | "ui-monospace") => &["Menlo"],
        (Platform::Mac, "cursive") => &["Apple Chancery"],
        (Platform::Mac, "fantasy") => &["Papyrus"],
        (Platform::Mac, "emoji") => &["Apple Color Emoji"],
        (Platform::Mac, "math") => &["STIX Two Math"],
        (Platform::Linux, "serif" | "ui-serif") => &["serif"],
        (
            Platform::Linux,
            "sans-serif" | "ui-sans-serif" | "system-ui" | "-apple-system" | "blinkmacsystemfont"
            | "ui-rounded" | "-webkit-standard",
        ) => &["sans-serif"],
        (Platform::Linux, "monospace" | "ui-monospace") => &["monospace"],
        (Platform::Linux, "cursive") => &["cursive"],
        (Platform::Linux, "fantasy") => &["fantasy"],
        (Platform::Linux, "emoji") => &["Noto Color Emoji"],
        (Platform::Linux, "math") => &["serif"],
        _ => return None,
    })
}

/// The fonts tried for a character the listed families lack, in order: the East Asian font the
/// document's language prefers first, then the others, then the emoji and symbol fonts.
pub fn fallback_chain(platform: Platform, lang: Option<&str>) -> Vec<&'static str> {
    let region = Region::of(lang);
    let (first, rest, tail): (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
    ) = match platform {
        Platform::Windows => (
            match region {
                Region::Traditional => "Microsoft JhengHei UI",
                Region::Japanese => "Yu Gothic UI",
                Region::Korean => "Malgun Gothic",
                Region::Simplified => "Microsoft YaHei UI",
            },
            &[
                "Microsoft YaHei UI",
                "Yu Gothic UI",
                "Malgun Gothic",
                "Microsoft JhengHei UI",
            ],
            &["Segoe UI Emoji", "Segoe UI Symbol"],
        ),
        Platform::Mac => (
            match region {
                Region::Traditional => "PingFang TC",
                Region::Japanese => "Hiragino Sans",
                Region::Korean => "Apple SD Gothic Neo",
                Region::Simplified => "PingFang SC",
            },
            &["PingFang SC", "Hiragino Sans", "Apple SD Gothic Neo"],
            &["Apple Color Emoji"],
        ),
        Platform::Linux => (
            match region {
                Region::Traditional => "Noto Sans CJK TC",
                Region::Japanese => "Noto Sans CJK JP",
                Region::Korean => "Noto Sans CJK KR",
                Region::Simplified => "Noto Sans CJK SC",
            },
            &["Noto Sans CJK SC", "Noto Sans CJK JP", "Noto Sans CJK KR"],
            &["Noto Color Emoji"],
        ),
    };
    let mut chain = vec![first];
    for name in rest.iter().chain(tail) {
        if !chain.contains(name) {
            chain.push(name);
        }
    }
    chain
}

/// Which East Asian font a `lang` asks for first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Region {
    Simplified,
    Traditional,
    Japanese,
    Korean,
}

impl Region {
    fn of(lang: Option<&str>) -> Self {
        let lang = lang.unwrap_or("").to_ascii_lowercase();
        let mut parts = lang.split(['-', '_']);
        match parts.next() {
            Some("ja") => Region::Japanese,
            Some("ko") => Region::Korean,
            Some("zh") => {
                if parts.any(|part| matches!(part, "tw" | "hk" | "mo" | "hant")) {
                    Region::Traditional
                } else {
                    Region::Simplified
                }
            }
            _ => Region::Simplified,
        }
    }
}

/// The names to resolve for a `font-family` list: each declared family (a registered face as it
/// is, a generic family as the platform's names, any other as written), then the fallback chain.
/// A list that names nothing falls to the platform's `serif`, the standard font.
pub fn stack(
    platform: Platform,
    declared: &[String],
    lang: Option<&str>,
    registry: &FontRegistry,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name.to_owned());
        }
    };
    for family in declared {
        if registry.contains(family) {
            push(family);
        } else if let Some(mapped) = generic(platform, family) {
            mapped.iter().for_each(|name| push(name));
        } else {
            push(family);
        }
    }
    if declared.is_empty() {
        generic(platform, "serif")
            .into_iter()
            .flatten()
            .for_each(|name| push(name));
    }
    for name in fallback_chain(platform, lang) {
        push(name);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn families(text: &str) -> Vec<String> {
        split_families(text)
    }

    #[test]
    fn a_family_list_splits_at_commas_outside_quotes() {
        assert_eq!(
            families(r#""Segoe UI", 'Noto, Sans', monospace"#),
            vec!["Segoe UI", "Noto, Sans", "monospace"]
        );
        assert!(families("").is_empty());
    }

    #[test]
    fn generic_families_become_the_platforms_names() {
        let registry = FontRegistry::default();
        let windows = stack(
            Platform::Windows,
            &families("system-ui, monospace"),
            None,
            &registry,
        );
        assert_eq!(&windows[..2], ["Segoe UI", "Consolas"]);
        let mac = stack(Platform::Mac, &families("monospace"), None, &registry);
        assert_eq!(mac[0], "Menlo");
        let linux = stack(Platform::Linux, &families("sans-serif"), None, &registry);
        assert_eq!(linux[0], "sans-serif");
    }

    #[test]
    fn a_list_with_nothing_declared_is_the_standard_serif() {
        let registry = FontRegistry::default();
        let windows = stack(Platform::Windows, &[], None, &registry);
        assert_eq!(windows[0], "Times New Roman");
    }

    #[test]
    fn the_fallback_chain_follows_the_language() {
        let registry = FontRegistry::default();
        let plain = stack(Platform::Windows, &families("Arial"), None, &registry);
        assert_eq!(plain[0], "Arial");
        assert_eq!(plain[1], "Microsoft YaHei UI");
        assert!(plain.contains(&"Segoe UI Emoji".to_owned()));
        let taiwan = stack(
            Platform::Windows,
            &families("Arial"),
            Some("zh-TW"),
            &registry,
        );
        assert_eq!(taiwan[1], "Microsoft JhengHei UI");
        let japan = fallback_chain(Platform::Windows, Some("ja"));
        assert_eq!(japan[0], "Yu Gothic UI");
        assert_eq!(
            fallback_chain(Platform::Mac, Some("zh-Hant"))[0],
            "PingFang TC"
        );
        assert_eq!(
            fallback_chain(Platform::Linux, Some("ko-KR"))[0],
            "Noto Sans CJK KR"
        );
    }

    #[test]
    fn a_name_is_listed_once() {
        let registry = FontRegistry::default();
        let list = stack(
            Platform::Windows,
            &families("Microsoft YaHei UI, sans-serif"),
            None,
            &registry,
        );
        let count = list
            .iter()
            .filter(|name| name.as_str() == "Microsoft YaHei UI")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn a_registered_face_wins_over_a_generic_of_the_same_name() {
        let mut registry = FontRegistry::default();
        assert!(registry.register("Serif", 400., false));
        assert!(!registry.register("serif", 400., false));
        assert_eq!(registry.generation(), 1);
        let list = stack(Platform::Windows, &families("serif"), None, &registry);
        assert_eq!(list[0], "serif");
        assert!(registry.contains("SERIF"));
    }

    #[test]
    fn the_best_registered_face_matches_slant_then_weight() {
        let mut registry = FontRegistry::default();
        registry.register("Brand", 400., false);
        registry.register("Brand", 700., false);
        registry.register("Brand", 400., true);
        assert_eq!(registry.best("brand", 700., false), Some((700., false)));
        assert_eq!(registry.best("brand", 600., false), Some((700., false)));
        assert_eq!(registry.best("brand", 300., false), Some((400., false)));
        assert_eq!(registry.best("brand", 700., true), Some((400., true)));
        assert_eq!(registry.best("other", 400., false), None);
    }

    #[test]
    fn a_weight_from_400_to_500_tries_heavier_faces_up_to_500_first() {
        let mut registry = FontRegistry::default();
        registry.register("Brand", 300., false);
        registry.register("Brand", 500., false);
        registry.register("Brand", 700., false);
        assert_eq!(registry.best("brand", 400., false), Some((500., false)));
        assert_eq!(registry.best("brand", 450., false), Some((500., false)));
        assert_eq!(registry.best("brand", 500., false), Some((500., false)));
        let mut sparse = FontRegistry::default();
        sparse.register("Brand", 300., false);
        sparse.register("Brand", 700., false);
        assert_eq!(sparse.best("brand", 400., false), Some((300., false)));
    }
}
