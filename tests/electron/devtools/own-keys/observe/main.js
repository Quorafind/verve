// What the object of 'console-message' is in Electron 39.8.10 (Windows): its own keys and its frame. Run it with
//   electron --user-data-dir=<scratch> tests/electron/devtools/own-keys/observe
// (the window is never shown); the first line it prints, without the `P43 `, is expected/own-keys.txt.
const { app, BrowserWindow } = require('electron');
const path = require('path');
const out = [];
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false });
  const wc = win.webContents;
  let n = 0;
  wc.on('console-message', function (event, level, message) {
    if (n++ > 1) return;
    out.push(['typeof _level', typeof event._level, event._level, 'defaultPrevented', event.defaultPrevented, typeof event.preventDefault,
      'proto', Object.getPrototypeOf(event) === Object.prototype, 'own', Object.getOwnPropertyNames(event).sort(), 'frameIsMain', event.frame === wc.mainFrame, 'frame.url', event.frame && event.frame.url.split('/').pop()].map(x => JSON.stringify(x)).join(' '));
  });
  wc.loadFile(path.join(__dirname, 'page.html'));
  setTimeout(() => { for (const l of out) console.log('P43 ' + l); app.exit(0); }, 2000);
});
