//! Canvas 2D and `<img>`: pixels read back from what scripts drew, and the frames the drawer
//! is handed. Chrome's reference images are not committed yet (see the package's status), so
//! these compare against values worked out from the HTML specification.

#![cfg(feature = "canvas")]
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use common::{PNG_1X1, Page, canvas_page, on_page};

const GIF_1X1: &str = "data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==";
const WEBP_1X1: &str = "data:image/webp;base64,UklGRhoAAABXRUJQVlA4TA0AAAAvAAAAEAcQERGIiP4HAA==";

/// A page with a 20 by 10 canvas.
fn page() -> Page {
    canvas_page(20, 10)
}

/// The bytes of a `data:` URL's base64 payload.
fn payload(url: &str) -> Vec<u8> {
    verve::encoding::unbase64(url.split(',').nth(1).unwrap()).unwrap()
}

/// The numbers `px` printed.
fn numbers(printed: &str) -> Vec<i32> {
    printed.split(',').map(|n| n.parse().unwrap()).collect()
}

#[test]
fn fill_rect_paints_what_get_image_data_reads() {
    on_page(|| {
        let mut p = page();
        assert_eq!(p.eval("px(5, 5)"), "0,0,0,0");
        p.eval("g.fillStyle = '#ff8000'; g.fillRect(2, 2, 4, 4)");
        assert_eq!(p.eval("px(3, 3)"), "255,128,0,255");
        assert_eq!(p.eval("px(6, 6)"), "0,0,0,0");
        p.eval("g.clearRect(0, 0, 20, 10)");
        assert_eq!(p.eval("px(3, 3)"), "0,0,0,0");
    });
}

#[test]
fn transforms_state_and_alpha_apply_until_restored() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "g.save(); g.translate(10, 0); g.scale(2, 1); g.globalAlpha = 0.5;
             g.fillStyle = 'rgb(0,0,255)'; g.fillRect(0, 0, 2, 2); g.restore();",
        );
        let covered = numbers(&p.eval("px(11, 1)"));
        // Half-opaque blue, as premultiplied pixels read back un-premultiplied.
        assert_eq!(covered[..2], [0, 0]);
        assert!(
            covered[2] > 250 && (120..136).contains(&covered[3]),
            "{covered:?}"
        );
        assert_eq!(p.eval("px(9, 1)"), "0,0,0,0");
        assert_eq!(p.eval("g.globalAlpha + ',' + g.fillStyle"), "1,#000000");
    });
}

#[test]
fn get_transform_answers_a_full_dom_matrix() {
    on_page(|| {
        let mut p = page();
        p.eval("g.translate(3, 4); g.scale(2, 5)");
        assert_eq!(
            p.eval("const m = g.getTransform(); [m.a, m.b, m.c, m.d, m.e, m.f, m.is2D, m.isIdentity].join()"),
            "2,0,0,5,3,4,true,false"
        );
        p.eval("g.setTransform({ a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 })");
        assert_eq!(
            p.eval("const t = g.getTransform(); [t.a, t.b, t.c, t.d, t.e, t.f].join()"),
            "1,2,3,4,5,6"
        );
        p.eval("g.setTransform(new DOMMatrix([2, 0, 0, 2, 0, 0]))");
        assert_eq!(p.eval("g.getTransform().a"), "2");
        p.eval("g.resetTransform()");
        assert_eq!(p.eval("g.getTransform().isIdentity"), "true");
    });
}

#[test]
fn image_data_round_trips_through_premultiplied_pixels() {
    on_page(|| {
        let mut p = page();
        let out = p.eval(
            "const d = g.createImageData(1, 1); d.data.set([200, 100, 50, 128]);
             g.putImageData(d, 0, 0); px(0, 0)",
        );
        for (got, want) in numbers(&out).iter().zip([200, 100, 50, 128]) {
            assert!((got - want).abs() <= 1, "{out}");
        }
    });
}

