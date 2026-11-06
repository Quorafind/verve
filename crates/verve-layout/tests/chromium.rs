//! Boxes laid out against Chromium: each test is a page of boxes with no text, and the rectangles
//! are what `getBoundingClientRect()` answered for the same page in Electron 39.8.10
//! (Chromium 142.0.7444.265) on Windows, in a hidden window of 800 by 600 CSS pixels at a device pixel ratio of 1.
//! The pages are set as `document.body.innerHTML`, in Electron and here, so both read the same
//! markup. `scrollbar-width: none` keeps the scrollbars' 15 pixels out of Chromium's numbers,
//! as the engine's scrollbars overlay and take no room.
//!
//! This file is written by `tests/chromium/gen_tests.js` from the pages in `probes.js` and what
//! `main.js` collected in Electron: each page is loaded, every element with an id is read, the
//! scrolls the page names are applied, and every element is read again. Do not edit the numbers
//! by hand; see `tests/chromium/README.md` to collect them again.

mod common;

use common::{lay_out, on_page};

#[test]
fn block_flow_and_margins() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="a" style="height:50px;margin-bottom:20px"></div>
<div id="b" style="height:30px;margin-top:10px;margin-bottom:5px;padding:4px 6px;border:2px solid #000"></div>
<div id="c" style="margin:10px 0"><div id="c1" style="height:20px;margin-top:30px"></div></div>
<div id="d" style="box-sizing:border-box;width:50%;height:40px;padding:10px;margin:0 auto"></div>
<div id="e" style="width:100px;height:20px;margin-left:auto;margin-right:20px"></div>
<div id="f" style="width:200px;height:20px;min-width:300px;max-width:400px"></div>
<div id="g" style="height:10px;margin-top:-5px"></div>"###,
            "",
        );
        page.expect_rects(&[
            ("a", [0., 0., 800., 50.]),
            ("b", [0., 70., 800., 42.]),
            ("c", [0., 142., 800., 20.]),
            ("c1", [0., 142., 800., 20.]),
            ("d", [200., 172., 400., 40.]),
            ("e", [680., 212., 100., 20.]),
            ("f", [0., 232., 300., 20.]),
            ("g", [0., 247., 800., 10.]),
        ]);
    });
}

#[test]
fn flex_wrap_and_alignment() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="wrap" style="display:flex;flex-wrap:wrap;width:300px;height:200px;gap:10px 20px;align-content:flex-start">
  <div id="w1" style="width:100px;height:40px"></div><div id="w2" style="width:100px;height:60px"></div>
  <div id="w3" style="width:100px;height:40px"></div><div id="w4" style="width:100px;height:40px"></div>
</div>
<div id="between" style="display:flex;width:300px;height:50px;justify-content:space-between;align-items:center">
  <div id="b1" style="width:50px;height:10px"></div><div id="b2" style="width:50px;height:20px"></div><div id="b3" style="width:50px;height:30px"></div>
</div>
<div id="col" style="display:flex;flex-direction:column;width:200px;height:300px;align-items:flex-end;justify-content:center">
  <div id="c1" style="width:80px;height:40px"></div><div id="c2" style="width:120px;height:40px"></div>
</div>
<div id="rev" style="display:flex;flex-direction:row-reverse;width:300px;height:30px">
  <div id="r1" style="width:50px;height:30px"></div><div id="r2" style="width:70px;height:30px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("wrap", [0., 0., 300., 200.]),
            ("w1", [0., 0., 100., 40.]),
            ("w2", [120., 0., 100., 60.]),
            ("w3", [0., 70., 100., 40.]),
            ("w4", [120., 70., 100., 40.]),
            ("between", [0., 200., 300., 50.]),
            ("b1", [0., 220., 50., 10.]),
            ("b2", [125., 215., 50., 20.]),
            ("b3", [250., 210., 50., 30.]),
            ("col", [0., 250., 200., 300.]),
            ("c1", [120., 360., 80., 40.]),
            ("c2", [80., 400., 120., 40.]),
            ("rev", [0., 550., 300., 30.]),
            ("r1", [250., 550., 50., 30.]),
            ("r2", [180., 550., 70., 30.]),
        ]);
    });
}

