// The application menu, a context menu popped on right click, and a tray icon. The menu items
// log when clicked, so a test that clicks them (HeadlessShell::menu_click) can see them run.
// Run for real, this puts a tray icon in the notification area: it is for tests, not for a desktop.
const { app, BrowserWindow, Menu, Tray, nativeImage } = require('electron');
const path = require('node:path');

app.whenReady().then(() => {
  Menu.setApplicationMenu(
    Menu.buildFromTemplate([
      {
        label: 'File',
        submenu: [
          { label: 'Open', accelerator: 'CmdOrCtrl+O', click: () => console.log('menu: open') },
          { type: 'separator' },
          { role: 'quit' },
        ],
      },
      { label: 'Edit', submenu: [{ role: 'copy' }, { role: 'paste' }] },
    ]),
  );
  const win = new BrowserWindow({ show: false });
  const context = Menu.buildFromTemplate([{ label: 'Inspect', click: () => console.log('menu: inspect') }]);
  win.webContents.on('context-menu', () => context.popup({ window: win }));
  const tray = new Tray(nativeImage.createFromPath(path.join(__dirname, 'icon.png')));
  tray.setToolTip('menu-tray fixture');
  tray.setContextMenu(Menu.buildFromTemplate([{ label: 'Show', click: () => console.log('tray: show') }]));
  console.log('menu items', Menu.getApplicationMenu().items.length);
  setTimeout(() => app.quit(), 500);
});
