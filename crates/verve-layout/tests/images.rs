//! Pictures in the layout: the size an `<img>` takes from its picture, where `object-fit` puts
//! the picture, and the tiles of `background-image: url()`. The pictures are `data:` PNGs, which
//! the document reads at once.

mod common;

use common::{Laid, lay_out, on_page};
use verve_layout::{Fill, ObjectFit, PaintItem, key_for_url};

const RESET: &str = "<style>html,body{margin:0} img{display:block}</style>";
const P4X2: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAACCAYAAAB/qH1jAAAAEklEQVR4nGP4z8DwHxkzoAsAAA8hD/EEN8afAAAAAElFTkSuQmCC";
const P40X20: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACgAAAAUCAYAAAD/Rn+7AAAAL0lEQVR4nO3OMQ0AAAjAsPk3DSrIOHb0LwPzGXagYEE7ULCgHShY0A4ULGgHCl5biXs6G0TdvUkAAAAASUVORK5CYII=";

fn page(html: &str) -> Laid {
    lay_out(&format!("{RESET}{html}"), "")
}

fn image_of(page: &Laid, id: &str) -> (verve_dom::Rect, verve_dom::Rect, ObjectFit) {
    page.items_of(id)
        .into_iter()
        .find_map(|item| match item {
            PaintItem::Image {
                bounds, clip, fit, ..
            } => Some((bounds, clip, fit)),
            _ => None,
        })
        .expect("the picture is in the list")
}

fn rounded(rect: verve_dom::Rect) -> [i32; 4] {
    [rect.x, rect.y, rect.width, rect.height].map(|v| v.round() as i32)
}

// The sizes below are what Electron 39.8.10 (Chromium 142) laid out for the same markup with a
// 4x2 picture: `getBoundingClientRect()` of each `<img>`.
#[test]
fn an_img_is_as_big_as_its_picture_and_keeps_its_ratio() {
    on_page(|| {
        let page = page(&format!(
            r#"<img id="a" src="{P4X2}"><img id="b" src="{P4X2}" width="100">
               <img id="c" src="{P4X2}" height="20"><img id="d" src="{P4X2}" style="max-width:2px">
               <img id="e" src="{P4X2}" width="100" height="100"><img id="f" src="{P4X2}" style="width:50%">"#
        ));
        assert_eq!(page.rect("a")[2..], [4., 2.]);
        assert_eq!(page.rect("b")[2..], [100., 50.]);
        assert_eq!(page.rect("c")[2..], [40., 20.]);
        assert_eq!(page.rect("d")[2..], [2., 1.]);
        assert_eq!(page.rect("e")[2..], [100., 100.]);
        assert_eq!(page.rect("f")[2..], [400., 200.]);
    });
}

#[test]
fn an_img_without_a_picture_is_what_its_attributes_say() {
    on_page(|| {
        let page = page(
            r#"<img id="a" src="http://127.0.0.1:9/none.png" width="100" height="50">
               <img id="b" width="30"><img id="c" alt="">"#,
        );
        assert_eq!(page.rect("a")[2..], [100., 50.]);
        assert_eq!(page.rect("b")[2], 30.);
        assert_eq!(page.rect("c")[2..], [0., 0.]);
    });
}

#[test]
fn the_picture_is_drawn_into_the_fitted_rectangle_and_cut_to_the_content_box() {
    on_page(|| {
        let page = page(&format!(
            r#"<img id="a" src="{P40X20}" style="width:100px;height:100px;object-fit:contain">
               <img id="b" src="{P40X20}" style="width:100px;height:100px;object-fit:cover;object-position:0 0">
               <img id="c" src="{P40X20}" style="width:100px;height:100px;object-fit:none;padding:5px">"#
        ));
        let (bounds, clip, fit) = image_of(&page, "a");
        assert_eq!(rounded(bounds), [0, 25, 100, 50]);
        assert_eq!(rounded(clip), [0, 0, 100, 100]);
        assert_eq!(fit, ObjectFit::Contain);
        let (bounds, clip, _) = image_of(&page, "b");
        assert_eq!(rounded(bounds), [0, 100, 200, 100]);
        assert_eq!(rounded(clip), [0, 100, 100, 100]);
        // `padding: 5px` on a content-box image: a 100x100 content box inside 110x110.
        let (bounds, clip, _) = image_of(&page, "c");
        assert_eq!(rounded(clip), [5, 205, 100, 100]);
        assert_eq!(rounded(bounds), [35, 245, 40, 20]);
        let key = key_for_url(P40X20);
        assert!(
            page.list().items.iter().any(
                |item| matches!(item, PaintItem::Image { image, frame: 0, .. } if *image == key)
            )
        );
    });
}