#[test]
fn flex_grow_shrink_order() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="row" style="display:flex;width:400px;height:50px">
  <div id="g1" style="flex:1;height:50px"></div>
  <div id="g2" style="flex:2 1 0;height:50px"></div>
  <div id="g3" style="flex:none;width:100px;height:50px"></div>
</div>
<div id="shr" style="display:flex;width:200px;height:20px">
  <div id="s1" style="flex-shrink:1;width:150px;height:20px"></div>
  <div id="s2" style="flex-shrink:3;width:150px;height:20px"></div>
</div>
<div id="ord" style="display:flex;width:300px;height:20px">
  <div id="o1" style="order:3;width:50px;height:20px"></div>
  <div id="o2" style="order:1;width:60px;height:20px"></div>
  <div id="o3" style="width:70px;height:20px"></div>
</div>
<div id="stretch" style="display:flex;width:300px;height:80px">
  <div id="t1" style="width:40px"></div><div id="t2" style="width:40px;height:30px;align-self:center"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("row", [0., 0., 400., 50.]),
            ("g1", [0., 0., 100., 50.]),
            ("g2", [100., 0., 200., 50.]),
            ("g3", [300., 0., 100., 50.]),
            ("shr", [0., 50., 200., 20.]),
            ("s1", [0., 50., 125., 20.]),
            ("s2", [125., 50., 75., 20.]),
            ("ord", [0., 70., 300., 20.]),
            ("o1", [130., 70., 50., 20.]),
            ("o2", [70., 70., 60., 20.]),
            ("o3", [0., 70., 70., 20.]),
            ("stretch", [0., 90., 300., 80.]),
            ("t1", [0., 90., 40., 80.]),
            ("t2", [40., 115., 40., 30.]),
        ]);
    });
}

#[test]
fn grid_columns_200px_1fr() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="grid" style="display:grid;grid-template-columns:200px 1fr;grid-template-rows:50px auto;gap:10px;width:600px">
  <div id="i1" style="height:50px"></div>
  <div id="i2" style="height:50px"></div>
  <div id="i3" style="height:30px"></div>
  <div id="i4" style="height:40px"></div>
</div>
<div id="span" style="display:grid;grid-template-columns:repeat(3,100px);grid-auto-rows:20px;width:400px">
  <div id="p1" style="grid-column:span 2"></div>
  <div id="p2"></div>
  <div id="p3" style="grid-column:2 / 4;grid-row:2"></div>
  <div id="p4" style="grid-column:1;grid-row:2"></div>
</div>
<div id="frs" style="display:grid;grid-template-columns:1fr 2fr 100px;width:700px">
  <div id="f1" style="height:10px"></div><div id="f2" style="height:10px"></div><div id="f3" style="height:10px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("grid", [0., 0., 600., 100.]),
            ("i1", [0., 0., 200., 50.]),
            ("i2", [210., 0., 390., 50.]),
            ("i3", [0., 60., 200., 30.]),
            ("i4", [210., 60., 390., 40.]),
            ("span", [0., 100., 400., 40.]),
            ("p1", [0., 100., 200., 20.]),
            ("p2", [200., 100., 100., 20.]),
            ("p3", [100., 120., 200., 20.]),
            ("p4", [0., 120., 100., 20.]),
            ("frs", [0., 140., 700., 10.]),
            ("f1", [0., 140., 200., 10.]),
            ("f2", [200., 140., 400., 10.]),
            ("f3", [600., 140., 100., 10.]),
        ]);
    });
}

#[test]
fn grid_named_areas() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="layout" style="display:grid;width:500px;height:300px;grid-template-columns:100px 1fr;grid-template-rows:50px 1fr 40px;
  grid-template-areas:'head head' 'side main' 'foot foot'">
  <div id="head" style="grid-area:head"></div>
  <div id="side" style="grid-area:side"></div>
  <div id="main" style="grid-area:main"></div>
  <div id="foot" style="grid-area:foot"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("layout", [0., 0., 500., 300.]),
            ("head", [0., 0., 500., 50.]),
            ("side", [0., 50., 100., 210.]),
            ("main", [100., 50., 400., 210.]),
            ("foot", [0., 260., 500., 40.]),
        ]);
    });
}

