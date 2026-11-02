//! Fallback evaluation: shape the same strings with rustybuzz straight from the font file and
//! compare widths/metrics with GPUI's DirectWrite numbers (same font, 16 px, no hinting).
use gpui::{TextSystem, WindowTextSystem, font, px};
use std::sync::Arc;
use verve_layout_spike::SAMPLES;

fn main() {
    let platform = gpui_platform::current_platform(false);
    let ts = Arc::new(TextSystem::new(platform.text_system()));
    let ws = WindowTextSystem::new(ts.clone());
    let size = 16.0f32;
    let files = [
        ("Segoe UI", "C:/Windows/Fonts/segoeui.ttf"),
        ("Consolas", "C:/Windows/Fonts/consola.ttf"),
    ];
    for (family, path) in files {
        let data = std::fs::read(path).expect("font file");
        let face = rustybuzz::Face::from_slice(&data, 0).expect("face");
        let upem = face.units_per_em() as f32;
        let scale = size / upem;
        let id = ts.resolve_font(&font(family));
        println!(
            "{family}: ours asc={:.3} desc={:.3} | gpui asc={:.3} desc={:.3}",
            face.ascender() as f32 * scale,
            -(face.descender() as f32) * scale,
            f32::from(ts.ascent(id, px(size))),
            -f32::from(ts.descent(id, px(size)))
        );
        for text in SAMPLES.iter().take(2) {
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(text);
            let out = rustybuzz::shape(&face, &[], buf);
            let ours: f32 = out
                .glyph_positions()
                .iter()
                .map(|p| p.x_advance as f32)
                .sum::<f32>()
                * scale;
            let run = gpui::TextRun {
                len: text.len(),
                font: font(family),
                color: Default::default(),
                background_color: None,
                background_radius: None,
                underline: None,
                strikethrough: None,
            };
            let l = ws.layout_line(text, px(size), &[run], None);
            println!(
                "  {:<30.30} ours={:.3} gpui={:.3}",
                text,
                ours,
                f32::from(l.width)
            );
        }
    }
}
