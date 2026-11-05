// Every way a page and the main process talk: send/on, invoke/handle, sendSync, reply,
// webContents.send, and an error thrown by a handler. The page (renderer.js) drives the order.
const { app, BrowserWindow, ipcMain } = require('electron');
const path = require('node:path');

ipcMain.on('ping', (event, a, b) => {
  console.log('main: ping', a, b);
  event.reply('pong', a + b);
});
ipcMain.handle('add', async (event, a, b) => a + b);
ipcMain.handle('fail', async () => {
  throw new Error('boom');
});
ipcMain.on('sync', (event, x) => {
  console.log('main: sync', x);
  event.returnValue = x * 2;
});
ipcMain.on('done', () => app.quit());

app.whenReady().then(() => {
  const win = new BrowserWindow({
    show: false,
    webPreferences: { nodeIntegration: true, contextIsolation: false },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
  win.webContents.on('did-finish-load', () => win.webContents.send('hello-from-main', { n: 1 }));
  // A fixture that hangs must still end.
  setTimeout(() => app.quit(), 8000);
});