#[test]
fn clip_limits_painting_and_paths_answer_hit_tests() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "g.beginPath(); g.rect(0, 0, 10, 10); g.clip();
             g.fillStyle = 'red'; g.fillRect(0, 0, 20, 10);",
        );
        assert_eq!(p.eval("px(5, 5)"), "255,0,0,255");
        assert_eq!(p.eval("px(15, 5)"), "0,0,0,0");
        p.eval("g.beginPath(); g.arc(5, 5, 4, 0, Math.PI * 2)");
        assert_eq!(
            p.eval("g.isPointInPath(5, 5) + ',' + g.isPointInPath(0, 0)"),
            "true,false"
        );
        assert_eq!(
            p.eval("new Path2D('M0 0 H4 V4 H0 Z') instanceof Path2D"),
            "true"
        );
    });
}

#[test]
fn svg_path_data_draws_arcs_on_the_side_the_sweep_flag_names() {
    on_page(|| {
        let mut p = page();
        p.eval("g.fillStyle = 'black'; g.fill(new Path2D('M2 5 A4 4 0 0 1 10 5 Z'))");
        // Sweeping clockwise from the left end to the right end passes over the top.
        assert_eq!(numbers(&p.eval("px(6, 3)"))[3], 255);
        assert_eq!(numbers(&p.eval("px(6, 7)"))[3], 0);
    });
}

#[test]
fn gradients_blend_and_composite_operations_apply() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const l = g.createLinearGradient(0, 0, 20, 0);
             l.addColorStop(0, 'black'); l.addColorStop(1, 'white');
             g.fillStyle = l; g.fillRect(0, 0, 20, 10);",
        );
        assert!(numbers(&p.eval("px(1, 5)"))[0] < 30);
        assert!(numbers(&p.eval("px(18, 5)"))[0] > 220);
        p.eval("g.globalCompositeOperation = 'destination-out'; g.fillRect(0, 0, 10, 10)");
        assert_eq!(p.eval("px(2, 2)"), "0,0,0,0");
    });
}

#[test]
fn a_linear_gradient_is_half_way_between_its_stops_in_the_middle() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const l = g.createLinearGradient(0, 0, 20, 0);
             l.addColorStop(0, '#f00'); l.addColorStop(1, '#00f');
             g.fillStyle = l; g.fillRect(0, 0, 20, 10);",
        );
        let middle = numbers(&p.eval("px(10, 5)"));
        assert!(
            (115..140).contains(&middle[0]) && (115..140).contains(&middle[2]),
            "{middle:?}"
        );
        assert_eq!(middle[3], 255);
    });
}

#[test]
fn a_conic_gradient_sweeps_its_stops_around_its_center() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const l = g.createConicGradient(0, 10, 5);
             l.addColorStop(0, '#f00'); l.addColorStop(0.5, '#f00');
             l.addColorStop(0.5, '#00f'); l.addColorStop(1, '#00f');
             g.fillStyle = l; g.fillRect(0, 0, 20, 10);",
        );
        // Angle 0 points right; the first half of the turn runs clockwise through the bottom.
        assert_eq!(p.eval("typeof g.createConicGradient(0, 0, 0)"), "object");
        let below = numbers(&p.eval("px(10, 9)"));
        let above = numbers(&p.eval("px(10, 0)"));
        assert!(below[0] > 200 && above[2] > 200, "{below:?} {above:?}");
    });
}

#[test]
fn set_line_dash_repeats_an_odd_list_and_ignores_a_bad_one() {
    on_page(|| {
        let mut p = page();
        p.eval("g.setLineDash([2, 3, 4])");
        assert_eq!(p.eval("g.getLineDash().join()"), "2,3,4,2,3,4");
        p.eval("g.setLineDash([2, -1])");
        assert_eq!(p.eval("g.getLineDash().join()"), "2,3,4,2,3,4");
        p.eval("g.setLineDash([])");
        assert_eq!(p.eval("g.getLineDash().length"), "0");
    });
}