fn picture_fill(page: &Laid, id: &str) -> Fill {
    page.items_of(id)
        .into_iter()
        .find_map(|item| match item {
            PaintItem::Box { background, .. } => background
                .into_iter()
                .find(|fill| matches!(fill, Fill::Picture { .. })),
            _ => None,
        })
        .expect("a picture layer")
}

#[test]
fn a_background_image_is_tiled_over_the_box() {
    on_page(|| {
        let page = page(&format!(
            r#"<div id="a" style="width:100px;height:50px;background:url({P40X20})"></div>
               <div id="b" style="width:100px;height:50px;background:url({P40X20}) no-repeat center / cover"></div>
               <div id="c" style="width:100px;height:50px;background-image:url({P40X20});background-repeat:repeat-x;background-size:20px auto"></div>"#
        ));
        let Fill::Picture {
            area,
            tile,
            step,
            count,
            ..
        } = picture_fill(&page, "a")
        else {
            unreachable!()
        };
        assert_eq!(rounded(area), [0, 0, 100, 50]);
        assert_eq!(rounded(tile), [0, 0, 40, 20]);
        assert_eq!((step, count), ([40., 20.], [3, 3]));
        // `cover` makes a 100x50 tile of the 2:1 picture, so one tile sits in the middle.
        let Fill::Picture { tile, count, .. } = picture_fill(&page, "b") else {
            unreachable!()
        };
        assert_eq!(rounded(tile), [0, 0, 100, 50]);
        assert_eq!(count, [1, 1]);
        let Fill::Picture {
            tile, step, count, ..
        } = picture_fill(&page, "c")
        else {
            unreachable!()
        };
        assert_eq!(rounded(tile), [0, 0, 20, 10]);
        assert_eq!((step[0], count), (20., [5, 1]));
    });
}

// Observed in Electron 39.8.10 (Chromium 142), DPR 1, `getBoundingClientRect()` of the same markup
// (DPR 1).
#[test]
fn a_css_height_makes_the_width_by_the_ratio() {
    on_page(|| {
        let page = page(&format!(
            r#"<img id="a" src="{P4X2}" style="height:50px">
               <div style="width:400px"><img id="b" src="{P4X2}" width="800" height="400" style="max-width:100%;height:auto"></div>
               <img id="c" src="{P40X20}" style="max-height:10px">
               <img id="d" src="{P40X20}" style="min-height:40px">"#
        ));
        assert_eq!(page.rect("a")[2..], [100., 50.]);
        assert_eq!(page.rect("b")[2..], [400., 200.]);
        assert_eq!(page.rect("c")[2..], [20., 10.]);
        assert_eq!(page.rect("d")[2..], [80., 40.]);
    });
}

// Inline is an `<img>`'s default display, and atoms take their own path through layout.
#[test]
fn an_inline_img_is_sized_like_a_block_one() {
    on_page(|| {
        let page = lay_out(
            &format!(
                r#"<style>html,body{{margin:0}}</style>
                   <img id="a" src="{P4X2}"><img id="b" src="{P4X2}" width="100">
                   <img id="c" src="{P4X2}" style="height:50px">
                   <img id="d" src="{P40X20}" style="max-height:10px">"#
            ),
            "",
        );
        assert_eq!(page.rect("a")[2..], [4., 2.]);
        assert_eq!(page.rect("b")[2..], [100., 50.]);
        assert_eq!(page.rect("c")[2..], [100., 50.]);
        assert_eq!(page.rect("d")[2..], [20., 10.]);
    });
}

// Observed in Electron 39.8.10 (Chromium 142), DPR 1, with a 40x20 picture: a side that was given
// is not scaled by a limit on the other one, and with neither given the limits cut the size by
// the table of CSS 2.1, 10.4.
#[test]
fn a_given_side_keeps_its_size_against_a_limit_on_the_other() {
    on_page(|| {
        let page = page(&format!(
            r#"<img id="a" src="{P40X20}" width="40" style="max-height:10px">
               <img id="b" src="{P40X20}" style="width:40px;min-height:40px">
               <img id="c" src="{P40X20}" width="100" height="100" style="max-width:50px">
               <div style="width:10px"><img id="d" src="{P40X20}" height="20" style="max-width:100%"></div>
               <img id="e" src="{P40X20}" style="max-width:10px;max-height:4px">
               <img id="f" src="{P40X20}" style="min-height:40px">
               <div style="width:400px"><img id="g" src="{P40X20}" width="800" height="400" style="max-width:100%"></div>"#
        ));
        assert_eq!(page.rect("a")[2..], [40., 10.]);
        assert_eq!(page.rect("b")[2..], [40., 40.]);
        assert_eq!(page.rect("c")[2..], [50., 100.]);
        assert_eq!(page.rect("d")[2..], [10., 20.]);
        assert_eq!(page.rect("e")[2..], [8., 4.]);
        assert_eq!(page.rect("f")[2..], [80., 40.]);
        assert_eq!(page.rect("g")[2..], [400., 400.]);
    });
}

