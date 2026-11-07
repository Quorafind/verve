// A window of boxes with no text, 400 x 300 CSS pixels: the page P30 draws. Under real Electron, with
// VERVE_BOXES_PNG set to a path, it writes a picture of the window to it (`capturePage()` at a device
// scale factor of 1) and quits: that is how `expected-chromium.png` was made, with Electron 39.8.10 on
// Windows. Without it the window stays open; `electron --screenshot <path>` of Verve draws the same page.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');

if (app) app.commandLine.appendSwitch('force-device-scale-factor', '1');

function start() {
  const win = new BrowserWindow({ width: 400, height: 300, frame: false, resizable: false, show: !process.env.VERVE_BOXES_PNG, title: 'boxes' });
  win.loadFile('index.html');
  const path = process.env.VERVE_BOXES_PNG;
  if (!path) return;
  win.webContents.once('did-finish-load', () => {
    setTimeout(async () => {
      const image = await win.webContents.capturePage();
      fs.writeFileSync(path, image.toPNG());
      console.log('boxes: wrote', path, JSON.stringify(image.getSize()));
      app.quit();
    }, 500);
  });
}

if (app) app.whenReady().then(start);
else start();
