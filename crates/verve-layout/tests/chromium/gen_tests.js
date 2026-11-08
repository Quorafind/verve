// Writes ../chromium.rs from probes.js and the results main.js collected (PROBE_OUT, or
// chromium.json beside this file).
const fs = require('fs');
const path = require('path');
const probes = require('./probes.js');
const results = require(path.resolve(process.env.PROBE_OUT || path.join(__dirname, 'chromium.json')));

const target = process.argv[2] || path.resolve(__dirname, '../chromium.rs');

// Elements whose box Chromium reports as all zero because they have none.
const skip = {
  display_contents_and_none: ['wrapper', 'gone', 'inner'],
};
const fmt = (n) => {
  const v = Math.round(n * 1000) / 1000;
  return Number.isInteger(v) ? `${v}.` : `${v}`;
};
const rects = (map, name) =>
  Object.entries(map)
    .filter(([id]) => !(skip[name] || []).includes(id))
    .map(([id, r]) => `            ("${id}", [${r.map(fmt).join(', ')}]),`)
    .join('\n');

let code = `//! Boxes laid out against Chromium: each test is a page of boxes with no text, and the rectangles
//! are what \`getBoundingClientRect()\` answered for the same page in Electron ${results.electron}
//! (Chromium ${results.chrome}) on ${results.platform === 'win32' ? 'Windows' : results.platform}, in a hidden window of 800 by 600 CSS pixels at a device pixel ratio of 1.
//! The pages are set as \`document.body.innerHTML\`, in Electron and here, so both read the same
//! markup. \`scrollbar-width: none\` keeps the scrollbars' 15 pixels out of Chromium's numbers,
//! as the engine's scrollbars overlay and take no room.
//!
//! This file is written by \`tests/chromium/gen_tests.js\` from the pages in \`probes.js\` and what
//! \`main.js\` collected in Electron: each page is loaded, every element with an id is read, the
//! scrolls the page names are applied, and every element is read again. Do not edit the numbers
//! by hand; see \`tests/chromium/README.md\` to collect them again.

mod common;

use common::{lay_out, on_page};

`;

for (const probe of probes) {
  const r = results.probes[probe.name];
  const html = probe.html;
  if (html.includes('"###')) throw new Error('raw string clash in ' + probe.name);
  code += `#[test]\nfn ${probe.name}() {\n    on_page(|| {\n`;
  const script = probe.script ? `r###"${probe.script.replace(/\n\s*/g, ' ')}"###` : '""';
  code += `        let ${probe.scrolls ? 'mut ' : ''}page = lay_out(\n            r###"${html}"###,\n            ${script},\n        );\n`;
  code += `        page.expect_rects(&[\n${rects(r.rects, probe.name)}\n        ]);\n`;
  if (r.hits) {
    code += `        page.expect_hits(&[\n${r.hits.map(([x, y, id]) => `            (${fmt(x)}, ${fmt(y)}, "${id}"),`).join('\n')}\n        ]);\n`;
  }
  if (r.extents) {
    for (const [id, w, h] of r.extents) code += `        page.expect_extent("${id}", [${fmt(w)}, ${fmt(h)}]);\n`;
  }
  if (probe.scrolls) {
    for (const [id, [x, y]] of Object.entries(probe.scrolls)) code += `        page.scroll_to("${id}", ${fmt(x)}, ${fmt(y)});\n`;
    for (const [id, x, y] of r.scrolled) code += `        assert_eq!(page.engine.scroll_offset(page.node("${id}")), [${fmt(x)}, ${fmt(y)}]);\n`;
    code += `        page.expect_rects(&[\n${rects(r.after, probe.name)}\n        ]);\n`;
  }
  code += `    });\n}\n\n`;
}
fs.writeFileSync(target, code.trimEnd() + '\n');
console.log('wrote', target);
