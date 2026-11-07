//! The text measurer of the renderer threads: layout's [`TextMeasurer`] on GPUI's own text system.
//!
//! The app's `Arc<TextSystem>` is shared (`cx.text_system()`), so a `FontId` a renderer thread gets
//! from shaping is the one the paint thread draws with, and a glyph shaped here paints with
//! `Window::paint_glyph` as it is. Nothing in here needs the main thread. Resolving a family list
//! and shaping a run are cached, because layout asks again for the same ones after every change.
//!
//! `WindowTextSystem` keeps a cache of the lines it laid out that its owner clears once a frame
//! (`finish_frame`, which is `pub(crate)` in GPUI). Nobody does that for the one kept here, so it
//! is made anew, when a layout ends, once it has shaped [`SHAPED_BUDGET`] bytes: the runs layout keeps are in the
//! measurer's own cache and are not lost. Once GPUI makes `WindowTextSystem::finish_frame`
//! public, call it from `TextMeasurer::finish_layout` instead of replacing the system.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{
    Font, FontFallbacks, FontId, FontStyle, FontWeight, Hsla, TextRun, TextSystem,
    WindowTextSystem, px,
};
use verve_layout::{FontKey, FontMetrics, PlacedGlyph, ShapedRun, TextMeasurer};

use crate::font_name;

/// How many bytes of text and glyphs the shaping system shapes before it is replaced.
const SHAPED_BUDGET: usize = 4 << 20;
/// How many bytes of text and glyphs the kept runs hold before the cache starts over.
const RUNS_BUDGET: usize = 32 << 20;
/// How many font sizes are kept measured before the cache starts over.
const KEPT_METRICS: usize = 8192;

/// What a font family asks for on top of ascent and descent, in ems, for the fonts that have it
/// (the font's hhea line gap, less what its Windows ascent and descent add beyond hhea's). GPUI
/// reads this as `FontMetrics::line_gap` but gives no way to ask for it: once `TextSystem` shows
/// its font metrics, read it there and drop this table. Electron 39.8.10 (Chromium 142) rounds it
/// into the line's height (Arial at 16px: 14 + 3 + 1 = 18). The rows for Yu Gothic and SimSun
/// were worked out from the fonts' tables and not probed in Electron.
const LINE_GAPS: &[(&str, f32)] = &[
    ("arial", 67. / 2048.),
    ("times new roman", 87. / 2048.),
    ("yu gothic ui", 645. / 2048.),
    ("yu gothic", 645. / 2048.),
    ("simsun", 36. / 256.),
    ("nsimsun", 36. / 256.),
];

/// The measurer layout asks: one per page, over a core of caches the pages share.
///
/// A page's `@font-face` names are its own: they are kept here, and a name the page did not
/// declare is not one it sees. What is shared is the text system and what is derived from it
/// (which names resolve to which font, shaped runs), keyed by names the system itself has. The
/// fonts' bytes are another matter: `TextSystem::add_fonts` is global and cannot be undone, so a
/// face stays in the app's collection once any page has declared it, and shadows an installed
/// family of the same internal name for every page.
pub struct GpuiMeasurer {
    core: Arc<Core>,
    page: Mutex<Page>,
}

/// What the pages share.
struct Core {
    system: Arc<TextSystem>,
    inner: Mutex<Inner>,
}

/// A face the page registered: which GPUI family draws it.
struct Face {
    weight: f32,
    italic: bool,
    family: String,
}

/// What one page declared.
#[derive(Default)]
struct Page {
    /// The faces of `@font-face`/`FontFace`, by the family the page calls them, in lower case.
    faces: HashMap<String, Vec<Face>>,
    /// What this page's family lists resolved to, as of `generation`.
    resolved: HashMap<(Vec<String>, u32, bool), FontKey>,
    generation: u64,
}

struct Inner {
    shaper: WindowTextSystem,
    /// How many bytes `shaper` has shaped since it was made.
    shaped_bytes: usize,
    /// Bumped whenever fonts are added to the system, which can change what a name resolves to.
    generation: u64,
    /// Whether the system has a family of the name (lower case), in whatever language the name
    /// is; forgotten when a font is added.
    installed: HashMap<String, bool>,
    /// The fonts added to the system, by a hash of their bytes: the names they may go by.
    added: HashMap<u64, Vec<String>>,
    /// What lists of names the system was asked for resolved to.
    resolved: HashMap<(Vec<String>, u32, bool), FontKey>,
    fonts: HashMap<FontKey, Font>,
    metrics: HashMap<(FontKey, u32), FontMetrics>,
    /// The shaped runs, by font and size, then by text.
    runs: HashMap<(FontKey, u32), HashMap<String, Arc<ShapedRun>>>,
    run_bytes: usize,
}

/// About what a shaped run costs to keep.
fn weight_of(text: &str, glyphs: usize) -> usize {
    text.len() + glyphs * std::mem::size_of::<PlacedGlyph>()
}