#[test]
fn to_data_url_decodes_back_and_webp_falls_back_to_png() {
    on_page(|| {
        let mut p = page();
        p.eval("g.fillStyle = '#00ff00'; g.fillRect(0, 0, 20, 10)");
        let decoded = verve_dom::canvas::decode(&payload(&p.eval("c.toDataURL()")), None).unwrap();
        assert_eq!((decoded.width, decoded.height), (20, 10));
        assert_eq!(&decoded.pixels[..4], &[0, 255, 0, 255]);
        assert!(
            p.eval("c.toDataURL('image/webp')")
                .contains("data:image/png")
        );
        assert!(
            p.eval("c.toDataURL('image/jpeg', 0.5)")
                .contains("data:image/jpeg")
        );
    });
}

#[test]
fn to_blob_calls_back_as_a_task_and_convert_to_blob_resolves_a_blob() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "globalThis.seen = []; g.fillRect(0, 0, 20, 10);
             c.toBlob(b => seen.push('toBlob ' + (b instanceof Blob) + ' ' + b.type + ' ' + (b.size > 0)));
             seen.push('sync');
             const o = new OffscreenCanvas(3, 3);
             o.convertToBlob({ type: 'image/jpeg' }).then(b => seen.push('convert ' + b.type + ' ' + (b.size > 0)));",
        );
        p.settle();
        assert_eq!(
            p.eval("seen.join('|')"),
            "sync|convert image/jpeg true|toBlob true image/png true"
        );
    });
}

#[test]
fn measure_text_reports_real_metrics() {
    on_page(|| {
        let mut p = page();
        p.eval("g.font = '20px sans-serif'");
        assert_eq!(
            p.eval("g.measureText('W').width > g.measureText('i').width"),
            "true"
        );
        let width = p
            .eval("g.measureText('Hello').width")
            .parse::<f64>()
            .unwrap();
        assert!((30. ..80.).contains(&width), "{width}");
        assert_eq!(p.eval("g.measureText('').width"), "0");
        p.eval("g.fillStyle = 'black'; g.fillText('Hello', 0, 9)");
        assert_eq!(
            p.eval("g.getImageData(0, 0, 20, 10).data.some(v => v > 0)"),
            "true"
        );
    });
}

#[test]
fn a_cross_origin_image_taints_the_canvas() {
    on_page(|| {
        let mut p = page();
        p.eval("globalThis.i = new Image(); i.src = 'https://other.test/a.png';");
        let url = p.image_requests().remove(0);
        let bytes: Arc<[u8]> = payload(PNG_1X1).into();
        p.document
            .set_image_bytes(&mut p.runtime, &url, Some("image/png"), bytes);
        assert_eq!(p.eval("i.complete + ',' + i.naturalWidth"), "true,1");
        p.eval("g.drawImage(i, 0, 0)");
        let error = p.try_eval("g.getImageData(0, 0, 1, 1)").unwrap_err();
        assert!(error.contains("SecurityError"), "{error}");
        assert!(p.try_eval("c.toDataURL()").is_err());
    });
}

#[test]
fn committing_gives_one_generation_per_frame() {
    on_page(|| {
        let mut p = page();
        let canvas = p.find("canvas");
        let generation = |p: &mut Page| {
            let update = p.document.update();
            update.snapshot.canvases.get(&canvas).map(|f| f.generation)
        };
        p.eval("g.fillRect(0, 0, 1, 1); g.fillRect(1, 1, 1, 1); g.fillRect(2, 2, 1, 1)");
        let first = generation(&mut p).expect("a frame");
        assert_eq!(generation(&mut p), Some(first), "no drawing, no new frame");
        p.eval("g.fillRect(3, 3, 1, 1)");
        assert_eq!(generation(&mut p), Some(first + 1));
        let frame = p.document.update().snapshot.canvases[&canvas].clone();
        assert_eq!(
            (frame.width, frame.height, frame.pixels.len()),
            (20, 10, 800)
        );
    });
}

