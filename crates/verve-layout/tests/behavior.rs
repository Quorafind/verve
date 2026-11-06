//! What the engine does beyond where boxes land: the display list's items and their order, clips
//! and opacity groups, hit boxes, scrolling, sticky and fixed boxes, and keeping up with a
//! document that changes. The geometry itself is checked against Chromium in `chromium.rs`.

mod common;

use std::sync::Arc;

use common::{Laid, lay_out, on_page};
use verve_dom::{Align, Rect, ScrollRequest};
use verve_layout::{AppRegionKind, BorderLine, Cursor, Fill, PaintItem};

const RESET: &str = "<style>html,body{margin:0}</style>";

/// A page with the body's margin taken away, so boxes start at the corner.
fn page(html: &str) -> Laid {
    lay_out(
        &format!("{RESET}{html}"),
        "document.documentElement.id = 'page';",
    )
}

fn boxes_of(page: &Laid, id: &str) -> Vec<PaintItem> {
    page.items_of(id)
        .into_iter()
        .filter(|item| matches!(item, PaintItem::Box { .. }))
        .collect()
}

fn position_of(items: &[PaintItem], wanted: impl Fn(&PaintItem) -> bool) -> usize {
    items
        .iter()
        .position(wanted)
        .expect("the item is in the list")
}

#[test]
fn display_none_and_contents_have_no_box_of_their_own() {
    on_page(|| {
        let page = page(
            r#"<div id="outer" style="display:flex">
                <div id="wrapper" style="display:contents"><div id="kept" style="width:10px;height:10px"></div></div>
                <div id="gone" style="display:none"><div id="inside" style="width:5px;height:5px"></div></div>
                <span id="inline"><div id="lifted" style="width:7px;height:7px"></div></span>
            </div>"#,
        );
        for id in ["wrapper", "gone", "inside"] {
            assert!(page.engine.rect(page.node(id)).is_none(), "{id} has a box");
        }
        assert_eq!(page.rect("kept")[2..], [10., 10.]);
        // An inline child of a flex container is blockified: a flex item, stretched to the row,
        // with the block child inside it.
        assert_eq!(page.rect("inline"), [10., 0., 7., 10.]);
        assert_eq!(page.rect("lifted"), [10., 0., 7., 7.]);
    });
}

#[test]
fn visibility_hidden_keeps_the_room_and_paints_nothing() {
    on_page(|| {
        let page = page(
            r#"<div id="hidden" style="visibility:hidden;width:20px;height:10px;background:red"></div>
               <div id="below" style="width:20px;height:10px"></div>"#,
        );
        assert_eq!(page.rect("below")[1], 10.);
        assert!(page.items_of("hidden").is_empty());
    });
}

#[test]
fn a_box_paints_its_background_border_radius_and_shadow() {
    on_page(|| {
        let page = page(
            r##"<div id="b" style="width:100px;height:50px;background:#f00;border:2px solid #00f;
                border-radius:10px;box-shadow:0 2px 4px rgba(0,0,0,.5)"></div>"##,
        );
        let items = boxes_of(&page, "b");
        let [
            PaintItem::Box {
                bounds,
                radii,
                background,
                border,
                shadows,
                ..
            },
        ] = items.as_slice()
        else {
            panic!("one box item, got {items:?}");
        };
        assert_eq!(*bounds, Rect::new(0., 0., 104., 54.));
        assert_eq!(*radii, [10.; 4]);
        assert_eq!(*background, vec![Fill::Color([1., 0., 0., 1.])]);
        assert_eq!(border.widths, [2.; 4]);
        assert_eq!(border.colors, [[0., 0., 1., 1.]; 4]);
        assert_eq!(border.styles, [BorderLine::Solid; 4]);
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].offset, [0., 2.]);
        assert_eq!(shadows[0].blur, 4.);
        assert_eq!(shadows[0].color, [0., 0., 0., 0.5]);
    });
}

#[test]
fn the_canvas_takes_the_bodys_background_when_the_root_has_none() {
    on_page(|| {
        let page = lay_out(
            "<style>html,body{margin:0} body{background:#0f0}</style><div style=\"height:10px\"></div>",
            "",
        );
        let body = page.snapshot.dom.body;
        let items = page.list().items.clone();
        let Some(PaintItem::Box {
            node,
            bounds,
            background,
            ..
        }) = items.first()
        else {
            panic!("the first item is the canvas, got {:?}", items.first());
        };
        assert_eq!(*node, body);
        assert_eq!(*bounds, Rect::new(0., 0., 800., 600.));
        assert_eq!(*background, vec![Fill::Color([0., 1., 0., 1.])]);
        let for_body = items
            .iter()
            .filter(|item| matches!(item, PaintItem::Box { node, .. } if *node == body))
            .count();
        assert_eq!(for_body, 1, "the body does not paint the background again");
    });
}

