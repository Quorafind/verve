// 2000 paragraphs of mixed Latin and Chinese text with inline elements in an 800 x 600 window, for
// measuring how long Verve lays out and draws a text-heavy page:
// `VERVE_FRAME_STATS=1 electron tests/electron/apps/text-2000`. It is the text counterpart of
// boxes-2000. The app ends by itself after a few seconds.
const { app, BrowserWindow } = require('electron');

function start() {
  const win = new BrowserWindow({ width: 800, height: 600, frame: false, resizable: false, title: '2000 paragraphs' });
  win.loadFile('index.html');
  setTimeout(() => (app ? app.quit() : process.exit(0)), Number(process.env.VERVE_TEXT_MS) || 8000);
}

if (app) app.whenReady().then(start);
else start();
