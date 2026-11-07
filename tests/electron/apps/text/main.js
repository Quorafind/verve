// A window of text, 640 x 900 CSS pixels: the page P31 draws with real fonts. Under real Electron,
// with VERVE_TEXT_JSON set to a path, it writes what `lines.js` measures to it (and, with
// VERVE_TEXT_PNG, a picture of the window, `capturePage()` at a device scale factor of 1) and quits:
// that is how `expected-chromium.json` and `expected-chromium.png` were made, with Electron 39.8.10
// on Windows. Without it the window stays open.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');

if (app) app.commandLine.appendSwitch('force-device-scale-factor', '1');

function start() {
  const json = process.env.VERVE_TEXT_JSON;
  const win = new BrowserWindow({ width: 640, height: 900, frame: false, resizable: false, show: !json, title: 'text' });
  win.loadFile('index.html');
  if (!json) return;
  win.webContents.once('did-finish-load', () => {
    setTimeout(async () => {
      const lines = await win.webContents.executeJavaScript('document.fonts.ready.then(() => window.__lines())');
      fs.writeFileSync(json, JSON.stringify(lines, null, 1));
      const png = process.env.VERVE_TEXT_PNG;
      if (png) fs.writeFileSync(png, (await win.webContents.capturePage()).toPNG());
      console.log('text: wrote', json, lines.length, 'blocks');
      app.quit();
    }, 500);
  });
}

if (app) app.whenReady().then(start);
else start();
