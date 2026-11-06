// The manual checklist of P14 (docs/electron/packages/P14-system-a.md, 「状态」): a window whose
// buttons each call one system module, and write what came back under the button. It opens
// real dialogs, a real notification, the browser and Explorer, so it is run by a person:
//
//   electron tests/electron/apps/system-a
//
// Nothing runs it automatically.
const { app, BrowserWindow, dialog, shell, clipboard, nativeTheme, screen, nativeImage, Notification, powerSaveBlocker, safeStorage, ipcMain } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

let win;

const actions = {
  'open-file': () => dialog.showOpenDialog(win, { properties: ['openFile', 'multiSelections'], filters: [{ name: 'Text', extensions: ['txt'] }, { name: 'All', extensions: ['*'] }] }),
  'open-directory': () => dialog.showOpenDialog(win, { properties: ['openDirectory'] }),
  'save-file': () => dialog.showSaveDialog(win, { defaultPath: path.join(os.homedir(), 'verve-p14.txt') }),
  'message-box': () => dialog.showMessageBox(win, { type: 'question', message: 'Save the changes?', detail: 'They will be lost otherwise.', buttons: ['Save', 'Cancel', 'Discard'], defaultId: 0, cancelId: 1, checkboxLabel: 'Remember' }),
  'message-box-sync': () => dialog.showMessageBoxSync(win, { type: 'warning', message: 'This stops the main process until you answer.', buttons: ['OK', 'Cancel'] }),
  'error-box': () => dialog.showErrorBox('P14', 'An error box.'),
  notification: () => {
    const notification = new Notification({ title: 'P14', body: 'A notification.' });
    notification.show();
    return { supported: Notification.isSupported() };
  },
  'open-external': () => shell.openExternal('https://example.com/'),
  'show-item': () => { shell.showItemInFolder(__filename); return 'asked Explorer'; },
  'trash-item': async () => {
    const file = path.join(os.tmpdir(), `verve-p14-${process.pid}.txt`);
    fs.writeFileSync(file, 'trash me');
    await shell.trashItem(file);
    return { trashed: !fs.existsSync(file), file };
  },
  clipboard: () => {
    const saved = clipboard.readText();
    clipboard.write({ text: 'P14 text', html: '<b>P14 html</b>' });
    const result = { text: clipboard.readText(), html: clipboard.readHTML(), formats: clipboard.availableFormats() };
    clipboard.writeText(saved);
    return result;
  },
  theme: () => {
    nativeTheme.themeSource = nativeTheme.shouldUseDarkColors ? 'light' : 'dark';
    return { source: nativeTheme.themeSource, dark: nativeTheme.shouldUseDarkColors };
  },
  screen: () => ({ primary: screen.getPrimaryDisplay().bounds, count: screen.getAllDisplays().length, cursor: screen.getCursorScreenPoint() }),
  image: () => {
    const image = nativeImage.createFromDataURL(nativeImage.createEmpty().toDataURL());
    return { empty: image.isEmpty() };
  },
  'power-save': () => {
    const id = powerSaveBlocker.start('prevent-display-sleep');
    const started = powerSaveBlocker.isStarted(id);
    powerSaveBlocker.stop(id);
    return { started, stopped: !powerSaveBlocker.isStarted(id) };
  },
  'safe-storage': () => {
    const cipher = safeStorage.encryptString('secret');
    return { available: safeStorage.isEncryptionAvailable(), length: cipher.length, back: safeStorage.decryptString(cipher) };
  },
};

ipcMain.handle('run', async (_event, name) => {
  try {
    return { ok: await actions[name]() };
  } catch (error) {
    return { error: String(error && error.message) };
  }
});

app.whenReady().then(() => {
  win = new BrowserWindow({ width: 640, height: 720, title: 'P14 system-a', webPreferences: { preload: path.join(__dirname, 'preload.js') } });
  win.loadFile(path.join(__dirname, 'index.html'));
});

app.on('window-all-closed', () => app.quit());
