// A window of one of each form control, 480 x 270 CSS pixels: the page P32 draws. Under real
// Electron, with VERVE_FORMS_OUT set to a folder, it writes `expected-chromium.png` (`capturePage()`
// at a device scale factor of 1) and `expected-geometry.json` (each control's `getBoundingClientRect`)
// there and quits: that is how the files next to this one were made, with Electron 39.8.10 on Windows.
// Without it the window stays open; `electron --screenshot <path>` of Verve draws the same page.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

if (app) {
  app.commandLine.appendSwitch('force-device-scale-factor', '1');
  // The labels a control gets of its own ("Submit", "mm/dd/yyyy") and its monospace font follow
  // the machine's language: the geometry is the one of en-US, which is what Verve draws.
  app.commandLine.appendSwitch('lang', 'en-US');
}

function start() {
  const out = process.env.VERVE_FORMS_OUT;
  const win = new BrowserWindow({ width: 480, height: 270, frame: false, resizable: false, show: !out, useContentSize: true, title: 'forms' });
  win.loadFile('index.html');
  if (!out) return;
  win.webContents.once('did-finish-load', () => {
    setTimeout(async () => {
      const rects = await win.webContents.executeJavaScript(
        `Object.fromEntries([...document.querySelectorAll('[id]')].map((e) => { const r = e.getBoundingClientRect(); return [e.id, [r.x, r.y, r.width, r.height]]; }))`,
      );
      fs.writeFileSync(path.join(out, 'expected-geometry.json'), JSON.stringify(rects, null, 1));
      const image = await win.webContents.capturePage();
      fs.writeFileSync(path.join(out, 'expected-chromium.png'), image.toPNG());
      console.log('forms: wrote', out, JSON.stringify(image.getSize()));
      app.quit();
    }, 500);
  });
}

if (app) app.whenReady().then(start);
else start();