#[test]
fn images_report_their_size_and_decode() {
    on_page(|| {
        let mut p = page();
        for (name, url) in [("png", PNG_1X1), ("gif", GIF_1X1), ("webp", WEBP_1X1)] {
            let out = p.eval(&format!(
                "(() => {{ const i = new Image(); i.src = '{url}';
                   return i.complete + ',' + i.naturalWidth + 'x' + i.naturalHeight; }})()"
            ));
            assert_eq!(out, "true,1x1", "{name}");
        }
        p.eval(&format!(
            "globalThis.ok = 'pending'; const i = new Image(); i.src = '{PNG_1X1}';
             i.decode().then(() => ok = 'yes', e => ok = e.name)"
        ));
        p.settle();
        assert_eq!(p.eval("ok"), "yes");
        p.eval(
            "globalThis.bad = 'pending'; const j = new Image();
             j.src = 'data:image/png;base64,AAAA'; j.decode().then(() => bad = 'yes', e => bad = e.name)",
        );
        p.settle();
        assert_eq!(p.eval("bad"), "EncodingError");
    });
}

#[test]
fn a_pending_decode_resolves_when_the_loader_delivers_the_image() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "globalThis.out = 'pending'; globalThis.i = new Image(); i.src = 'https://cdn.test/a.png';
             i.decode().then(() => out = 'resolved', e => out = e.name);",
        );
        let url = p.image_requests().remove(0);
        p.settle();
        assert_eq!(p.eval("out"), "pending");
        p.document.set_image_bytes(
            &mut p.runtime,
            &url,
            Some("image/png"),
            payload(PNG_1X1).into(),
        );
        p.settle();
        assert_eq!(p.eval("out"), "resolved");
    });
}

#[test]
fn a_pending_decode_rejects_when_the_loader_fails() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "globalThis.out = 'pending'; globalThis.i = new Image(); i.src = 'https://cdn.test/a.png';
             i.decode().then(() => out = 'resolved', e => out = e.name);",
        );
        let url = p.image_requests().remove(0);
        p.document.fail_image(&mut p.runtime, &url);
        p.settle();
        assert_eq!(p.eval("out"), "EncodingError");
    });
}

#[test]
fn create_image_bitmap_and_offscreen_canvas_hand_over_pixels() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "globalThis.out = 'pending';
             const o = new OffscreenCanvas(4, 4); const og = o.getContext('2d');
             og.fillStyle = '#0000ff'; og.fillRect(0, 0, 4, 4);
             createImageBitmap(o).then(b => { g.drawImage(b, 0, 0); out = b.width + 'x' + b.height + ' ' + px(1, 1); });",
        );
        p.settle();
        assert_eq!(p.eval("out"), "4x4 0,0,255,255");
    });
}

/// A 100 by 60 canvas `c` with context `g`, and `ink()` giving the box of its painted pixels as
/// `minX,maxX,minY,maxY`.
fn wide() -> Page {
    let mut p = canvas_page(100, 60);
    p.eval(
        "globalThis.ink = () => {
           const d = g.getImageData(0, 0, 100, 60).data; let b = [100, -1, 60, -1];
           for (let y = 0; y < 60; y++) for (let x = 0; x < 100; x++) if (d[(y * 100 + x) * 4 + 3] > 40) {
             b = [Math.min(b[0], x), Math.max(b[1], x), Math.min(b[2], y), Math.max(b[3], y)]; }
           return b.join(',');
         };",
    );
    p
}

#[test]
fn draw_image_crops_a_source_rectangle() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const o = new OffscreenCanvas(4, 4); const og = o.getContext('2d');
             og.fillStyle = '#f00'; og.fillRect(0, 0, 2, 4);
             og.fillStyle = '#00f'; og.fillRect(2, 0, 2, 4);
             g.drawImage(o, 2, 0, 2, 4, 10, 0, 4, 4);",
        );
        assert_eq!(p.eval("px(10, 1)"), "0,0,255,255");
        assert_eq!(p.eval("px(13, 3)"), "0,0,255,255");
        assert_eq!(p.eval("px(9, 1)"), "0,0,0,0");
        assert_eq!(p.eval("px(14, 1)"), "0,0,0,0");
    });
}

