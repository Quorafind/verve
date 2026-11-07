// Runs probes.js in Electron 39.8.10: each page at 800x600 CSS px, DPR 1, window hidden.
const { app, BrowserWindow } = require('electron');
const fs = require('fs');
const path = require('path');
const probes = require('./probes.js');
app.commandLine.appendSwitch('force-device-scale-factor', '1');
app.disableHardwareAcceleration();

const collect = `(() => {
  const out = {};
  const walk = (root) => root.querySelectorAll('[id]').forEach((el) => {
    const r = el.getBoundingClientRect();
    out[el.id] = [r.x, r.y, r.width, r.height];
  });
  walk(document);
  for (const el of document.querySelectorAll('*')) if (el.shadowRoot) walk(el.shadowRoot);
  return out;
})()`;

app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, useContentSize: true, width: 800, height: 600,
    webPreferences: { sandbox: true, backgroundThrottling: false } });
  const results = { electron: process.versions.electron, chrome: process.versions.chrome, platform: process.platform, probes: {} };
  for (const p of probes) {
    await win.loadURL('data:text/html,' + encodeURIComponent('<!doctype html><html><head></head><body></body></html>'));
    const size = await win.webContents.executeJavaScript('[innerWidth, innerHeight, devicePixelRatio]');
    const setup = `document.body.innerHTML = ${JSON.stringify(p.html)};` + (p.script ? p.script : '');
    await win.webContents.executeJavaScript(setup);
    const entry = { viewport: size };
    entry.rects = await win.webContents.executeJavaScript(collect);
    if (p.points) entry.hits = await win.webContents.executeJavaScript(
      `${JSON.stringify(p.points)}.map(([x, y]) => { const e = document.elementFromPoint(x, y); return [x, y, e ? (e.id || e.tagName.toLowerCase()) : null]; })`);
    if (p.scrolls) {
      await win.webContents.executeJavaScript(
        `Object.entries(${JSON.stringify(p.scrolls)}).forEach(([id, [x, y]]) => { const e = id === 'html' ? document.documentElement : document.getElementById(id); e.scrollLeft = x; e.scrollTop = y; })`);
      entry.after = await win.webContents.executeJavaScript(collect);
      entry.scrolled = await win.webContents.executeJavaScript(
        `Object.keys(${JSON.stringify(p.scrolls)}).map((id) => { const e = id === 'html' ? document.documentElement : document.getElementById(id); return [id, e.scrollLeft, e.scrollTop]; })`);
    }
    if (p.extents) entry.extents = await win.webContents.executeJavaScript(
      `${JSON.stringify(p.extents)}.map((id) => { const e = document.getElementById(id); return [id, e.scrollWidth, e.scrollHeight, e.clientWidth, e.clientHeight]; })`);
    results.probes[p.name] = entry;
  }
  fs.writeFileSync(process.env.PROBE_OUT || path.join(__dirname, 'chromium.json'), JSON.stringify(results, null, 1));
  app.exit(0);
});
