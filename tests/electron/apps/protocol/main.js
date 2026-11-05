// A whole site served from a custom scheme: protocol.handle('app', ...) answers every request
// from site/, and the page loads its own script, style and JSON through it.
const { app, BrowserWindow, protocol, net } = require('electron');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

protocol.registerSchemesAsPrivileged([
  { scheme: 'app', privileges: { standard: true, secure: true, supportFetchAPI: true } },
]);

app.whenReady().then(() => {
  protocol.handle('app', (request) => {
    const file = path.join(__dirname, 'site', new URL(request.url).pathname);
    return net.fetch(pathToFileURL(file).toString());
  });
  const win = new BrowserWindow({ show: false });
  win.webContents.on('console-message', (event) => console.log('page:', event.message));
  win.webContents.once('did-finish-load', () => setTimeout(() => app.quit(), 500));
  win.loadURL('app://bundle/index.html');
  setTimeout(() => app.quit(), 8000);
});