#[test]
fn a_shadow_falls_beside_the_fill_and_blurs() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "g.shadowColor = 'black'; g.shadowOffsetX = 10; g.shadowBlur = 4;
             g.fillStyle = 'white'; g.fillRect(2, 3, 4, 4);",
        );
        assert_eq!(p.eval("px(3, 4)"), "255,255,255,255");
        let shadow = numbers(&p.eval("px(14, 5)"));
        assert!(shadow[3] > 30 && shadow[0] < 20, "{shadow:?}");
        assert!(
            numbers(&p.eval("px(19, 5)"))[3] < shadow[3],
            "the edge fades"
        );
        assert!(
            numbers(&p.eval("px(8, 5)"))[3] < shadow[3] / 2,
            "left of the shadow"
        );
    });
}

#[test]
fn text_align_and_baseline_move_the_text() {
    on_page(|| {
        let mut p = wide();
        p.eval("g.font = '20px sans-serif'; g.fillText('HHH', 50, 30)");
        let left = numbers(&p.eval("ink()"));
        p.eval("g.clearRect(0, 0, 100, 60); g.textAlign = 'right'; g.fillText('HHH', 50, 30)");
        let right = numbers(&p.eval("ink()"));
        p.eval("g.clearRect(0, 0, 100, 60); g.textAlign = 'center'; g.fillText('HHH', 50, 30)");
        let center = numbers(&p.eval("ink()"));
        let width = p.eval("g.measureText('HHH').width").parse::<f64>().unwrap() as i32;
        assert!(
            left[0] >= 49 && (left[1] - 50 - width).abs() <= 4,
            "{left:?} {width}"
        );
        assert!(
            right[1] <= 51 && (50 - right[0] - width).abs() <= 4,
            "{right:?} {width}"
        );
        assert!((center[0] + center[1] - 100).abs() <= 3, "{center:?}");
        // Capitals stand on the baseline; `top` hangs them from the em box's top.
        assert!((left[3] - 30).abs() <= 1, "{left:?}");
        p.eval("g.clearRect(0, 0, 100, 60); g.textAlign = 'left'; g.textBaseline = 'top'; g.fillText('HHH', 50, 30)");
        let top = numbers(&p.eval("ink()"));
        assert!(top[2] > 30 && top[2] <= 40, "{top:?}");
        p.eval("g.clearRect(0, 0, 100, 60); g.textBaseline = 'bottom'; g.fillText('HHH', 50, 30)");
        assert!(numbers(&p.eval("ink()"))[3] <= 30);
    });
}

#[test]
fn round_rect_cuts_its_corners_and_keeps_its_sides() {
    on_page(|| {
        let mut p = page();
        p.eval("g.beginPath(); g.roundRect(0, 0, 10, 10, 4); g.fill()");
        assert_eq!(numbers(&p.eval("px(0, 0)"))[3], 0);
        assert_eq!(numbers(&p.eval("px(9, 9)"))[3], 0);
        assert_eq!(p.eval("px(5, 5)"), "0,0,0,255");
        assert_eq!(numbers(&p.eval("px(5, 0)"))[3], 255);
        assert_eq!(numbers(&p.eval("px(0, 5)"))[3], 255);
        // A corner on the arc, not on the box's corner or on a control polygon.
        assert!(numbers(&p.eval("px(1, 1)"))[3] > 0);
    });
}

/// Runs the tasks due, then answers what the page recorded.
fn events_of(p: &mut Page) -> String {
    p.settle();
    p.eval("log.join(' ')")
}

#[test]
fn img_load_and_error_events_are_tasks() {
    on_page(|| {
        let mut p = page();
        p.eval(&format!(
            "globalThis.log = []; globalThis.i = new Image();
             i.src = '{PNG_1X1}'; i.onload = () => log.push('load'); i.onerror = () => log.push('error');
             log.push('sync:' + i.complete);"
        ));
        assert_eq!(events_of(&mut p), "sync:true load");
        p.eval("log.length = 0; i.src = 'data:image/png;base64,AAAA';");
        assert_eq!(events_of(&mut p), "error");
        assert_eq!(p.eval("i.complete + ',' + i.naturalWidth"), "true,0");
    });
}

