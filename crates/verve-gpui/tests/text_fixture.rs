//! The `text` fixture (`tests/electron/apps/text`) laid out by Verve with the real fonts, against
//! what Chromium measured of it: `expected-chromium.json`, written by Electron 39.8.10 on Windows
//! with `VERVE_TEXT_JSON=<path> electron tests/electron/apps/text` (a 640 x 900 window, DPR 1). The
//! page's own script (`lines.js`) measures each marked block through `Range.getClientRects()`, so
//! the same measurement runs in both engines: the test wants the same number of lines in each block
//! and every line's top and bottom, and each block's top and height, within 2px. The pixels of the
//! text themselves are not compared (the rasterisers differ).
//!
//! The first test needs no window. The others run the `electron` binary on a real window and are
//! `#[ignore]`d for that. `--screenshot` draws the display list with the software painter, which has
//! no fonts, so no test compares the glyphs a window shows.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Platform, TextSystem};
use verve_dom::{Options, html};
use verve_gpui::GpuiMeasurer;
use verve_layout::LayoutEngine;

mod support;
use support::{frame_p95s, on_thread, run};

const PAGE: &str = include_str!("../../../tests/electron/apps/text/index.html");
const LINES: &str = include_str!("../../../tests/electron/apps/text/lines.js");
const EXPECTED: &str = include_str!("../../../tests/electron/apps/text/expected-chromium.json");

/// How far a line or a block may be from where Chromium put it, in CSS pixels.
const TOLERANCE: f32 = 2.;

/// The script that compares what `lines.js` measures here with Chromium's, and lists the
/// differences.
fn compare() -> String {
    format!(
        r#"(function (want) {{
            const got = window.__lines();
            const bad = [];
            const near = (a, b) => Math.abs(a - b) <= {TOLERANCE};
            for (const w of want) {{
                const g = got.find((x) => x.id === w.id);
                if (!g) {{ bad.push(w.id + ': not measured'); continue; }}
                if (g.lines.length !== w.lines.length) {{
                    bad.push(w.id + ': ' + g.lines.length + ' lines, Chromium has ' + w.lines.length);
                }} else {{
                    w.lines.forEach((line, i) => {{
                        const mine = g.lines[i];
                        if (!near(mine[0], line[0]) || !near(mine[1], line[1])) {{
                            bad.push(w.id + ' line ' + i + ': ' + mine + ', Chromium ' + line);
                        }}
                    }});
                }}
                if (!near(g.box[1], w.box[1]) || !near(g.box[3], w.box[3])) {{
                    bad.push(w.id + ': box y/height ' + g.box[1] + '/' + g.box[3] + ', Chromium ' + w.box[1] + '/' + w.box[3]);
                }}
            }}
            return bad.join('\n');
        }})({EXPECTED})"#
    )
}

#[test]
fn the_text_page_has_chromiums_lines_where_chromium_has_them() {
    let differences = on_thread(|| {
        let platform: Rc<dyn Platform> = gpui_platform::current_platform(false);
        let system = Arc::new(TextSystem::new(platform.text_system()));
        let mut runtime = verve::Runtime::new();
        let document = verve_dom::install(
            &mut runtime,
            Options {
                viewport: (640., 900.),
                ..Options::default()
            },
        )
        .expect("a document");
        let (root, body) = (document.document_node(), document.body());
        verve_dom::host::with_dom(runtime.context(), |dom| {
            dom.empty(body);
            html::parse_document(dom, root, PAGE);
        });
        let mut engine = LayoutEngine::new(Arc::new(GpuiMeasurer::new(system)));
        let update = document.update();
        engine.update(&update.snapshot, &update.changes);
        // The page's geometry comes from the engine, as it does in a window.
        let engine = std::cell::RefCell::new(engine);
        let engine = Rc::new(engine);
        let provider = Rc::clone(&engine);
        document.set_layout_provider(Some(Box::new(move |update| {
            let mut engine = provider.borrow_mut();
            engine.update(&update.snapshot, &update.changes);
            verve_dom::Layout {
                rects: engine.rects().collect(),
                fragments: engine.fragments().collect(),
                svg_boxes: engine.svg_boxes().collect(),
                ..verve_dom::Layout::default()
            }
        })));
        let ranges = Rc::clone(&engine);
        document.set_range_provider(Some(Box::new(move |start, end| {
            ranges.borrow().range_rects(start, end)
        })));
        runtime.eval(LINES).expect("lines.js runs");
        let result = runtime.eval(&compare()).expect("the comparison runs");
        verve::print(&result)
    });
    assert!(
        differences.is_empty(),
        "Verve and Chromium differ:\n{differences}"
    );
}

#[test]
#[ignore = "opens real windows"]
fn the_electron_binary_draws_the_text_page_and_takes_its_picture() {
    let picture = std::env::temp_dir().join(format!("verve-gpui-text-{}.png", std::process::id()));
    let shot = picture.to_string_lossy().into_owned();
    let output = run("text", &["--screenshot", &shot], &[], 90);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&picture).expect("the picture was written");
    let _ = std::fs::remove_file(&picture);
    let got = tiny_skia::Pixmap::decode_png(&bytes).expect("a PNG");
    assert_eq!((got.width(), got.height()), (640, 900));
}

/// The 2000-paragraph page (`tests/electron/apps/text-2000`), drawn frame after frame in a real
/// window: the binary logs how long a frame took to build (`VERVE_FRAME_STATS`). Meant for a
/// release build: the 95th percentile has to be under 4 ms.
#[test]
#[ignore = "opens real windows"]
fn two_thousand_paragraphs_are_replayed_within_a_frame() {
    let output = run(
        "text-2000",
        &[],
        &[("VERVE_FRAME_STATS", "1"), ("VERVE_TEXT_MS", "10000")],
        90,
    );
    let log = String::from_utf8_lossy(&output.stderr);
    let stats = frame_p95s(&log);
    eprintln!(
        "{}",
        stats
            .iter()
            .map(|(_, line)| line.as_str())
            .collect::<Vec<_>>()
            .join(
                "
"
            )
    );
    assert!(
        !stats.is_empty(),
        "no frame stats in:
{log}"
    );
    for (built_p95, line) in stats {
        assert!(
            built_p95 < 4.,
            "a frame took {built_p95} ms at the 95th percentile: {line}"
        );
    }
}