#[test]
fn overflow_hidden_clips_what_is_inside_it() {
    on_page(|| {
        let page = page(
            r##"<div id="p" style="overflow:hidden;width:100px;height:100px;border:5px solid #000">
                <div id="c" style="width:300px;height:300px;background:#123"></div></div>"##,
        );
        let items = page.list().items.clone();
        let own = position_of(
            &items,
            |i| matches!(i, PaintItem::Box { node, .. } if *node == page.node("p")),
        );
        let push = position_of(&items, |i| matches!(i, PaintItem::PushClip { .. }));
        let child = position_of(
            &items,
            |i| matches!(i, PaintItem::Box { node, .. } if *node == page.node("c")),
        );
        let pop = position_of(&items, |i| matches!(i, PaintItem::PopClip));
        assert!(own < push && push < child && child < pop, "{items:?}");
        assert_eq!(
            items[push],
            PaintItem::PushClip {
                bounds: Rect::new(5., 5., 100., 100.),
                radii: [0.; 4]
            }
        );
        let Some(PaintItem::Hitbox { bounds, .. }) = page
            .items_of("c")
            .into_iter()
            .find(|item| matches!(item, PaintItem::Hitbox { .. }))
        else {
            panic!("the child has a hitbox");
        };
        assert_eq!(bounds, Rect::new(5., 5., 100., 100.));
        assert_eq!(
            page.engine
                .hit_test(150., 150.)
                .map(|n| page.name(n))
                .as_deref(),
            Some("page")
        );
        assert_eq!(
            page.engine
                .hit_test(50., 50.)
                .map(|n| page.name(n))
                .as_deref(),
            Some("c")
        );
    });
}

#[test]
fn an_absolute_box_is_clipped_by_its_containing_block_not_by_its_parent() {
    on_page(|| {
        let hitbox_of_abs = |clipper: &str| {
            let page = page(&format!(
                r##"<div style="position:relative;width:100px;height:100px">
                    <div style="{clipper};overflow:hidden;width:50px;height:50px">
                    <div id="abs" style="position:absolute;left:0;top:0;width:80px;height:80px;background:red"></div>
                    </div></div>"##
            ));
            page.items_of("abs")
                .into_iter()
                .find_map(|item| match item {
                    PaintItem::Hitbox { bounds, .. } => Some(bounds),
                    _ => None,
                })
                .expect("a hitbox")
        };
        // The clipper is not positioned, so the box belongs to the outer block and overflows it.
        assert_eq!(hitbox_of_abs("margin:0"), Rect::new(0., 0., 80., 80.));
        assert_eq!(
            hitbox_of_abs("position:relative"),
            Rect::new(0., 0., 50., 50.)
        );
    });
}

