// A table of what crosses contextBridge, value type by direction, printed by both worlds as
// console lines; the recording of real Electron is the expected output.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');

ipcMain.handle('echo', (event, value) => value);
ipcMain.on('done', () => app.quit());

app.whenReady().then(() => {
  const win = new BrowserWindow({
    show: false,
    webPreferences: { preload: path.join(__dirname, 'preload.js') },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  setTimeout(() => app.quit(), 15000);
});
