// The application menu, a context menu popped on right click, a tray icon and a global shortcut.
// The menu items log when clicked, so a test that clicks them (HeadlessShell::menu_click) can see
// them run. On the headless shell that is all there is to it. Run for real (the `electron`
// binary, with no VERVE_NO_SYSTEM_DIALOGS in the environment) this puts a menu bar on the window,
// a tray icon in the notification area and Ctrl+Shift+X on the keyboard: that is for a person to
// try by hand, with MENU_TRAY_STAY=1 to keep the app up until it is quit from the tray menu.
const { app, BrowserWindow, Menu, Tray, globalShortcut, nativeImage } = require('electron');
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
  // Kept in globals: a Tray nothing refers to is collected, and its icon goes with it.
  const stay = process.env.MENU_TRAY_STAY === '1';
  const win = (global.win = new BrowserWindow({ show: stay }));
  const context = Menu.buildFromTemplate([{ label: 'Inspect', click: () => console.log('menu: inspect') }]);
  win.webContents.on('context-menu', () => context.popup({ window: win }));
  const tray = (global.tray = new Tray(nativeImage.createFromPath(path.join(__dirname, 'icon.png'))));
  tray.setToolTip('menu-tray fixture');
  tray.setContextMenu(Menu.buildFromTemplate([{ label: 'Show', click: () => console.log('tray: show') }]));
  console.log('menu items', Menu.getApplicationMenu().items.length);
  console.log('shortcut registered', globalShortcut.register('CommandOrControl+Shift+X', () => console.log('shortcut: pressed')));
  if (process.env.MENU_TRAY_STAY === '1') {
    tray.setContextMenu(Menu.buildFromTemplate([{ role: 'quit' }]));
  } else {
    setTimeout(() => app.quit(), 500);
  }
});
