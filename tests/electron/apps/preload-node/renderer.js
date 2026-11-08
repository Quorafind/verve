const os = require('node:os');
const { ipcRenderer } = require('electron');

console.log('page: preload said', window.fromPreload);
console.log('page: eol', JSON.stringify(os.EOL.length > 0), 'node', typeof process.versions.node);
ipcRenderer.send('done');
