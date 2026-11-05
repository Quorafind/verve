// A window that shows `process.versions` from index.html, then quits. It runs under real Electron
// (where `app` exists) and under Verve before P10 gives it `app` (where the script just runs).
const { app, BrowserWindow } = require('electron');

const quit = () => (app ? app.quit() : process.exit(0));

function start() {
  const win = new BrowserWindow({ width: 800, height: 600, title: 'hello', show: false });
  win.once('ready-to-show', () => win.show());
  // The page lists what it is given, so it fills itself in from its query.
  win.loadFile('index.html', { query: process.versions });
  console.log('hello: window created');
  setTimeout(quit, 1000);
}

if (app) app.whenReady().then(start);
else start();
