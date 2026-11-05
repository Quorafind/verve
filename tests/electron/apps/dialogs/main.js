// Each kind of dialog in turn, logging what it answers. A real run waits for a person at every
// one; the headless shell answers from presets (P14), which is the only way this app ends.
const { app, BrowserWindow, dialog } = require('electron');

app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false });
  const box = await dialog.showMessageBox(win, {
    type: 'question',
    message: 'Save?',
    buttons: ['Yes', 'No'],
    checkboxLabel: 'Remember',
  });
  console.log('message box', box.response, box.checkboxChecked);
  const open = await dialog.showOpenDialog(win, { properties: ['openFile', 'multiSelections'] });
  console.log('open', open.canceled, JSON.stringify(open.filePaths));
  const save = await dialog.showSaveDialog(win, { defaultPath: 'out.txt' });
  console.log('save', save.canceled, save.filePath);
  dialog.showErrorBox('Failed', 'Something went wrong');
  console.log('error box shown');
  app.quit();
});
