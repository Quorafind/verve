'use strict';
// The page's half of main.js: each scenario is a window of its own (`?scenario=`).
const { ipcRenderer } = require('electron');
const { shape, thrown, attempt, attemptAsync } = require('./shape');
const { samples } = require('./samples');

const scenario = new URLSearchParams(location.search).get('scenario');
const report = (key, value) => ipcRenderer.send('result', key, JSON.stringify(value));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
/** What a call answers or throws, a promise it answers settled. */
const both = async (fn) => {
  let out;
  try {
    out = fn();
  } catch (error) {
    return { threw: thrown(error) };
  }
  return attemptAsync(() => out);
};

const received = [];
ipcRenderer.on('rcv', (event, ...args) => received.push(shape(args)));
ipcRenderer.on('clone', (event, name, value) => report(`toRenderer:${name}`, shape(value)));
ipcRenderer.on('port-down', (event, message) => {
  const port = event.ports[0];
  report('portDown', {
    message: shape(message),
    ports: shape(event.ports),
    isMessagePort: port instanceof MessagePort,
    portCtor: port.constructor.name,
  });
  port.onmessage = (e) => {
    report('pageGotFromMain', { data: shape(e.data), keys: Object.keys(e) });
    port.close();
  };
  port.postMessage({ from: 'page' });
});
ipcRenderer.on('pong', (event, ...args) => report('pong', shape(args)));
ipcRenderer.on('echo', (event, ...args) => {
  report('rendererEvent', {
    keys: Object.keys(event),
    names: Object.getOwnPropertyNames(event),
    senderIsIpcRenderer: event.sender === ipcRenderer,
    senderId: typeof event.senderId,
    ports: shape(event.ports),
    args: shape(args),
  });
});