#[test]
fn a_percentage_padding_sets_where_a_background_starts_with_content_box_origin() {
    on_page(|| {
        let page = page(&format!(
            r#"<div id="a" style="width:100px;height:50px;padding:10%;box-sizing:content-box;background:url({P40X20}) no-repeat;background-origin:content-box"></div>"#
        ));
        let Fill::Picture { tile, .. } = picture_fill(&page, "a") else {
            unreachable!()
        };
        // 10% of the 800px body is 80px: the picture starts at the content box.
        assert_eq!(rounded(tile), [80, 80, 40, 20]);
    });
}

#[test]
fn an_inline_svg_is_a_vector_picture_and_two_copies_are_one_drawing() {
    on_page(|| {
        let page = page(
            r#"<div style="color:green">
                 <svg width="40" height="20" viewBox="0 0 20 10"><path d="M0 0h10v5z" fill="currentColor"/></svg>
                 <svg width="40" height="20" viewBox="0 0 20 10"><path d="M0 0h10v5z" fill="currentColor"/></svg>
               </div>"#,
        );
        let list = page.list();
        let keys: Vec<_> = list
            .items
            .iter()
            .filter_map(|item| match item {
                PaintItem::Image { image, bounds, .. } => Some((*image, rounded(*bounds))),
                _ => None,
            })
            .collect();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].0, keys[1].0, "the same text is the same key");
        assert_eq!(keys[0].1[2..], [40, 20]);
        assert_eq!(list.vectors.len(), 1);
        let text = String::from_utf8(list.vectors[0].source.to_vec()).expect("text");
        // The size is the laid-out one, `currentColor` has the computed `color` to resolve against.
        assert!(
            text.contains("width=\"40\"") && text.contains("height=\"20\""),
            "{text}"
        );
        assert!(text.contains("color:rgb(0, 128, 0)"), "{text}");
        assert_eq!(list.vectors[0].key, keys[0].0);
    });
}

#[test]
fn the_elements_inside_a_drawing_have_the_boxes_the_drawing_gives_them() {
    on_page(|| {
        let page = page(
            r#"<svg id="s" width="200" height="100" viewBox="0 0 100 50" style="position:absolute;left:10px;top:20px">
                 <rect id="r" x="10" y="5" width="30" height="20" fill="red" stroke="blue" stroke-width="4"/>
                 <g id="g" transform="translate(50,10)"><circle id="c" cx="10" cy="10" r="8"/></g>
               </svg>"#,
        );
        // The same numbers Electron 39.8.10 gave for this markup: `getBoundingClientRect` has the
        // viewBox scale, the transform and the page offset in it and leaves out the stroke.
        assert_eq!(page.rect("r"), [30., 30., 60., 40.]);
        assert_eq!(page.rect("g"), [114., 44., 32., 32.]);
        assert_eq!(page.rect("c"), [114., 44., 32., 32.]);
        let boxes: std::collections::HashMap<_, _> = page.engine.svg_boxes().collect();
        // `getBBox` is in the units of the drawing and leaves out the element's own transform.
        assert_eq!(
            boxes[&page.node("r")],
            verve_dom::Rect::new(10., 5., 30., 20.)
        );
        assert_eq!(
            boxes[&page.node("g")],
            verve_dom::Rect::new(2., 2., 16., 16.)
        );
        assert_eq!(
            boxes[&page.node("s")],
            verve_dom::Rect::new(10., 5., 58., 23.)
        );
        // No box of its own for anything in a drawing, and the drawing itself is where it is.
        assert_eq!(page.rect("s"), [10., 20., 200., 100.]);
    });
}

fn vector_text(page: &Laid) -> String {
    let list = page.list();
    String::from_utf8(list.vectors[0].source.to_vec()).expect("text")
}

