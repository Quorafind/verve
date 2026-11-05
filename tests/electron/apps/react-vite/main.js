// A React app built by Vite, loaded from its dist folder. The page is a counter: clicking
// #inc raises the number in the button's label. The app quits after a moment unless told how long
// to stay; a test that clicks needs the window for that long.
const { app, BrowserWindow } = require('electron');
const path = require('node:path');

app.whenReady().then(() => {
  const win = new BrowserWindow({ width: 800, height: 600, show: false });
  win.loadFile(path.join(__dirname, 'dist', 'index.html'));
  setTimeout(() => app.quit(), 3000);
});
