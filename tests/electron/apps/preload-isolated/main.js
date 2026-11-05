// contextIsolation (the default) with a preload that exposes a small API through contextBridge.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');

ipcMain.handle('echo', (event, text) => `echo ${text}`);
ipcMain.on('done', () => app.quit());

app.whenReady().then(() => {
  const win = new BrowserWindow({
    show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js') },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  setTimeout(() => app.quit(), 8000);
});