#[test]
fn floats_and_clear() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="box" style="width:400px">
  <div id="fl" style="float:left;width:100px;height:50px"></div>
  <div id="fr" style="float:right;width:150px;height:80px"></div>
  <div id="fl2" style="float:left;width:100px;height:30px"></div>
  <div id="clr" style="clear:both;height:20px"></div>
  <div id="after" style="height:10px"></div>
</div>
<div id="contain" style="overflow:hidden;width:200px"><div id="fc" style="float:left;width:50px;height:70px"></div></div>"###,
            "",
        );
        page.expect_rects(&[
            ("box", [0., 0., 400., 110.]),
            ("fl", [0., 0., 100., 50.]),
            ("fr", [250., 0., 150., 80.]),
            ("fl2", [100., 0., 100., 30.]),
            ("clr", [0., 80., 400., 20.]),
            ("after", [0., 100., 400., 10.]),
            ("contain", [0., 110., 200., 70.]),
            ("fc", [0., 110., 50., 70.]),
        ]);
    });
}

#[test]
fn float_shrinks_to_fit_and_percent_heights_resolve() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0} html,body{height:100%}</style>
<div id="shrink" style="float:left"><div id="inner40" style="width:40px;height:10px"></div></div>
<div id="clearer" style="clear:left;height:50%"></div>"###,
            "",
        );
        page.expect_rects(&[
            ("shrink", [0., 0., 40., 10.]),
            ("inner40", [0., 0., 40., 10.]),
            ("clearer", [0., 10., 800., 300.]),
        ]);
    });
}

#[test]
fn absolute_containing_block() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="rel" style="position:relative;left:20px;top:30px;width:300px;height:200px;padding:10px;border:5px solid #000">
  <div id="ab1" style="position:absolute;top:10px;left:15px;width:50px;height:40px"></div>
  <div id="ab2" style="position:absolute;right:10px;bottom:20px;width:60px;height:30px"></div>
  <div id="ab3" style="position:absolute;top:0;left:0;right:0;bottom:0"></div>
  <div id="ab4" style="position:absolute;left:10%;top:25%;width:20%;height:10%"></div>
  <div id="mid" style="margin-top:50px;height:60px">
    <div id="ab5" style="position:absolute;top:5px;left:6px;width:10px;height:10px"></div>
  </div>
</div>
<div id="plain" style="width:100px;height:100px;margin-top:20px">
  <div id="ab6" style="position:absolute;top:7px;left:9px;width:10px;height:10px"></div>
</div>
<div id="stat" style="margin-left:40px;padding-top:25px;height:80px">
  <div id="ab7" style="position:absolute;width:30px;height:30px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("rel", [20., 30., 330., 230.]),
            ("ab1", [40., 45., 50., 40.]),
            ("ab2", [275., 205., 60., 30.]),
            ("ab3", [25., 35., 320., 220.]),
            ("ab4", [57., 90., 64., 22.]),
            ("mid", [35., 95., 300., 60.]),
            ("ab5", [31., 40., 10., 10.]),
            ("plain", [0., 250., 100., 100.]),
            ("ab6", [9., 7., 10., 10.]),
            ("stat", [40., 350., 760., 105.]),
            ("ab7", [40., 375., 30., 30.]),
        ]);
    });
}

#[test]
fn fixed_ignores_ancestors() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="holder" style="position:relative;left:50px;top:50px;width:100px;height:100px">
  <div id="fx1" style="position:fixed;top:10px;left:20px;width:30px;height:40px"></div>
  <div id="fx2" style="position:fixed;right:5px;bottom:5px;width:30px;height:40px"></div>
  <div id="fx3" style="position:fixed;top:0;left:0;right:0;height:25px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("holder", [50., 50., 100., 100.]),
            ("fx1", [20., 10., 30., 40.]),
            ("fx2", [765., 555., 30., 40.]),
            ("fx3", [0., 0., 800., 25.]),
        ]);
    });
}

