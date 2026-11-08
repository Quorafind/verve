// A window of pictures, 480 x 330 CSS pixels: the page P35 draws (a PNG, a JPEG, a WebP, an animated GIF
// that plays twice and stops, an SVG icon grid, SVG as <img> and as a background, background tiles, a
// canvas clock fixed at 10:08:30). Under real Electron, with VERVE_IMAGES_PNG set to a path, it writes a
// picture of the window to it (`capturePage()` at a device scale factor of 1) and quits: that is how
// `expected-chromium.png` was made, with Electron 39.8.10 on Windows. Without it the window stays open;
// `electron --screenshot <path>` of Verve draws the same page. `index.html` is made by `gen.js` (run it
// with Electron: `electron gen.js`), which draws the pictures in a canvas and inlines them.
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');

if (app) app.commandLine.appendSwitch('force-device-scale-factor', '1');

function start() {
  const win = new BrowserWindow({ width: 480, height: 330, frame: false, resizable: false, show: !process.env.VERVE_IMAGES_PNG, useContentSize: true, title: 'images' });
  win.loadFile('index.html');
  const path = process.env.VERVE_IMAGES_PNG;
  if (!path) return;
  win.webContents.once('did-finish-load', () => {
    // The GIF plays its two rounds in 600 ms and stays on its last frame.
    setTimeout(async () => {
      const image = await win.webContents.capturePage();
      fs.writeFileSync(path, image.toPNG());
      console.log('images: wrote', path, JSON.stringify(image.getSize()));
      app.quit();
    }, 2500);
  });
}

if (app) app.whenReady().then(start);
else start();
