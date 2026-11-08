// nodeIntegration with no context isolation: the page and its preload share one world, and both
// can `require` Node's modules.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');

ipcMain.on('done', () => app.quit());

app.whenReady().then(() => {
  const win = new BrowserWindow({
    show: false,
    webPreferences: {
      nodeIntegration: true,
      contextIsolation: false,
      preload: path.join(__dirname, 'preload.js'),
    },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  setTimeout(() => app.quit(), 8000);
});