#[test]
fn sticky_follows_scroll() {
    on_page(|| {
        let mut page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="scroller" style="overflow:auto;scrollbar-width:none;width:200px;height:200px">
  <div id="before" style="height:50px"></div>
  <div id="stick" style="position:sticky;top:10px;height:20px"></div>
  <div id="stick2" style="position:sticky;bottom:5px;height:20px"></div>
  <div id="tall" style="height:600px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("scroller", [0., 0., 200., 200.]),
            ("before", [0., 0., 200., 50.]),
            ("stick", [0., 50., 200., 20.]),
            ("stick2", [0., 70., 200., 20.]),
            ("tall", [0., 90., 200., 600.]),
        ]);
        page.scroll_to("scroller", 0., 100.);
        assert_eq!(page.engine.scroll_offset(page.node("scroller")), [0., 100.]);
        page.expect_rects(&[
            ("scroller", [0., 0., 200., 200.]),
            ("before", [0., -100., 200., 50.]),
            ("stick", [0., 10., 200., 20.]),
            ("stick2", [0., -30., 200., 20.]),
            ("tall", [0., -10., 200., 600.]),
        ]);
    });
}

#[test]
fn display_contents_and_none() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="flex" style="display:flex;width:300px;height:40px;gap:5px">
  <div id="wrapper" style="display:contents">
    <div id="k1" style="width:50px;height:40px"></div>
    <div id="k2" style="width:60px;height:40px"></div>
  </div>
  <div id="gone" style="display:none;width:100px;height:40px"></div>
  <div id="k3" style="width:70px;height:40px"></div>
</div>
<div id="vis" style="visibility:hidden;width:90px;height:30px"></div>
<div id="blk" style="margin-top:10px"><div id="inner" style="display:contents"><div id="k4" style="height:15px"></div></div></div>"###,
            "",
        );
        page.expect_rects(&[
            ("flex", [0., 0., 300., 40.]),
            ("k1", [0., 0., 50., 40.]),
            ("k2", [55., 0., 60., 40.]),
            ("k3", [120., 0., 70., 40.]),
            ("vis", [0., 40., 90., 30.]),
            ("blk", [0., 80., 800., 15.]),
            ("k4", [0., 80., 800., 15.]),
        ]);
    });
}

#[test]
fn overflow_scroll_extent() {
    on_page(|| {
        let mut page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="clip" style="overflow:hidden;width:100px;height:100px">
  <div id="big" style="width:300px;height:250px"></div>
</div>
<div id="scr" style="overflow:scroll;scrollbar-width:none;width:100px;height:100px;margin-top:10px">
  <div id="inner" style="width:180px;height:260px"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("clip", [0., 0., 100., 100.]),
            ("big", [0., 0., 300., 250.]),
            ("scr", [0., 110., 100., 100.]),
            ("inner", [0., 110., 180., 260.]),
        ]);
        page.expect_extent("clip", [300., 250.]);
        page.expect_extent("scr", [180., 260.]);
        page.scroll_to("clip", 40., 30.);
        page.scroll_to("scr", 20., 50.);
        assert_eq!(page.engine.scroll_offset(page.node("clip")), [40., 30.]);
        assert_eq!(page.engine.scroll_offset(page.node("scr")), [20., 50.]);
        page.expect_rects(&[
            ("clip", [0., 0., 100., 100.]),
            ("big", [-40., -30., 300., 250.]),
            ("scr", [0., 110., 100., 100.]),
            ("inner", [-20., 60., 180., 260.]),
        ]);
    });
}