impl Inner {
    /// Whether the system has a family of this name. Its own answer decides: a name it does not
    /// know resolves to its fallback, and a family it knows resolves to a font of that name, in
    /// any language the family has names in ("Microsoft YaHei" and "微软雅黑" alike).
    fn is_installed(&mut self, system: &TextSystem, name: &str) -> bool {
        let lower = name.to_lowercase();
        if let Some(known) = self.installed.get(&lower) {
            return *known;
        }
        let missing = system.resolve_font(&gpui::font("\u{1}verve-missing-family"));
        let id = system.resolve_font(&gpui::font(name.to_owned()));
        let known = id != missing
            || system
                .get_font_for_id(id)
                .is_some_and(|font| font.family.eq_ignore_ascii_case(name));
        self.installed.insert(lower, known);
        known
    }
}

impl GpuiMeasurer {
    /// A measurer over `system`, which has to be the app's own (`cx.text_system().clone()`): font
    /// ids are those of one instance.
    pub fn new(system: Arc<TextSystem>) -> Self {
        let shaper = WindowTextSystem::new(Arc::clone(&system));
        let core = Core {
            system,
            inner: Mutex::new(Inner {
                shaper,
                shaped_bytes: 0,
                generation: 0,
                installed: HashMap::new(),
                added: HashMap::new(),
                resolved: HashMap::new(),
                fonts: HashMap::new(),
                metrics: HashMap::new(),
                runs: HashMap::new(),
                run_bytes: 0,
            }),
        };
        Self::over(Arc::new(core))
    }