#[test]
fn a_use_of_a_symbol_outside_the_drawing_is_copied_in() {
    on_page(|| {
        let page = page(
            r##"<svg style="display:none"><symbol id="sym" viewBox="0 0 10 10"><rect width="10" height="10" fill="#0f0"/></symbol></svg>
                <svg width="10" height="10"><use href="#sym" width="10" height="10"/></svg>"##,
        );
        let text = vector_text(&page);
        assert!(text.contains("<defs><symbol"), "{text}");
        assert_eq!(first_pixel(&text), [0, 255, 0, 255], "{text}");
    });
}

#[test]
fn attributes_without_a_namespace_or_a_valid_name_do_not_blank_the_drawing() {
    on_page(|| {
        let page = page(
            r##"<svg width="10" height="10" xmlns:sketch="http://x" sketch:type="a" @click="f" :class="c" x-on:click="g"><rect width="10" height="10" fill="red"/></svg>"##,
        );
        let text = vector_text(&page);
        assert!(
            !text.contains("sketch") && !text.contains("@click"),
            "{text}"
        );
        let bytes = verve_layout::rasterize_svg(text.as_bytes(), 10, 10).expect("it parses");
        assert_eq!(&bytes[..4], &[0, 0, 255, 255]);
    });
}

#[test]
fn the_opacity_of_the_svg_box_is_applied_once() {
    on_page(|| {
        let page = page(
            r#"<svg width="10" height="10" style="opacity:.5"><rect width="10" height="10"/></svg>"#,
        );
        assert!(!vector_text(&page).contains("opacity"));
    });
}

#[test]
fn fill_and_stroke_set_on_an_html_ancestor_reach_the_drawing() {
    on_page(|| {
        let page = page(
            r#"<style>.btn{fill:red;stroke:blue;stroke-width:3}</style>
               <div class="btn"><svg width="10" height="10"><rect width="10" height="10"/></svg></div>"#,
        );
        let text = vector_text(&page);
        assert!(text.contains("fill:rgb(255, 0, 0)"), "{text}");
        assert!(
            text.contains("stroke-width:3px") || text.contains("stroke-width:3"),
            "{text}"
        );
    });
}

/// The first pixel of the drawing in `text`, 10 by 10, as BGRA.
fn first_pixel(text: &str) -> [u8; 4] {
    let bytes = verve_layout::rasterize_svg(text.as_bytes(), 10, 10).expect("it parses");
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

// What Electron 39.8.10 painted for these (probe p35-review4\probe\out-svg.json).
#[test]
fn inherited_painting_properties_do_not_beat_the_attributes_of_the_elements() {
    on_page(|| {
        let blue = [255, 0, 0, 255];
        let under_div = page(
            r#"<div style="fill:red"><svg width="10" height="10"><rect width="10" height="10" fill="blue"/></svg></div>"#,
        );
        assert_eq!(first_pixel(&vector_text(&under_div)), blue);
        let logo = page(
            r##"<style>.logo{fill:currentColor;color:red}</style><svg class="logo" width="10" height="10"><rect width="10" height="10" fill="#00f"/></svg>"##,
        );
        assert_eq!(first_pixel(&vector_text(&logo)), blue);
        let stroked = page(
            r#"<div style="stroke:red;stroke-width:20px"><svg width="10" height="10"><rect width="10" height="10" fill="none" stroke-width="0"/></svg></div>"#,
        );
        assert_eq!(
            first_pixel(&vector_text(&stroked))[3],
            0,
            "nothing is drawn"
        );
    });
}

#[test]
fn a_filter_on_the_svg_root_is_applied_by_the_drawing() {
    on_page(|| {
        let page = page(
            r#"<svg width="10" height="10" style="filter:invert(1)"><rect width="10" height="10" fill="red"/></svg>"#,
        );
        // Cyan, as BGRA.
        assert_eq!(first_pixel(&vector_text(&page)), [255, 255, 0, 255]);
    });
}

#[test]
fn elements_of_another_vocabulary_are_left_out_and_the_drawing_still_draws() {
    on_page(|| {
        let page = page(
            r#"<svg width="10" height="10"><sodipodi:namedview id="base"/><metadata><rdf:RDF><cc:Work/></rdf:RDF></metadata><rect width="10" height="10" fill="lime"/></svg>"#,
        );
        let text = vector_text(&page);
        assert!(
            !text.contains("sodipodi") && !text.contains("rdf:"),
            "{text}"
        );
        assert_eq!(first_pixel(&text), [0, 255, 0, 255]);
    });
}
