// P02's app: one window, one page, its text drawn. P00 turns this into the `hello` fixture that
// shows `process.versions` from an `index.html`.
const { BrowserWindow } = require('electron');

const window = new BrowserWindow({ width: 800, height: 600, title: 'Verve' });
window.loadURL('data:text/html,<h1>hi</h1>');
