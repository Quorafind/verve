// A window that shows `process.versions` from index.html, then quits. It runs under real Electron
// (where `app` exists) and under Verve before P10 gives it `app` (where the script just runs).
const { app, BrowserWindow } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

const quit = () => (app ? app.quit() : process.exit(0));

// P20: win.loadFile('index.html') once file: pages load. Until then the page is the same markup
// as a data: URL, with the versions filled in.
function page() {
  const items = Object.entries(process.versions)
    .map(([name, version]) => `<li>${name} ${version}</li>`)
    .join('');
  return fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8').replace('<!--versions-->', items);
}

function start() {
  const win = new BrowserWindow({ width: 800, height: 600, title: 'hello', show: false });
  win.once('ready-to-show', () => win.show());
  win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(page()));
  console.log('hello: window created');
  setTimeout(quit, 1000);
}

if (app) app.whenReady().then(start);
else start();