#[test]
fn opacity_groups_a_context_and_siblings_are_separate_groups() {
    on_page(|| {
        let page = page(
            r##"<div id="o" style="opacity:0.5;width:50px;height:50px;background:#f00">
                <div id="oc" style="width:10px;height:10px;background:#00f"></div></div>"##,
        );
        let items = page.list().items.clone();
        let push = position_of(&items, |i| *i == PaintItem::PushOpacity(0.5));
        let own = position_of(
            &items,
            |i| matches!(i, PaintItem::Box { node, .. } if *node == page.node("o")),
        );
        let inner = position_of(
            &items,
            |i| matches!(i, PaintItem::Box { node, .. } if *node == page.node("oc")),
        );
        let pop = position_of(&items, |i| *i == PaintItem::PopOpacity);
        assert!(push < own && own < inner && inner < pop, "{items:?}");

        let two = page_two_groups();
        let kinds: Vec<&str> = two
            .iter()
            .filter_map(|item| match item {
                PaintItem::PushOpacity(_) => Some("push"),
                PaintItem::PopOpacity => Some("pop"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["push", "pop", "push", "pop"]);
    });
}

fn page_two_groups() -> Vec<PaintItem> {
    let page = page(
        r##"<div style="opacity:0.5;width:10px;height:10px;background:red"></div>
            <div style="opacity:0.5;width:10px;height:10px;background:red"></div>"##,
    );
    page.list().items.clone()
}

#[test]
fn hitboxes_carry_the_cursor_and_the_app_region() {
    on_page(|| {
        let page = page(
            r#"<div id="d" style="-webkit-app-region:drag;cursor:pointer;width:100px;height:30px">
                <div id="n" style="-webkit-app-region:no-drag;width:20px;height:20px"></div></div>
               <div id="ignored" style="pointer-events:none;width:50px;height:10px"></div>"#,
        );
        let hitbox = |id: &str| {
            page.items_of(id).into_iter().find_map(|item| match item {
                PaintItem::Hitbox {
                    cursor,
                    app_region,
                    bounds,
                    ..
                } => Some((cursor, app_region, bounds)),
                _ => None,
            })
        };
        let (cursor, region, bounds) = hitbox("d").expect("d has a hitbox");
        assert_eq!(cursor, Cursor::Pointer);
        assert_eq!(region, Some(AppRegionKind::Drag));
        assert_eq!(bounds, Rect::new(0., 0., 100., 30.));
        let (cursor, region, _) = hitbox("n").expect("n has a hitbox");
        assert_eq!(cursor, Cursor::Pointer, "cursor is inherited");
        assert_eq!(region, Some(AppRegionKind::NoDrag));
        assert!(hitbox("ignored").is_none());
        let under = page.engine.hit_test(10., 35.).map(|n| page.name(n));
        assert_eq!(
            under.as_deref(),
            Some("body"),
            "the box under a pointer-events:none one answers"
        );
    });
}

#[test]
fn a_scroll_container_with_overflow_gets_a_scroller_that_follows_scrolling() {
    on_page(|| {
        let mut page = page(
            r#"<div id="s" style="overflow:auto;width:100px;height:100px"><div style="width:100px;height:300px"></div></div>
               <div id="fits" style="overflow:auto;width:100px;height:100px"><div style="height:50px"></div></div>
               <div id="quiet" style="overflow:auto;scrollbar-width:none;width:100px;height:100px"><div style="height:300px"></div></div>"#,
        );
        let scroller = |page: &Laid, id: &str| {
            page.items_of(id).into_iter().find_map(|item| match item {
                PaintItem::Scroller {
                    viewport,
                    content,
                    offset,
                    scrollbar,
                    ..
                } => Some((viewport, content, offset, scrollbar)),
                _ => None,
            })
        };
        assert_eq!(
            scroller(&page, "s"),
            Some((Rect::new(0., 0., 100., 100.), [100., 300.], [0., 0.], true))
        );
        assert_eq!(scroller(&page, "fits"), None);
        assert_eq!(scroller(&page, "quiet").map(|s| s.3), Some(false));

        let before = page.list();
        let framed = |list: &verve_layout::DisplayList| {
            list.items.iter().any(
                |item| matches!(item, PaintItem::PushScroll { node } if *node == page.node("s")),
            )
        };
        assert!(framed(&before), "the content of a scroller is a frame");
        let moved = page.scroll_to("s", 0., 500.);
        assert_eq!(moved, vec![(page.node("s"), [0., 200.])]);
        assert_eq!(scroller(&page, "s").map(|s| s.2), Some([0., 200.]));
        // A scroll changes the offset the scroller holds and nothing else in the list.
        let without_scrollers = |list: &verve_layout::DisplayList| {
            list.items
                .iter()
                .filter(|item| !matches!(item, PaintItem::Scroller { .. }))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(without_scrollers(&before), without_scrollers(&page.list()));
        assert!(!Arc::ptr_eq(&before, &page.list()));
        assert!(page.list().revision > before.revision);
        // Nothing moves a second time, and a scroll up is clamped at the top.
        assert!(page.scroll_to("s", 0., 500.).is_empty());
        let node = page.node("s");
        let moved = page.engine.apply_scroll(&ScrollRequest::By {
            node,
            left: 0.,
            top: -1000.,
        });
        assert_eq!(moved, vec![(node, [0., 0.])]);
        // A box that does not scroll takes no scroll.
        assert!(page.scroll_to("fits", 0., 40.).is_empty());
    });
}

#[test]
fn the_shell_can_scroll_a_frame_ahead_of_the_renderer() {
    on_page(|| {
        let page = page(
            r#"<div id="s" style="overflow:auto;width:100px;height:100px">
                <div id="a" style="height:100px"></div><div id="b" style="height:100px"></div></div>"#,
        );
        let list = page.list();
        let name = |node| page.name(node);
        assert_eq!(
            list.hit_test(50., 50., &|_| None).map(name),
            Some("a".into())
        );
        let ahead = |node| (node == page.node("s")).then_some([0., 100.]);
        assert_eq!(list.hit_test(50., 50., &ahead).map(name), Some("b".into()));
        // The resolved hitbox of `b` is where the shell has put it, cut to the container.
        let hitboxes = list.resolved_hitboxes(&ahead);
        let b = hitboxes
            .iter()
            .find(|h| h.node == page.node("b"))
            .expect("b");
        assert_eq!(b.bounds, Rect::new(0., 0., 100., 100.));
        let a = hitboxes.iter().find(|h| h.node == page.node("a"));
        assert!(a.is_none(), "a is scrolled out of the container");
    });
}

#[test]
fn an_outline_waits_for_its_own_context_not_an_inner_one() {
    on_page(|| {
        let page = page(
            r##"<div id="ol" style="outline:3px solid #f00;width:50px;height:10px"></div>
                <div id="op" style="opacity:0.5;width:20px;height:20px;background:#00f"></div>"##,
        );
        let items = page.list().items.clone();
        let pop = position_of(&items, |i| *i == PaintItem::PopOpacity);
        let outline = position_of(&items, |i| {
            matches!(i, PaintItem::Box { node, bounds, .. }
                if *node == page.node("ol") && bounds.x == -3.)
        });
        assert!(pop < outline, "the outline is outside the group: {items:?}");
    });
}

#[test]
fn scroll_into_view_scrolls_the_containers_above_the_box() {
    on_page(|| {
        let mut page = page(
            r#"<div id="sc" style="overflow:auto;scrollbar-width:none;width:100px;height:100px">
                <div style="height:50px"></div><div style="height:50px"></div><div style="height:50px"></div>
                <div style="height:50px"></div><div id="i4" style="height:50px"></div><div style="height:50px"></div>
               </div>
               <div id="tall" style="height:2000px"></div><div id="target" style="height:10px"></div>"#,
        );
        let reveal = |page: &mut Laid, id: &str, block: Align| {
            let node = page.node(id);
            page.engine.apply_scroll(&ScrollRequest::Reveal {
                node,
                block,
                inline: Align::Nearest,
                smooth: false,
            })
        };
        let moved = reveal(&mut page, "i4", Align::Nearest);
        assert_eq!(moved, vec![(page.node("sc"), [0., 150.])]);
        assert_eq!(page.rect("i4"), [0., 50., 100., 50.]);

        let html = page.node("page");
        let moved = reveal(&mut page, "target", Align::Start);
        // 100 + 2000 + 10 of content in a window of 600: the page scrolls as far as it can.
        assert_eq!(moved, vec![(html, [0., 1510.])]);
        assert_eq!(page.rect("target")[1], 590.);
    });
}

#[test]
fn a_fixed_box_stays_while_the_page_scrolls() {
    on_page(|| {
        let mut page = page(
            r#"<div id="spacer" style="height:2000px"></div>
               <div id="fx" style="position:fixed;top:10px;left:20px;width:30px;height:40px"></div>
               <div id="abs" style="position:absolute;top:100px;left:0;width:30px;height:40px"></div>"#,
        );
        page.scroll_to("page", 0., 500.);
        assert_eq!(page.rect("spacer")[1], -500.);
        assert_eq!(page.rect("fx"), [20., 10., 30., 40.]);
        // An absolute box whose containing block is the page scrolls with it.
        assert_eq!(page.rect("abs")[1], -400.);
    });
}

#[test]
fn a_sticky_box_is_held_inside_its_parent() {
    on_page(|| {
        let mut page = page(
            r#"<div id="sc" style="overflow:auto;scrollbar-width:none;width:100px;height:100px">
                <div id="par" style="height:300px"><div id="st" style="position:sticky;top:0;height:20px"></div></div>
                <div style="height:200px"></div></div>"#,
        );
        assert_eq!(page.rect("st")[1], 0.);
        page.scroll_to("sc", 0., 100.);
        assert_eq!(
            page.rect("st")[1],
            0.,
            "it sticks to the top of the scroll container"
        );
        page.scroll_to("sc", 0., 290.);
        assert_eq!(
            page.rect("st")[1],
            -10.,
            "it leaves with the bottom of its parent"
        );
    });
}

#[test]
fn the_page_follows_a_document_that_changes_and_rests_when_it_does_not() {
    on_page(|| {
        let mut page =
            page(r#"<div id="a" style="height:40px"></div><div id="b" style="height:10px"></div>"#);
        let before = page.list();
        let again = page.document.update();
        assert!(!page.engine.update(&again.snapshot, &again.changes));
        assert!(Arc::ptr_eq(&before, &page.engine.display_list()));

        page.runtime
            .eval("document.getElementById('a').style.height = '80px'")
            .expect("the script runs");
        let update = page.document.update();
        assert!(page.engine.update(&update.snapshot, &update.changes));
        page.snapshot = update.snapshot;
        assert_eq!(page.rect("a")[3], 80.);
        assert_eq!(page.rect("b")[1], 80.);
    });
}

#[test]
fn what_cannot_be_laid_out_is_counted_and_left_alone() {
    on_page(|| {
        let page = page(
            r#"<div id="m" style="width:calc(100% - 10px);height:10px"></div>
               <div id="r" style="transform:rotate(10deg);width:20px;height:10px"></div>"#,
        );
        assert_eq!(page.rect("m")[2], 800., "a mixed calc() falls back to auto");
        assert_eq!(page.rect("r"), [0., 10., 20., 10.]);
        assert!(page.engine.unsupported() >= 2);
    });
}

#[test]
fn gradients_the_shell_can_paint_stay_linear_and_the_rest_are_rasterised() {
    on_page(|| {
        let page = page(
            r#"<div id="lin" style="width:100px;height:50px;background-image:linear-gradient(90deg, red, blue)"></div>
               <div id="rad" style="width:100px;height:50px;background-image:radial-gradient(circle, red, blue)"></div>"#,
        );
        let fills = |id: &str| match boxes_of(&page, id).as_slice() {
            [PaintItem::Box { background, .. }] => background.clone(),
            other => panic!("{id}: {other:?}"),
        };
        assert_eq!(
            fills("lin"),
            vec![Fill::Linear {
                angle: 90.,
                stops: vec![([1., 0., 0., 1.], 0.), ([0., 0., 1., 1.], 1.)]
            }]
        );
        let radial = fills("rad");
        let [Fill::Raster(key)] = radial.as_slice() else {
            panic!("a radial gradient is rasterised");
        };
        let list = page.list();
        assert_eq!(list.rasters.len(), 1);
        assert_eq!(list.rasters[0].key, *key);
        assert_eq!(list.rasters[0].size, [100., 50.]);
    });
}

#[test]
fn an_outline_is_a_box_outside_the_border_box_that_takes_no_room() {
    on_page(|| {
        let page = page(
            r##"<div id="ol" style="width:100px;height:50px;outline:3px solid #f00;outline-offset:2px"></div>
                <div id="next" style="height:10px"></div>"##,
        );
        assert_eq!(page.rect("ol"), [0., 0., 100., 50.]);
        assert_eq!(page.rect("next")[1], 50.);
        let outline = boxes_of(&page, "ol");
        let [PaintItem::Box { bounds, border, .. }] = outline.as_slice() else {
            panic!("the outline is the only box item: {outline:?}");
        };
        assert_eq!(*bounds, Rect::new(-5., -5., 110., 60.));
        assert_eq!(border.widths, [3.; 4]);
        assert_eq!(border.colors[0], [1., 0., 0., 1.]);
    });
}

#[test]
fn replaced_elements_have_their_intrinsic_sizes() {
    on_page(|| {
        let page = page(
            r#"<canvas id="cv"></canvas><img id="im" width="40" height="20"><canvas id="c2" width="64"></canvas>"#,
        );
        assert_eq!(page.rect("cv")[2..], [300., 150.]);
        assert_eq!(page.rect("im")[2..], [40., 20.]);
        assert_eq!(page.rect("c2")[2..], [64., 150.]);
    });
}

#[test]
fn a_point_inside_the_window_that_nothing_covers_is_the_root_element() {
    on_page(|| {
        let page = page(r#"<div style="height:10px;width:10px"></div>"#);
        let hit = |x, y| page.engine.hit_test(x, y).map(|n| page.name(n));
        assert_eq!(hit(500., 500.).as_deref(), Some("page"));
        assert_eq!(hit(-1., 5.), None);
        assert_eq!(hit(5., 601.), None);
    });
}

#[test]
fn overflow_on_one_axis_clips_the_other_as_well() {
    on_page(|| {
        let page = page(
            r#"<div style="overflow-x:hidden;width:100px;height:100px"><div id="c" style="width:50px;height:300px"></div></div>"#,
        );
        let hitboxes = page.list().resolved_hitboxes(&|_| None);
        let c = hitboxes
            .iter()
            .find(|hitbox| hitbox.node == page.node("c"))
            .expect("a hitbox");
        assert_eq!(c.bounds, Rect::new(0., 0., 50., 100.));
    });
}

#[test]
fn lists_are_numbered_across_pages_so_a_new_page_never_repeats_the_number_of_an_old_one() {
    on_page(|| {
        // A navigation makes a new engine; a window that shows the old page's list tells the new
        // page's first list from it by its number.
        let first = page(r#"<div style="width:10px;height:10px"></div>"#).list();
        let second = page(r#"<div style="width:10px;height:10px"></div>"#).list();
        assert_ne!(first.revision, second.revision);
    });
}
