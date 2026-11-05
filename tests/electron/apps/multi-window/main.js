// A parent with a child, a modal child, getAllWindows, and window.open allowed by
// setWindowOpenHandler. Each step logs what it sees; the app quits by itself.
const { app, BrowserWindow } = require('electron');

app.whenReady().then(() => {
  const parent = new BrowserWindow({ show: false, title: 'parent' });
  parent.webContents.setWindowOpenHandler(() => ({
    action: 'allow',
    overrideBrowserWindowOptions: { show: false, title: 'opened' },
  }));
  parent.loadURL('data:text/html,<title>parent</title><p>parent</p>');

  const child = new BrowserWindow({ show: false, parent, title: 'child' });
  const modal = new BrowserWindow({ show: false, parent, modal: true, title: 'modal' });
  console.log('windows', BrowserWindow.getAllWindows().length);
  console.log('child parent is parent', child.getParentWindow() === parent);
  console.log('children', parent.getChildWindows().length);
  console.log('modal', modal.isModal());

  parent.webContents.once('did-finish-load', async () => {
    await parent.webContents.executeJavaScript("window.open('about:blank'), 0");
    console.log('windows after window.open', BrowserWindow.getAllWindows().length);
    setTimeout(() => app.quit(), 300);
  });
  setTimeout(() => app.quit(), 8000);
});
