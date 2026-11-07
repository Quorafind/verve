//! Text with the real fonts: GPUI's text system under the layout, with no window. The system's
//! fonts are Windows': `Segoe UI`, `Arial`, `Consolas`, `Microsoft YaHei UI` and the rest, so the
//! tests are Windows'.
//!
//! The numbers are the ones Electron 39.8.10 (Chromium on Windows, DPR 1, a hidden window with
//! `--force-device-scale-factor=1`) gave for the same pages, probed on the machine this was written
//! on (zh-CN, Windows 10): `line-height: normal` of a line of `Hxg` is 17, 21 and 27 in Segoe UI at
//! 13, 16 and 20px; 15, 18 and 23 in Arial; 15, 19 and 23 in Consolas; 16, 18 and 23 in Times New
//! Roman; 15, 18 and 23 in Courier New; the baseline of Segoe UI at 16px is 17 below the top of its
//! line, of Consolas 15 and of Times New Roman 14; `Hxg` is 28.125 wide in Segoe UI at 16px, 28.453
//! in Arial, 26.391 in Consolas and 27.563 in Times New Roman; a line of Segoe UI that has CJK
//! or an emoji in it is as high as one without (21). `<sub>` and `<sup>` in a Segoe UI 16px line:
//! the `<sub>` box (13.33px, 17 high) at 10.516 from the top of the line and the `<sup>` at 0, the
//! line 27.516 high.
#![cfg(windows)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Platform, TextSystem};
use verve::Runtime;
use verve_dom::{Document, NodeId, Options};
use verve_gpui::GpuiMeasurer;
use verve_layout::{LayoutEngine, TextMeasurer, fonts};

mod support;
use support::on_thread;

/// A measurer over a text system of its own (a window's is the app's; the ids are the same kind).
fn measurer() -> (Arc<GpuiMeasurer>, Rc<dyn Platform>) {
    let platform: Rc<dyn Platform> = gpui_platform::current_platform(false);
    let system = Arc::new(TextSystem::new(platform.text_system()));
    (Arc::new(GpuiMeasurer::new(system)), platform)
}

fn stack(declared: &[&str]) -> Vec<String> {
    let declared: Vec<String> = declared.iter().map(|name| (*name).to_owned()).collect();
    fonts::stack(
        fonts::Platform::Windows,
        &declared,
        None,
        &fonts::FontRegistry::default(),
    )
}

/// A page laid out with `measurer`.
struct Page {
    runtime: Runtime,
    document: Document,
    engine: LayoutEngine,
}

fn js_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn lay_out(measurer: Arc<dyn TextMeasurer>, html: &str) -> (Page, verve_dom::Snapshot) {
    let mut runtime = Runtime::new();
    let document = verve_dom::install(
        &mut runtime,
        Options {
            viewport: (800., 600.),
            ..Options::default()
        },
    )
    .expect("the document installs");
    let source = format!(
        "document.body.style.margin = '0'; document.body.innerHTML = {};",
        js_literal(html)
    );
    runtime.eval(&source).expect("the page's html is set");
    let update = document.update();
    let mut engine = LayoutEngine::new(measurer);
    engine.update(&update.snapshot, &update.changes);
    let snapshot = update.snapshot;
    (
        Page {
            runtime,
            document,
            engine,
        },
        snapshot,
    )
}

fn node(snapshot: &verve_dom::Snapshot, id: &str) -> NodeId {
    let dom = &snapshot.dom;
    dom.descendants(dom.document)
        .into_iter()
        .find(|node| dom.get(*node).and_then(|n| n.attr("id")) == Some(id))
        .unwrap_or_else(|| panic!("no element with id {id}"))
}

