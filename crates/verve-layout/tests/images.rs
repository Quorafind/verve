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
