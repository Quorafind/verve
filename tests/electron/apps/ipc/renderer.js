const { ipcRenderer } = require('electron');

const once = (channel) => new Promise((resolve) => ipcRenderer.once(channel, (event, ...args) => resolve(args)));

async function run() {
  const [message] = await once('hello-from-main');
  console.log('renderer: hello-from-main', JSON.stringify(message));

  const pong = once('pong');
  ipcRenderer.send('ping', 1, 2);
  console.log('renderer: pong', (await pong)[0]);

  console.log('renderer: add', await ipcRenderer.invoke('add', 20, 22));
  try {
    await ipcRenderer.invoke('fail');
  } catch (error) {
    console.log('renderer: fail', error.message);
  }
  console.log('renderer: sync', ipcRenderer.sendSync('sync', 21));
  ipcRenderer.send('done');
}

run();
