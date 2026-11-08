// An app that is meant to run from an app.asar. The tests pack this directory into one with Rust
// (P11), so nothing here depends on npm's asar package; run unpacked it behaves the same.
const { app } = require('electron');
const fs = require('node:fs');
const path = require('node:path');

app.whenReady().then(() => {
  console.log('appPath ends with', path.basename(app.getAppPath()));
  console.log('text', fs.readFileSync(path.join(__dirname, 'data', 'hello.txt'), 'utf8').trim());
  console.log('dir', JSON.stringify(fs.readdirSync(path.join(__dirname, 'data'))));
  app.quit();
});