#[test]
fn z_index_paint_order() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}
.s{position:absolute;width:100px;height:100px}</style>
<div id="root" style="position:relative;width:300px;height:200px">
  <div id="neg" class="s" style="z-index:-1;left:0;top:0"></div>
  <div id="flow" style="width:300px;height:200px"></div>
  <div id="fl" style="float:left;width:150px;height:150px"></div>
  <div id="p0" class="s" style="left:20px;top:20px"></div>
  <div id="p1" class="s" style="z-index:1;left:60px;top:60px"></div>
  <div id="p2" class="s" style="z-index:2;left:100px;top:100px"></div>
  <div id="p1b" class="s" style="z-index:1;left:70px;top:70px"></div>
  <div id="op" style="opacity:0.5;width:50px;height:50px;position:absolute;left:200px;top:0"></div>
</div>"###,
            "",
        );
        page.expect_rects(&[
            ("root", [0., 0., 300., 200.]),
            ("neg", [0., 0., 100., 100.]),
            ("flow", [0., 0., 300., 200.]),
            ("fl", [0., 200., 150., 150.]),
            ("p0", [20., 20., 100., 100.]),
            ("p1", [60., 60., 100., 100.]),
            ("p2", [100., 100., 100., 100.]),
            ("p1b", [70., 70., 100., 100.]),
            ("op", [200., 0., 50., 50.]),
        ]);
        page.expect_hits(&[
            (5., 5., "flow"),
            (30., 30., "p0"),
            (65., 65., "p1"),
            (72., 72., "p1b"),
            (110., 110., "p2"),
            (160., 160., "p2"),
            (220., 20., "op"),
            (250., 190., "flow"),
            (400., 400., "html"),
        ]);
    });
}

#[test]
fn size_constraints_and_aspect_ratio() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="ar" style="width:200px;aspect-ratio:2/1"></div>
<div id="ar2" style="height:60px;width:auto;aspect-ratio:3/2;display:flex"></div>
<div id="mm" style="width:50%;min-width:500px;height:20px"></div>
<div id="cb" style="width:200px;height:50px;padding:10px;border:3px solid #000"></div>
<div id="bb" style="box-sizing:border-box;width:200px;height:50px;padding:10px;border:3px solid #000"></div>
<div id="pct" style="width:400px;height:100px"><div id="pc" style="width:25%;height:50%;margin-left:10%;padding:5%"></div></div>"###,
            "",
        );
        page.expect_rects(&[
            ("ar", [0., 0., 200., 100.]),
            ("ar2", [0., 100., 90., 60.]),
            ("mm", [0., 160., 500., 20.]),
            ("cb", [0., 180., 226., 76.]),
            ("bb", [0., 256., 200., 50.]),
            ("pct", [0., 306., 400., 100.]),
            ("pc", [40., 306., 140., 90.]),
        ]);
    });
}

#[test]
fn transform_translate_moves_rect() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="tr" style="width:100px;height:50px;transform:translate(30px,20px)"></div>
<div id="tr2" style="width:100px;height:50px;transform:translateX(10px) translateY(5px)"></div>
<div id="tp" style="width:100px;height:50px;transform:translate(10%,20%)"></div>"###,
            "",
        );
        page.expect_rects(&[
            ("tr", [30., 20., 100., 50.]),
            ("tr2", [10., 55., 100., 50.]),
            ("tp", [10., 110., 100., 50.]),
        ]);
        page.expect_hits(&[(10., 10., "body"), (40., 30., "tr"), (120., 50., "tr")]);
    });
}

#[test]
fn relative_offsets_and_static_insets() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="pr" style="position:relative;left:10px;top:5px;width:100px;height:20px"></div>
<div id="st" style="left:50px;top:50px;width:100px;height:20px"></div>
<div id="pn" style="position:relative;right:10px;bottom:5px;width:100px;height:20px"></div>
<div id="after" style="height:10px"></div>"###,
            "",
        );
        page.expect_rects(&[
            ("pr", [10., 5., 100., 20.]),
            ("st", [0., 20., 100., 20.]),
            ("pn", [-10., 35., 100., 20.]),
            ("after", [0., 60., 800., 10.]),
        ]);
    });
}