async function mainScenario() {
  // Dispatch.
  ipcRenderer.send('order', 1, 'two');
  await ipcRenderer.invoke('order-invoke', 'a', { b: 2 });
  report('orderSyncReturn', shape(ipcRenderer.sendSync('order-sync', 'q')));
  ipcRenderer.send('reply', 5);
  await sleep(200);

  // The members of ipcRenderer, and what bad arguments do.
  const names = ['send', 'invoke', 'sendSync', 'postMessage', 'sendToHost', 'sendTo', 'on', 'once', 'off', 'removeListener', 'removeAllListeners', 'addListener', 'listenerCount', 'emit', 'eventNames'];
  report('rendererApi', {
    types: Object.fromEntries(names.map((name) => [name, typeof ipcRenderer[name]])),
    ctor: ipcRenderer.constructor.name,
    own: Object.keys(ipcRenderer),
  });
  report('rendererArgs', {
    sendNone: await both(() => ipcRenderer.send()),
    sendNumber: await both(() => ipcRenderer.send(5)),
    sendObject: await both(() => ipcRenderer.send({})),
    sendFunction: await both(() => ipcRenderer.send('x', () => 1)),
    invokeNone: await both(() => ipcRenderer.invoke()),
    invokeNumber: await both(() => ipcRenderer.invoke(5)),
    invokeFunction: await both(() => ipcRenderer.invoke('x', () => 1)),
    sendSyncNone: await both(() => ipcRenderer.sendSync()),
    sendSyncNumber: await both(() => ipcRenderer.sendSync(5)),
    sendSyncFunction: await both(() => ipcRenderer.sendSync('x', () => 1)),
    postMessageNone: await both(() => ipcRenderer.postMessage()),
    postMessageNumberTransfer: await both(() => ipcRenderer.postMessage('x', 1, [1])),
    postMessageObjectTransfer: await both(() => ipcRenderer.postMessage('x', 1, [{}])),
    postMessageBufferTransfer: await both(() => ipcRenderer.postMessage('x', 1, [new ArrayBuffer(1)])),
    postMessageStringTransfer: await both(() => ipcRenderer.postMessage('x', 1, 'str')),
    postMessagePlain: await both(() => ipcRenderer.postMessage('x', { a: 1 })),
    sendToHost: await both(() => ipcRenderer.sendToHost('x', 1)),
  });

  // Priority of handlers, and the errors a handler's failure becomes.
  await ipcRenderer.invoke('prio-install');
  const prio = [];
  for (let at = 0; at < 4; at++) {
    prio.push(await both(() => ipcRenderer.invoke('prio')));
    if (at < 3) await ipcRenderer.invoke('prio-step');
  }
  report('priority', prio);
  report('onlyListenerInvoke', await both(() => ipcRenderer.invoke('only-listener')));
  const failures = {};
  for (const channel of ['throws-error', 'throws-string', 'throws-object', 'throws-null', 'throws-number', 'throws-custom', 'throws-empty', 'throws-cause', 'rejects', 'returns-function', 'returns-promise-function', 'returns-symbol', 'no-such-channel']) {
    await ipcRenderer.invoke('take-log');
    const outcome = await both(() => ipcRenderer.invoke(channel));
    failures[channel] = { outcome, main: await ipcRenderer.invoke('take-log') };
  }
  report('failures', failures);
  report('handleOnce', [await both(() => ipcRenderer.invoke('once')), await both(() => ipcRenderer.invoke('once'))]);
  report('invokeDuplicate', await both(() => ipcRenderer.invoke('dup')));
  report('invokeLate', await both(() => ipcRenderer.invoke('late', 300)));

  // Cloning each way: send, invoke's answer, sendSync's returnValue.
  for (const [name, value] of Object.entries(samples())) {
    const outcome = attempt(() => ipcRenderer.send('clone', name, value));
    if (outcome.threw) report(`sendThrew:${name}`, outcome.threw);
  }
  const platform = {
    domNode: () => document.body,
    blob: () => new Blob(['abc'], { type: 'text/plain' }),
    file: () => new File(['abc'], 'a.txt', { type: 'text/plain', lastModified: 5 }),
    imageData: () => new ImageData(1, 1),
    style: () => document.body.style,
    classList: () => document.body.classList,
    dataset: () => document.body.dataset,
    children: () => document.body.children,
    attributes: () => document.body.attributes,
    event: () => new Event('x'),
  };
  for (const [name, make] of Object.entries(platform)) {
    const outcome = attempt(() => ipcRenderer.send('clone', name, make()));
    if (outcome.threw) report(`sendThrew:${name}`, outcome.threw);
  }
  for (const name of ['error', 'errorWithProps', 'customError', 'errorWithCause', 'buffer', 'uint8Window', 'arrayBuffer', 'map', 'regexp', 'date', 'nullProto', 'instance', 'function', 'symbol', 'circular']) {
    report(`invokeResult:${name}`, await both(() => ipcRenderer.invoke('echo-sample', name)));
    report(`syncResult:${name}`, attempt(() => ipcRenderer.sendSync('return-sample', name)));
  }
  await ipcRenderer.invoke('step', 'sendToRenderer');
  report('received', received);

  // Order: sends, invokes and sendSyncs interleaved arrive in the order they were made.
  const pending = [];
  for (let i = 0; i < 100; i++) {
    ipcRenderer.send('seq', i);
    pending.push(ipcRenderer.invoke('seq-invoke', i));
    if (i % 25 === 0) ipcRenderer.sendSync('seq-sync', i);
  }
  await Promise.all(pending);
  ipcRenderer.send('seq-done');

  // Ports: to the main process, and from it.
  ipcRenderer.postMessage('post-plain', 'x');
  ipcRenderer.postMessage('post-order', 'x');
  const { port1, port2 } = new MessageChannel();
  port1.onmessage = (e) => report('pageGotFromMainPort', shape(e.data));
  ipcRenderer.postMessage('port-up', { with: 'port' }, [port2]);
  report('afterTransferUp', {
    post: attempt(() => port2.postMessage('x')),
    again: await both(() => ipcRenderer.postMessage('port-up', 1, [port2])),
  });
  port1.postMessage('page-to-main');
  await ipcRenderer.invoke('step', 'ports');
  await sleep(300);
  ipcRenderer.send('main-done');
}

async function syncScenario() {
  const began = Date.now();
  report('syncNone', attempt(() => ipcRenderer.sendSync('nobody-listens', 1)));
  report('syncNoneBlockedFor', Date.now() - began < 500 ? 'under 500 ms' : 'long');
  report('syncLate', attempt(() => ipcRenderer.sendSync('late-return')));
  report('syncTwice', attempt(() => ipcRenderer.sendSync('twice-return')));
  report('syncBad', attempt(() => ipcRenderer.sendSync('bad-return')));
  ipcRenderer.send('sync-done');
}

const scenarios = {
  main: mainScenario,
  sync: syncScenario,
  'sync-unset': async () => report('syncUnset', attempt(() => ipcRenderer.sendSync('unset'))),
  plain: async () => ipcRenderer.send('hello-plain'),
};
scenarios[scenario]();