#[test]
fn mixed_chinese_english_and_emoji_shape_to_real_glyphs() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        let text = "中文English混排😀";
        let families = stack(&["Segoe UI"]);
        let font = measurer.resolve(&families, 400., false);
        let run = measurer.shape(text, font, 16.);
        let chars = text.chars().count();
        assert_eq!(run.glyphs.len(), chars, "a glyph for each character");
        // No glyph is the font's missing-glyph box (id 0), and each one takes room.
        assert!(
            run.glyphs.iter().all(|glyph| glyph.id != 0),
            "missing glyphs in {:?}",
            run.glyphs
        );
        let mut ends: Vec<f32> = run.glyphs.iter().skip(1).map(|glyph| glyph.x).collect();
        ends.push(run.width);
        for (glyph, end) in run.glyphs.iter().zip(ends) {
            assert!(
                end > glyph.x,
                "glyph {glyph:?} has no width (ends at {end})"
            );
        }
        assert!(run.glyphs.last().is_some_and(|glyph| glyph.is_emoji));
        assert!(run.glyphs[..chars - 1].iter().all(|glyph| !glyph.is_emoji));

        // The Chinese is drawn in Microsoft YaHei UI: shaped in that font alone it comes to the
        // same glyphs and widths, and not in the font the page asked for.
        let yahei = measurer.resolve(&["Microsoft YaHei UI".to_owned()], 400., false);
        let alone = measurer.shape("中文", yahei, 16.);
        let [first, second, ..] = &run.glyphs[..] else {
            panic!("glyphs");
        };
        assert_ne!(first.font, font, "the requested font has no 中");
        assert_eq!(
            (first.id, second.id),
            (alone.glyphs[0].id, alone.glyphs[1].id)
        );
        assert!((second.x - first.x - (alone.glyphs[1].x - alone.glyphs[0].x)).abs() < 0.01);
        let latin = &run.glyphs[2];
        assert_eq!(latin.font, font, "the Latin stays in Segoe UI");
    });
}

#[test]
fn line_heights_of_normal_are_the_ones_chromium_gives() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        // (family, size, Chromium's `line-height: normal`)
        let frozen = [
            ("Segoe UI", [(13., 17.), (16., 21.), (20., 27.)]),
            ("Arial", [(13., 15.), (16., 18.), (20., 23.)]),
            ("Consolas", [(13., 15.), (16., 19.), (20., 23.)]),
            ("Times New Roman", [(13., 16.), (16., 18.), (20., 23.)]),
            ("Courier New", [(13., 15.), (16., 18.), (20., 23.)]),
        ];
        for (family, sizes) in frozen {
            let font = measurer.resolve(&[family.to_owned()], 400., false);
            for (size, line) in sizes {
                let m = measurer.metrics(font, size);
                assert_eq!(
                    m.ascent + m.descent + m.line_gap,
                    line,
                    "{family} at {size}px: {m:?}"
                );
            }
        }
        // The baseline of a line is the font's ascent below its top.
        for (family, baseline) in [
            ("Segoe UI", 17.),
            ("Consolas", 15.),
            ("Times New Roman", 14.),
        ] {
            let font = measurer.resolve(&[family.to_owned()], 400., false);
            assert_eq!(measurer.metrics(font, 16.).ascent, baseline, "{family}");
        }
    });
}

#[test]
fn text_is_as_wide_as_chromium_measures_it() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        for (family, width) in [
            ("Segoe UI", 28.125),
            ("Arial", 28.453_125),
            ("Consolas", 26.390_625),
            ("Times New Roman", 27.562_5),
        ] {
            let font = measurer.resolve(&[family.to_owned()], 400., false);
            let run = measurer.shape("Hxg", font, 16.);
            assert!(
                (run.width - width).abs() < 0.1,
                "{family}: {} against Chromium's {width}",
                run.width
            );
        }
    });
}

#[test]
fn a_block_of_real_text_is_as_high_as_chromium_makes_it() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        let html = r#"<div id="a" style="font:16px 'Segoe UI'">Hxg</div>
            <div id="b" style="font:16px Arial">Hxg</div>
            <div id="c" style="font:16px 'Segoe UI'">Hello 中文</div>
            <div id="d" style="font:16px 'Segoe UI'">Hello 😀</div>
            <div id="e" style="font:16px 'Segoe UI';width:40px">Hxg Hxg Hxg</div>"#;
        let (page, snapshot) = lay_out(measurer, html);
        let height = |id: &str| {
            page.engine
                .rect(node(&snapshot, id))
                .map(|rect| rect.height)
        };
        assert_eq!(height("a"), Some(21.));
        assert_eq!(height("b"), Some(18.));
        assert_eq!(
            height("c"),
            Some(21.),
            "CJK in the line does not make it taller"
        );
        assert_eq!(height("d"), Some(21.), "an emoji does not either");
        assert_eq!(height("e"), Some(63.), "one word to a line, 21 each");
    });
}

