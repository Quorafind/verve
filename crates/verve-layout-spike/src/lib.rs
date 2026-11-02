//! P30 spike: shape and measure text through GPUI's text system. Output is a plain string so
//! callers on different threads can compare it byte for byte.

use gpui::{Font, Hsla, Pixels, TextRun, TextSystem, WindowTextSystem, font, px};
use std::fmt::Write;
use std::sync::Arc;

pub const SAMPLES: &[&str] = &[
    "Hello 你好 مرحبا",
    "The quick brown fox jumps over the lazy dog",
    "日本語のテキストと English が混ざった長い行です。",
];

pub fn fonts() -> Vec<Font> {
    vec![
        font("Segoe UI"),
        font("Consolas"),
        font("Segoe UI").bold(),
        font("NoSuchFont"),
    ]
}

fn run(len: usize, font: &Font) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color: Hsla::default(),
        background_color: None,
        background_radius: None,
        underline: None,
        strikethrough: None,
    }
}

/// Metrics through `TextSystem` (the part that is public without an `App`).
pub fn metrics(ts: &TextSystem, size: Pixels) -> String {
    let mut out = String::new();
    for f in fonts() {
        let id = ts.resolve_font(&f);
        let adv = ts
            .advance(id, size, 'x')
            .map(|s| f32::from(s.width))
            .unwrap_or(-1.0);
        let _ = writeln!(
            out,
            "metrics {:<12} w={:?} id={:?} asc={:.3} desc={:.3} xh={:.3} cap={:.3} adv(x)={:.3} upem={}",
            f.family,
            f.weight.0,
            id,
            f32::from(ts.ascent(id, size)),
            f32::from(ts.descent(id, size)),
            f32::from(ts.x_height(id, size)),
            f32::from(ts.cap_height(id, size)),
            adv,
            ts.units_per_em(id)
        );
    }
    out
}

/// Line layout and wrapping through `WindowTextSystem` (a cache around `TextSystem`).
pub fn shaping(ws: &WindowTextSystem, size: Pixels, wrap: Pixels) -> String {
    let mut out = String::new();
    for f in fonts() {
        for text in SAMPLES {
            let runs = [run(text.len(), &f)];
            let l = ws.layout_line(text, size, &runs, None);
            let fonts_used: Vec<_> = l.runs.iter().map(|r| r.font_id.0).collect();
            let _ = writeln!(
                out,
                "line {:<12} w={:<5} {:<24.24} width={:.3} asc={:.3} desc={:.3} runs={:?} glyphs={}",
                f.family,
                f.weight.0,
                text,
                f32::from(l.width),
                f32::from(l.ascent),
                f32::from(l.descent),
                fonts_used,
                l.runs.iter().map(|r| r.glyphs.len()).sum::<usize>()
            );
            let lines = ws
                .shape_text((*text).to_string().into(), size, &runs, Some(wrap), None)
                .unwrap_or_default();
            for wl in &lines {
                // Byte index in the text where each wrap happens.
                let breaks: Vec<usize> = wl
                    .wrap_boundaries
                    .iter()
                    .map(|b| wl.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix].index)
                    .collect();
                let _ = writeln!(
                    out,
                    "  wrap@{} width={:.3} breaks(byte)={:?}",
                    f32::from(wrap),
                    f32::from(wl.unwrapped_layout.width),
                    breaks
                );
            }
        }
    }
    out
}

pub fn report(ts: &Arc<TextSystem>, ws: &WindowTextSystem) -> String {
    let size = px(16.0);
    let mut out = metrics(ts, size);
    out.push_str(&shaping(ws, size, px(120.0)));
    out
}
