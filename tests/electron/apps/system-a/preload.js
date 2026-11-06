const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('run', (name) => ipcRenderer.invoke('run', name));