#[test]
fn sub_and_super_sit_where_chromium_puts_them() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        let html = r#"<div id="d" style="font:16px 'Segoe UI'">x<sub id="sub">s</sub><sup id="sup">s</sup></div>"#;
        let (page, snapshot) = lay_out(measurer, html);
        let rect = |id: &str| page.engine.client_rects(node(&snapshot, id))[0];
        let (sub, sup, block) = (rect("sub"), rect("sup"), rect("d"));
        assert!((sub.y - 10.5156).abs() < 0.05, "sub at {}", sub.y);
        assert!(sup.y.abs() < 0.05, "sup at {}", sup.y);
        assert_eq!((sub.height, sup.height), (17., 17.));
        assert!(
            (block.height - 27.5156).abs() < 0.05,
            "line {}",
            block.height
        );
    });
}

#[test]
fn a_registered_face_is_the_font_its_family_names() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        let bytes = std::fs::read(r"C:\Windows\Fonts\consola.ttf").expect("Consolas is installed");
        let html = r#"<div id="d" style="font:16px Brand, Arial"><span id="s">Hxg</span></div>"#;
        let (mut page, snapshot) = lay_out(Arc::clone(&measurer) as Arc<dyn TextMeasurer>, html);
        let width = |page: &Page| page.engine.client_rects(node(&snapshot, "s"))[0].width;
        // Brand is not installed: the text is in Arial until the face arrives.
        assert!((width(&page) - 28.453).abs() < 0.1, "{}", width(&page));
        // The face's own family (Consolas) is installed, and the alias is not its name.
        assert!(measurer.add_face("Brand", 400., false, bytes.clone().into()));
        assert!(page.engine.register_font("Brand", 400., false));
        let update = page.document.update();
        page.engine.update(&update.snapshot, &update.changes);
        assert!((width(&page) - 26.391).abs() < 0.1, "{}", width(&page));
        // The same bytes again (a reload) and a bold face under the same alias both work, and
        // the bold one is another font.
        assert!(measurer.add_face("Brand", 400., false, bytes.into()));
        let bold = std::fs::read(r"C:\Windows\Fonts\consolab.ttf").expect("Consolas Bold");
        assert!(measurer.add_face("Brand", 700., false, bold.into()));
        let brand = ["Brand".to_owned(), "Arial".to_owned()];
        let (regular, heavy) = (
            measurer.resolve(&brand, 400., false),
            measurer.resolve(&brand, 700., false),
        );
        assert_ne!(regular, heavy);
        assert!((measurer.shape("Hxg", heavy, 16.).width - 26.391).abs() < 0.1);
        assert!((measurer.shape("Hxg", regular, 16.).width - 26.391).abs() < 0.1);
        // Bytes that are not a font are refused.
        assert!(!measurer.add_face("Junk", 400., false, vec![0u8; 16].into()));
        let _ = page.runtime;
    });
}

#[test]
fn a_family_is_installed_by_any_name_it_has_and_a_face_is_the_page_s_own() {
    on_thread(|| {
        let (measurer, _platform) = measurer();
        // These names are not the ones a zh-CN system lists its families by.
        let missing = measurer.resolve(&["No Such Family Anywhere".into()], 400., false);
        for name in ["Microsoft YaHei", "SimSun", "SimHei"] {
            let font = measurer.resolve(&[name.to_owned()], 400., false);
            assert_ne!(font, missing, "{name} is dropped as not installed");
        }
        // A face a page registers is not another page's.
        let bytes = std::fs::read(r"C:\Windows\Fonts\consola.ttf").expect("Consolas is installed");
        let first: Arc<dyn TextMeasurer> = Arc::clone(&measurer).for_page();
        let second: Arc<dyn TextMeasurer> = Arc::clone(&measurer).for_page();
        assert!(first.add_face("Brand", 400., false, bytes.into()));
        let brand = ["Brand".to_owned()];
        assert_ne!(first.resolve(&brand, 400., false), missing);
        let elsewhere = ["Not Declared Here".to_owned()];
        assert_eq!(
            second.resolve(&brand, 400., false),
            second.resolve(&elsewhere, 400., false)
        );
    });
}
