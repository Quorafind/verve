// What a window sends as it is shown, hidden, resized, maximized, minimized, put on full screen and
// closed: the order of its events, which the headless shell and the GPUI shell must both keep.
// Each step logs its name and then the events it caused.
const { app, BrowserWindow } = require('electron');

const EVENTS = [
  'show', 'hide', 'focus', 'blur', 'close', 'closed', 'resize', 'move', 'maximize', 'unmaximize',
  'minimize', 'restore', 'enter-full-screen', 'leave-full-screen',
];
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false });
  for (const name of EVENTS) win.on(name, () => console.log('window ' + name));
  await new Promise((resolve) => {
    win.webContents.once('did-finish-load', resolve);
    win.loadURL('data:text/html,<p>x</p>');
  });
  const steps = [
    ['show', () => win.show()],
    ['hide', () => win.hide()],
    ['showInactive', () => win.showInactive()],
    ['setBounds', () => win.setBounds({ x: 50, y: 60, width: 500, height: 400 })],
    ['maximize', () => win.maximize()],
    ['unmaximize', () => win.unmaximize()],
    ['minimize', () => win.minimize()],
    ['restore', () => win.restore()],
    ['blur', () => win.blur()],
    ['focus', () => win.focus()],
    ['setFullScreen(true)', () => win.setFullScreen(true)],
    ['setFullScreen(false)', () => win.setFullScreen(false)],
    ['close', () => win.close()],
  ];
  for (const [name, step] of steps) {
    console.log('-- ' + name);
    step();
    await wait(500);
  }
  app.quit();
});

// A fixture that hangs must still end.
setTimeout(() => app.exit(9), 20000);
