// Probe pages for P30: each page is HTML with no text, ids on the boxes whose border boxes are
// frozen. `points` are hit-tested with elementFromPoint; `scrolls` are applied (scrollLeft/Top)
// and every rect is read a second time as `after`.
const reset = 'html,body{margin:0;padding:0}';
module.exports = [
  { name: 'block_flow_and_margins', html: `<style>${reset}</style>
<div id="a" style="height:50px;margin-bottom:20px"></div>
<div id="b" style="height:30px;margin-top:10px;margin-bottom:5px;padding:4px 6px;border:2px solid #000"></div>
<div id="c" style="margin:10px 0"><div id="c1" style="height:20px;margin-top:30px"></div></div>
<div id="d" style="box-sizing:border-box;width:50%;height:40px;padding:10px;margin:0 auto"></div>
<div id="e" style="width:100px;height:20px;margin-left:auto;margin-right:20px"></div>
<div id="f" style="width:200px;height:20px;min-width:300px;max-width:400px"></div>
<div id="g" style="height:10px;margin-top:-5px"></div>` },

  { name: 'flex_wrap_and_alignment', html: `<style>${reset}</style>
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
</div>` },

  { name: 'flex_grow_shrink_order', html: `<style>${reset}</style>
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
</div>` },

  { name: 'grid_columns_200px_1fr', html: `<style>${reset}</style>
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
</div>` },

  { name: 'grid_named_areas', html: `<style>${reset}</style>
<div id="layout" style="display:grid;width:500px;height:300px;grid-template-columns:100px 1fr;grid-template-rows:50px 1fr 40px;
  grid-template-areas:'head head' 'side main' 'foot foot'">
  <div id="head" style="grid-area:head"></div>
  <div id="side" style="grid-area:side"></div>
  <div id="main" style="grid-area:main"></div>
  <div id="foot" style="grid-area:foot"></div>
</div>` },

  { name: 'floats_and_clear', html: `<style>${reset}</style>
<div id="box" style="width:400px">
  <div id="fl" style="float:left;width:100px;height:50px"></div>
  <div id="fr" style="float:right;width:150px;height:80px"></div>
  <div id="fl2" style="float:left;width:100px;height:30px"></div>
  <div id="clr" style="clear:both;height:20px"></div>
  <div id="after" style="height:10px"></div>
</div>
<div id="contain" style="overflow:hidden;width:200px"><div id="fc" style="float:left;width:50px;height:70px"></div></div>` },

  { name: 'float_shrinks_to_fit_and_percent_heights_resolve', html: `<style>${reset} html,body{height:100%}</style>
<div id="shrink" style="float:left"><div id="inner40" style="width:40px;height:10px"></div></div>
<div id="clearer" style="clear:left;height:50%"></div>` },

  { name: 'absolute_containing_block', html: `<style>${reset}</style>
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
</div>` },

  { name: 'fixed_ignores_ancestors', html: `<style>${reset}</style>
<div id="holder" style="position:relative;left:50px;top:50px;width:100px;height:100px">
  <div id="fx1" style="position:fixed;top:10px;left:20px;width:30px;height:40px"></div>
  <div id="fx2" style="position:fixed;right:5px;bottom:5px;width:30px;height:40px"></div>
  <div id="fx3" style="position:fixed;top:0;left:0;right:0;height:25px"></div>
</div>` },

  { name: 'sticky_follows_scroll', html: `<style>${reset}</style>
<div id="scroller" style="overflow:auto;scrollbar-width:none;width:200px;height:200px">
  <div id="before" style="height:50px"></div>
  <div id="stick" style="position:sticky;top:10px;height:20px"></div>
  <div id="stick2" style="position:sticky;bottom:5px;height:20px"></div>
  <div id="tall" style="height:600px"></div>
</div>`, scrolls: { scroller: [0, 100] } },

  { name: 'display_contents_and_none', html: `<style>${reset}</style>
<div id="flex" style="display:flex;width:300px;height:40px;gap:5px">
  <div id="wrapper" style="display:contents">
    <div id="k1" style="width:50px;height:40px"></div>
    <div id="k2" style="width:60px;height:40px"></div>
  </div>
  <div id="gone" style="display:none;width:100px;height:40px"></div>
  <div id="k3" style="width:70px;height:40px"></div>
</div>
<div id="vis" style="visibility:hidden;width:90px;height:30px"></div>
<div id="blk" style="margin-top:10px"><div id="inner" style="display:contents"><div id="k4" style="height:15px"></div></div></div>` },

  { name: 'overflow_scroll_extent', html: `<style>${reset}</style>
<div id="clip" style="overflow:hidden;width:100px;height:100px">
  <div id="big" style="width:300px;height:250px"></div>
</div>
<div id="scr" style="overflow:scroll;scrollbar-width:none;width:100px;height:100px;margin-top:10px">
  <div id="inner" style="width:180px;height:260px"></div>
</div>`, scrolls: { clip: [40, 30], scr: [20, 50] }, extents: ['clip', 'scr'] },

  { name: 'z_index_paint_order', html: `<style>${reset}
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
</div>`, points: [[5, 5], [30, 30], [65, 65], [72, 72], [110, 110], [160, 160], [220, 20], [250, 190], [400, 400]] },

  { name: 'size_constraints_and_aspect_ratio', html: `<style>${reset}</style>
<div id="ar" style="width:200px;aspect-ratio:2/1"></div>
<div id="ar2" style="height:60px;width:auto;aspect-ratio:3/2;display:flex"></div>
<div id="mm" style="width:50%;min-width:500px;height:20px"></div>
<div id="cb" style="width:200px;height:50px;padding:10px;border:3px solid #000"></div>
<div id="bb" style="box-sizing:border-box;width:200px;height:50px;padding:10px;border:3px solid #000"></div>
<div id="pct" style="width:400px;height:100px"><div id="pc" style="width:25%;height:50%;margin-left:10%;padding:5%"></div></div>` },

  { name: 'transform_translate_moves_rect', html: `<style>${reset}</style>
<div id="tr" style="width:100px;height:50px;transform:translate(30px,20px)"></div>
<div id="tr2" style="width:100px;height:50px;transform:translateX(10px) translateY(5px)"></div>
<div id="tp" style="width:100px;height:50px;transform:translate(10%,20%)"></div>`, points: [[10, 10], [40, 30], [120, 50]] },

  { name: 'relative_offsets_and_static_insets', html: `<style>${reset}</style>
<div id="pr" style="position:relative;left:10px;top:5px;width:100px;height:20px"></div>
<div id="st" style="left:50px;top:50px;width:100px;height:20px"></div>
<div id="pn" style="position:relative;right:10px;bottom:5px;width:100px;height:20px"></div>
<div id="after" style="height:10px"></div>` },

  { name: 'table_grid_approximation', html: `<style>${reset} table{border-spacing:0;width:150px} td{padding:0}</style>
<table id="t"><tr><td id="a" style="width:100px;height:20px"></td><td id="b" style="width:50px;height:20px"></td></tr>
<tr><td id="c" colspan="2" style="height:30px"></td></tr></table>` },

  { name: 'scrolled_page_moves_flow_and_absolute_boxes_not_fixed_ones', html: `<style>${reset} html{scrollbar-width:none}</style>
<div id="top" style="height:100px"></div>
<div id="stk" style="position:sticky;top:20px;height:10px"></div>
<div id="mid" style="height:2000px"><div id="deep" style="margin-top:500px;height:10px"></div></div>
<div id="abs" style="position:absolute;top:50px;left:10px;width:20px;height:20px"></div>
<div id="fx" style="position:fixed;top:5px;left:5px;width:20px;height:20px"></div>`, scrolls: { html: [0, 300] } },

  { name: 'inline_children_of_flex_and_grid_are_blockified', html: `<style>${reset}</style>
<div id="fl" style="display:flex"><span id="sp" style="width:16px;height:16px"></span><span id="sp2" style="display:inline-block;width:10px;height:5px"></span></div>
<div id="gr" style="display:grid"><span id="gs" style="height:7px"></span></div>` },

  { name: 'table_border_spacing_runs_round_the_cells', html: `<style>${reset} td{padding:0}</style>
<table id="t" style="border-spacing:4px;width:162px"><tr><td id="a" style="width:100px;height:20px"></td><td id="b" style="width:50px;height:20px"></td></tr></table>` },

  { name: 'shadow_dom_flat_tree', html: `<style>${reset}</style><div id="host" style="width:300px"></div>`,
    script: `const host = document.getElementById('host'); const root = host.attachShadow({mode:'open'});
      root.innerHTML = '<style>:host{display:block}.in{height:30px;margin:5px}</style><div class="in" id="sh1"></div><slot></slot>';
      const lit = document.createElement('div'); lit.id = 'light'; lit.style.height = '20px'; host.append(lit);` },
];
