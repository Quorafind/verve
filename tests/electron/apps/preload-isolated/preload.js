const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('api', {
  node: process.versions.node,
  echo: (text) => ipcRenderer.invoke('echo', text),
  done: () => ipcRenderer.send('done'),
});
