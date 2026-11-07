// The lines of every block marked `data-m`, as the page's own geometry says them: for each block its
// border box and its lines. A line is the union of the boxes `Range.getClientRects()` gives for the
// text of the block that overlap vertically, so it is the same measurement in Chromium and in Verve.
// Chromium's answer is `expected-chromium.json`.
window.__lines = function () {
  const round = (n) => Math.round(n * 100) / 100;
  const out = [];
  for (const el of document.querySelectorAll('[data-m]')) {
    const range = document.createRange();
    range.selectNodeContents(el);
    const rects = [...range.getClientRects()]
      .filter((r) => r.width > 0 && r.height > 0)
      .sort((a, b) => a.top - b.top || a.left - b.left);
    const lines = [];
    for (const r of rects) {
      const last = lines[lines.length - 1];
      if (last && r.top < last.bottom - 1 && r.bottom > last.top + 1) {
        last.top = Math.min(last.top, r.top);
        last.bottom = Math.max(last.bottom, r.bottom);
      } else {
        lines.push({ top: r.top, bottom: r.bottom });
      }
    }
    const box = el.getBoundingClientRect();
    out.push({
      id: el.dataset.m,
      box: [round(box.x), round(box.y), round(box.width), round(box.height)],
      lines: lines.map((l) => [round(l.top), round(l.bottom)]),
    });
  }
  return out;
};