#[test]
fn img_events_follow_what_the_loader_answers() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "globalThis.log = []; globalThis.a = new Image(); globalThis.b = new Image();
             for (const [n, i] of [['a', a], ['b', b]]) {
               i.onload = () => log.push(n + ' load'); i.onerror = () => log.push(n + ' error'); }
             a.src = 'https://cdn.test/a.png'; b.src = 'https://cdn.test/b.png';
             log.push(a.complete + ',' + b.complete);",
        );
        let urls = p.image_requests();
        assert_eq!(urls, ["https://cdn.test/a.png", "https://cdn.test/b.png"]);
        p.document.set_image_bytes(
            &mut p.runtime,
            &urls[0],
            Some("image/png"),
            payload(PNG_1X1).into(),
        );
        p.document.fail_image(&mut p.runtime, &urls[1]);
        assert_eq!(events_of(&mut p), "false,false a load b error");
        assert_eq!(
            p.eval("a.complete + ',' + a.naturalWidth + ',' + b.complete"),
            "true,1,true"
        );
    });
}

#[test]
fn decode_rejects_a_file_with_a_good_header_and_a_bad_body() {
    on_page(|| {
        let mut p = page();
        p.eval(&format!(
            "globalThis.result = 'pending';
             const bytes = atob('{}').slice(0, 40);
             const i = new Image(); i.src = 'data:image/png;base64,' + btoa(bytes);
             i.decode().then(() => result = 'ok', e => result = e.name);",
            PNG_1X1.split(',').nth(1).unwrap()
        ));
        p.settle();
        assert_eq!(p.eval("result"), "EncodingError");
    });
}

#[test]
fn a_shadow_stays_inside_the_clip() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "g.beginPath(); g.rect(0, 0, 10, 10); g.clip();
             g.shadowColor = 'black'; g.shadowOffsetX = 6; g.shadowBlur = 1;
             g.fillStyle = 'white'; g.fillRect(2, 3, 4, 4);",
        );
        assert!(numbers(&p.eval("px(9, 5)"))[3] > 100, "inside the clip");
        assert_eq!(numbers(&p.eval("px(11, 5)"))[3], 0, "outside the clip");
    });
}

#[test]
fn a_conic_gradient_casts_a_shadow_like_any_fill() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const l = g.createConicGradient(0, 4, 4);
             l.addColorStop(0, '#f00'); l.addColorStop(1, '#f00');
             g.shadowColor = 'black'; g.shadowOffsetX = 10; g.shadowBlur = 1;
             g.fillStyle = l; g.fillRect(0, 0, 6, 8);",
        );
        assert!(numbers(&p.eval("px(13, 4)"))[3] > 100, "the shadow");
    });
}

#[test]
fn color_stops_added_out_of_order_still_blend_in_order() {
    on_page(|| {
        let mut p = page();
        p.eval(
            "const l = g.createConicGradient(0, 10, 5);
             l.addColorStop(1, '#00f'); l.addColorStop(0.5, '#f00'); l.addColorStop(0.5, '#00f');
             l.addColorStop(0, '#f00');
             g.fillStyle = l; g.fillRect(0, 0, 20, 10);",
        );
        // The turn runs clockwise from the right: red through the bottom, blue over the top.
        let below = numbers(&p.eval("px(10, 9)"));
        let above = numbers(&p.eval("px(10, 0)"));
        assert!(below[0] > 200 && above[2] > 200, "{below:?} {above:?}");
    });
}

#[test]
fn a_context_knows_its_canvas_and_a_style_is_one_object() {
    on_page(|| {
        let mut p = page();
        assert_eq!(p.eval("g.canvas === c && c.getContext('2d') === g"), "true");
        p.eval("g.canvas = null");
        assert_eq!(p.eval("g.canvas === c"), "true");
        p.eval("globalThis.l = g.createLinearGradient(0, 0, 1, 1); g.fillStyle = l");
        assert_eq!(p.eval("g.fillStyle === l"), "true");
    });
}
