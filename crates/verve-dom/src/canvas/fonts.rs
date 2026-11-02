//! Canvas text's fonts: the system's, discovered on first use, and the page's `@font-face`s.
//!
//! `fillText` and `measureText` are synchronous and answer in pixels before they return, so they
//! cannot wait on a text system that lives on the drawing thread. This is the renderer's own
//! stack, kept behind [`Measurer`](crate::canvas::text::Measurer) so P30 can swap the advances for the
//! layout engine's without touching the drawing commands.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use fontdb::{Database, Family, ID, Query, Stretch, Style, Weight};

/// Generic family → the concrete families it names on this platform. A system with none of them
/// falls through to whatever the font database answers, so the last entry is the assumed default.
#[cfg(windows)]
pub fn generic_families(generic: &str) -> &'static [&'static str] {
    match generic {
        "serif" | "ui-serif" => &["Times New Roman", "Georgia", "Cambria"],
        "sans-serif" | "system-ui" | "ui-sans-serif" => &["Segoe UI", "Arial", "Tahoma"],
        "monospace" | "ui-monospace" => &["Consolas", "Courier New", "Lucida Console"],
        "cursive" => &["Comic Sans MS", "Segoe Script"],
        "fantasy" => &["Impact", "Gabriola"],
        _ => &[],
    }
}

#[cfg(target_os = "macos")]
pub fn generic_families(generic: &str) -> &'static [&'static str] {
    match generic {
        "serif" | "ui-serif" => &["Times New Roman", "Georgia", "Times"],
        "sans-serif" | "system-ui" | "ui-sans-serif" => &["Helvetica", "Helvetica Neue", "Arial"],
        "monospace" | "ui-monospace" => &["Menlo", "Monaco", "Courier New"],
        "cursive" => &["Apple Chancery"],
        "fantasy" => &["Papyrus"],
        _ => &[],
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn generic_families(generic: &str) -> &'static [&'static str] {
    match generic {
        "serif" | "ui-serif" => &["DejaVu Serif", "Liberation Serif", "Times New Roman"],
        "sans-serif" | "system-ui" | "ui-sans-serif" => {
            &["DejaVu Sans", "Liberation Sans", "Noto Sans", "Arial"]
        }
        "monospace" | "ui-monospace" => &["DejaVu Sans Mono", "Liberation Mono", "Courier New"],
        "cursive" => &["DejaVu Serif"],
        "fantasy" => &["DejaVu Serif"],
        _ => &[],
    }
}

/// A `@font-face` rule's descriptors: which of a family's faces a weight and style pick. P22
/// fills these from the cascade.
#[derive(Clone, Default)]
pub struct FontFaceRule {
    pub weight: u16,
    pub italic: bool,
}

/// The system's fonts, scanned once for the whole process, and the faces the page added.
pub struct FontStack {
    db: Arc<Database>,
    web: HashMap<String, Vec<WebFace>>,
}

struct WebFace {
    id: ID,
    weight: u16,
    italic: bool,
}

/// Scanned at most once per process: a renderer per window would otherwise pay for it each time.
static SYSTEM: OnceLock<Arc<Database>> = OnceLock::new();

/// The bytes of a web font, kept as they came so `fontdb` may hand them out unchanged.
struct SharedBytes(Arc<[u8]>);

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl FontStack {
    /// A stack that has discovered nothing yet: the system's fonts are read when text is first
    /// measured or drawn, not when a document is made.
    pub fn system() -> Self {
        Self {
            db: Arc::new(Database::new()),
            web: HashMap::new(),
        }
    }

    /// Finds the system's fonts if they have not been found.
    pub(crate) fn warm(&mut self) {
        if self.db.is_empty() {
            let system = SYSTEM.get_or_init(|| {
                let mut db = Database::new();
                db.load_system_fonts();
                Arc::new(db)
            });
            self.db = Arc::clone(system);
        }
    }

    /// Adds an `@font-face`'s bytes under the family the rule names — the font's own name may
    /// differ from the one the page declared.
    pub fn add_web_font(&mut self, family: &str, descriptors: &FontFaceRule, bytes: Arc<[u8]>) {
        self.warm();
        // The system database is shared by every stack: it is copied the first time a stack adds
        // a face of its own, and not again.
        let source = fontdb::Source::Binary(Arc::new(SharedBytes(bytes)));
        let ids = Arc::make_mut(&mut self.db).load_font_source(source);
        for id in ids {
            self.web
                .entry(family.to_owned())
                .or_default()
                .push(WebFace {
                    id,
                    weight: descriptors.weight,
                    italic: descriptors.italic,
                });
        }
    }

    /// The face that draws `ch` for a family list, the way CSS picks one: the first family that
    /// has the character, a generic family's concrete names, then the default.
    pub fn resolve(&self, families: &[&str], weight: u16, italic: bool, ch: char) -> Option<ID> {
        for family in families {
            if let Some(faces) = self.web.get(*family) {
                let picked = pick_web(faces, weight, italic);
                if picked.is_some_and(|id| self.covers(id, ch)) {
                    return picked;
                }
            }
            for name in std::iter::once(*family).chain(generic_families(family).iter().copied()) {
                let found = self.matching(Family::Name(name), weight, italic);
                if let Some(id) = found.filter(|id| self.covers(*id, ch)) {
                    return Some(id);
                }
            }
        }
        self.matching(Family::SansSerif, weight, italic)
            .or_else(|| self.first_face())
    }

    /// The face of `family` nearest the weight and style asked for.
    fn matching(&self, family: Family<'_>, weight: u16, italic: bool) -> Option<ID> {
        self.db.query(&Query {
            families: &[family],
            weight: Weight(weight),
            stretch: Stretch::Normal,
            style: if italic { Style::Italic } else { Style::Normal },
        })
    }

    /// Whether the face has a glyph for `ch`: the whole of per-character fallback.
    pub(crate) fn covers(&self, id: ID, ch: char) -> bool {
        self.db
            .with_face_data(id, |data, index| {
                rustybuzz::ttf_parser::Face::parse(data, index)
                    .ok()
                    .and_then(|face| face.glyph_index(ch))
                    .is_some()
            })
            .unwrap_or(false)
    }

    /// The face's data, for shaping: `(bytes, index)`.
    pub(crate) fn with_face<R>(&self, id: ID, f: impl FnOnce(&[u8], u32) -> R) -> Option<R> {
        self.db.with_face_data(id, f)
    }

    fn first_face(&self) -> Option<ID> {
        self.db.faces().next().map(|face| face.id)
    }
}

fn pick_web(faces: &[WebFace], weight: u16, italic: bool) -> Option<ID> {
    faces
        .iter()
        .filter(|face| face.italic == italic)
        .min_by_key(|face| (i32::from(face.weight) - i32::from(weight)).unsigned_abs())
        .or_else(|| faces.first())
        .map(|face| face.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generic_family_names_a_concrete_one() {
        assert!(!generic_families("sans-serif").is_empty());
        assert!(generic_families("no-such-family").is_empty());
    }
}