    fn over(core: Arc<Core>) -> Self {
        Self {
            core,
            page: Mutex::new(Page::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.core
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The page's own state. Always taken before [`Self::lock`].
    fn page(&self) -> MutexGuard<'_, Page> {
        self.page
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The GPUI font a key stands for.
    pub fn font_id(key: FontKey) -> FontId {
        FontId(key.0 as usize)
    }

    /// The names GPUI should try for the families a page listed: a registered face under the name
    /// of its own, an installed family as it is, and what is neither left out.
    fn names(
        &self,
        page: &Page,
        inner: &mut Inner,
        families: &[String],
        weight: f32,
        italic: bool,
    ) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for family in families {
            let lower = family.to_lowercase();
            let registered = page
                .faces
                .get(&lower)
                .and_then(|faces| best_face(faces, weight, italic));
            let chosen = match registered {
                Some(name) => Some(name),
                None if lower.starts_with('.') || inner.is_installed(&self.core.system, family) => {
                    Some(family.clone())
                }
                None => None,
            };
            if let Some(name) = chosen
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        names
    }

    /// Registers the bytes of a face the page declared, as the page's `family`. The face's own
    /// family name is what GPUI knows it by, read from the font's name table; the first of its
    /// names the system resolves is the one taken. Bytes the system already has are not added
    /// again.
    fn register(&self, family: &str, weight: f32, italic: bool, bytes: &[u8]) -> bool {
        let mut page = self.page();
        let mut inner = self.lock();
        let system = &self.core.system;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if let std::collections::hash_map::Entry::Vacant(slot) = inner.added.entry(hash) {
            let names = font_name::family_names(bytes);
            if names.is_empty() || system.add_fonts(vec![Cow::Owned(bytes.to_vec())]).is_err() {
                return false;
            }
            slot.insert(names);
            // Names that did not resolve may now, and what resolved to a fallback may not.
            inner.generation += 1;
            inner.installed.clear();
            inner.resolved.clear();
            inner.metrics.clear();
        }
        let candidates = inner.added.get(&hash).cloned().unwrap_or_default();
        let Some(own) = candidates
            .into_iter()
            .find(|name| inner.is_installed(system, name))
        else {
            // The face stays as it was: a working face is not replaced by a name nothing has.
            return false;
        };
        let entry = page.faces.entry(family.to_lowercase()).or_default();
        entry.retain(|face| face.weight != weight || face.italic != italic);
        entry.push(Face {
            weight,
            italic,
            family: own,
        });
        page.resolved.clear();
        true
    }
}

/// The face of a family that a weight and a slant ask for: the nearest weight of the right slant.
fn best_face(faces: &[Face], weight: f32, italic: bool) -> Option<String> {
    faces
        .iter()
        .min_by(|a, b| {
            let cost = |face: &Face| {
                let slant = if face.italic == italic { 0. } else { 1000. };
                slant + (face.weight - weight).abs()
            };
            cost(a)
                .partial_cmp(&cost(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|face| face.family.clone())
}

fn line_gap_em(family: &str) -> f32 {
    let lower = family.to_lowercase();
    LINE_GAPS
        .iter()
        .find(|(name, _)| *name == lower)
        .map_or(0., |(_, gap)| *gap)
}

/// The font a list of names, a weight and a slant ask GPUI for: the first name, the rest as its
/// fallbacks.
fn font_for(names: &[String], weight: f32, italic: bool) -> Font {
    let mut font = match names.first() {
        Some(first) => gpui::font(first.clone()),
        None => Font::default(),
    };
    if names.len() > 1 {
        font.fallbacks = Some(FontFallbacks::from_fonts(names[1..].to_vec()));
    }
    font.weight = FontWeight(weight);
    font.style = if italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    font
}

impl TextMeasurer for GpuiMeasurer {
    fn for_page(self: Arc<Self>) -> Arc<dyn TextMeasurer> {
        Arc::new(Self::over(Arc::clone(&self.core)))
    }

    fn resolve(&self, families: &[String], weight: f32, italic: bool) -> FontKey {
        let mut page = self.page();
        let mut inner = self.lock();
        if page.generation != inner.generation {
            page.resolved.clear();
            page.generation = inner.generation;
        }
        let wanted = (families.to_vec(), weight.to_bits(), italic);
        if let Some(key) = page.resolved.get(&wanted) {
            return *key;
        }
        let names = self.names(&page, &mut inner, families, weight, italic);
        let by_names = (names, weight.to_bits(), italic);
        let key = match inner.resolved.get(&by_names) {
            Some(key) => *key,
            None => {
                let font = font_for(&by_names.0, weight, italic);
                let key = FontKey(self.core.system.resolve_font(&font).0 as u64);
                inner.fonts.entry(key).or_insert(font);
                inner.resolved.insert(by_names, key);
                key
            }
        };
        page.resolved.insert(wanted, key);
        key
    }

    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics {
        let system = &self.core.system;
        let mut inner = self.lock();
        let at = (font, size.to_bits());
        if let Some(known) = inner.metrics.get(&at) {
            return *known;
        }
        let id = Self::font_id(font);
        let family = inner
            .fonts
            .get(&font)
            .map(|font| font.family.to_string())
            .unwrap_or_default();
        // Chromium rounds the ascent, the descent and the gap of a font separately.
        let metrics = FontMetrics {
            ascent: f32::from(system.ascent(id, px(size))).round(),
            descent: f32::from(system.descent(id, px(size))).abs().round(),
            line_gap: (line_gap_em(&family) * size).round(),
            x_height: f32::from(system.x_height(id, px(size))),
            cap_height: f32::from(system.cap_height(id, px(size))),
        };
        if inner.metrics.len() >= KEPT_METRICS {
            inner.metrics.clear();
        }
        inner.metrics.insert(at, metrics);
        metrics
    }

    fn shape(&self, text: &str, font: FontKey, size: f32) -> Arc<ShapedRun> {
        let mut inner = self.lock();
        let at = (font, size.to_bits());
        if let Some(run) = inner.runs.get(&at).and_then(|runs| runs.get(text)) {
            return Arc::clone(run);
        }
        let font_ref = inner
            .fonts
            .get(&font)
            .cloned()
            .or_else(|| self.core.system.get_font_for_id(Self::font_id(font)))
            .unwrap_or_default();
        let run = TextRun {
            len: text.len(),
            font: font_ref,
            color: Hsla::default(),
            background_color: None,
            background_radius: None,
            underline: None,
            strikethrough: None,
        };
        let layout = inner
            .shaper
            .layout_line(text, px(size), std::slice::from_ref(&run), None);
        let glyphs: Vec<PlacedGlyph> = layout
            .runs
            .iter()
            .flat_map(|shaped| {
                let drawn = FontKey(shaped.font_id.0 as u64);
                shaped.glyphs.iter().map(move |glyph| PlacedGlyph {
                    id: glyph.id.0,
                    x: f32::from(glyph.position.x),
                    byte: glyph.index as u32,
                    is_emoji: glyph.is_emoji,
                    font: drawn,
                })
            })
            .collect();
        let cost = weight_of(text, glyphs.len());
        let shaped = Arc::new(ShapedRun {
            font,
            size,
            glyphs: glyphs.into(),
            width: f32::from(layout.width),
        });
        inner.shaped_bytes += cost;
        if inner.run_bytes + cost >= RUNS_BUDGET {
            inner.runs.clear();
            inner.run_bytes = 0;
        }
        inner.run_bytes += cost;
        inner
            .runs
            .entry(at)
            .or_default()
            .insert(text.to_owned(), Arc::clone(&shaped));
        shaped
    }

    fn finish_layout(&self) {
        let mut inner = self.lock();
        if inner.shaped_bytes >= SHAPED_BUDGET {
            inner.shaper = WindowTextSystem::new(Arc::clone(&self.core.system));
            inner.shaped_bytes = 0;
        }
    }

    fn add_face(&self, family: &str, weight: f32, italic: bool, bytes: Arc<[u8]>) -> bool {
        self.register(family, weight, italic, &bytes)
    }
}
