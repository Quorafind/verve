// 2000 absolutely positioned boxes in an 800 x 600 window, for measuring how long Verve's GPUI window
// takes to replay a page of that size: `VERVE_FRAME_STATS=1 electron tests/electron/apps/boxes-2000`
// draws frames one after the other and logs, every 240, the median and the 95th percentile of how long
// a frame took to build. The app ends by itself after a few seconds.
const { app, BrowserWindow } = require('electron');

function start() {
  const win = new BrowserWindow({ width: 800, height: 600, frame: false, resizable: false, title: '2000 boxes' });
  win.loadFile('index.html');
  setTimeout(() => (app ? app.quit() : process.exit(0)), Number(process.env.VERVE_BOXES_MS) || 8000);
}

if (app) app.whenReady().then(start);
else start();