#[test]
fn table_grid_approximation() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0} table{border-spacing:0;width:150px} td{padding:0}</style>
<table id="t"><tr><td id="a" style="width:100px;height:20px"></td><td id="b" style="width:50px;height:20px"></td></tr>
<tr><td id="c" colspan="2" style="height:30px"></td></tr></table>"###,
            "",
        );
        page.expect_rects(&[
            ("t", [0., 0., 150., 50.]),
            ("a", [0., 0., 100., 20.]),
            ("b", [100., 0., 50., 20.]),
            ("c", [0., 20., 150., 30.]),
        ]);
    });
}

#[test]
fn scrolled_page_moves_flow_and_absolute_boxes_not_fixed_ones() {
    on_page(|| {
        let mut page = lay_out(
            r###"<style>html,body{margin:0;padding:0} html{scrollbar-width:none}</style>
<div id="top" style="height:100px"></div>
<div id="stk" style="position:sticky;top:20px;height:10px"></div>
<div id="mid" style="height:2000px"><div id="deep" style="margin-top:500px;height:10px"></div></div>
<div id="abs" style="position:absolute;top:50px;left:10px;width:20px;height:20px"></div>
<div id="fx" style="position:fixed;top:5px;left:5px;width:20px;height:20px"></div>"###,
            "",
        );
        page.expect_rects(&[
            ("top", [0., 0., 800., 100.]),
            ("stk", [0., 100., 800., 10.]),
            ("mid", [0., 610., 800., 2000.]),
            ("deep", [0., 610., 800., 10.]),
            ("abs", [10., 50., 20., 20.]),
            ("fx", [5., 5., 20., 20.]),
        ]);
        page.scroll_to("html", 0., 300.);
        assert_eq!(page.engine.scroll_offset(page.node("html")), [0., 300.]);
        page.expect_rects(&[
            ("top", [0., -300., 800., 100.]),
            ("stk", [0., 20., 800., 10.]),
            ("mid", [0., 310., 800., 2000.]),
            ("deep", [0., 310., 800., 10.]),
            ("abs", [10., -250., 20., 20.]),
            ("fx", [5., 5., 20., 20.]),
        ]);
    });
}

#[test]
fn inline_children_of_flex_and_grid_are_blockified() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style>
<div id="fl" style="display:flex"><span id="sp" style="width:16px;height:16px"></span><span id="sp2" style="display:inline-block;width:10px;height:5px"></span></div>
<div id="gr" style="display:grid"><span id="gs" style="height:7px"></span></div>"###,
            "",
        );
        page.expect_rects(&[
            ("fl", [0., 0., 800., 16.]),
            ("sp", [0., 0., 16., 16.]),
            ("sp2", [16., 0., 10., 5.]),
            ("gr", [0., 16., 800., 7.]),
            ("gs", [0., 16., 800., 7.]),
        ]);
    });
}

#[test]
fn table_border_spacing_runs_round_the_cells() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0} td{padding:0}</style>
<table id="t" style="border-spacing:4px;width:162px"><tr><td id="a" style="width:100px;height:20px"></td><td id="b" style="width:50px;height:20px"></td></tr></table>"###,
            "",
        );
        page.expect_rects(&[
            ("t", [0., 0., 162., 28.]),
            ("a", [4., 4., 100., 20.]),
            ("b", [108., 4., 50., 20.]),
        ]);
    });
}

#[test]
fn shadow_dom_flat_tree() {
    on_page(|| {
        let page = lay_out(
            r###"<style>html,body{margin:0;padding:0}</style><div id="host" style="width:300px"></div>"###,
            r###"const host = document.getElementById('host'); const root = host.attachShadow({mode:'open'}); root.innerHTML = '<style>:host{display:block}.in{height:30px;margin:5px}</style><div class="in" id="sh1"></div><slot></slot>'; const lit = document.createElement('div'); lit.id = 'light'; lit.style.height = '20px'; host.append(lit);"###,
        );
        page.expect_rects(&[
            ("host", [0., 5., 300., 55.]),
            ("light", [0., 40., 300., 20.]),
            ("sh1", [5., 5., 290., 30.]),
        ]);
    });
}
