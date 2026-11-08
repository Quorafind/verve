'use strict';
// What Electron's IPC does, as data. Run it with `npx electron .` (Electron 39): the main process
// and the pages it opens (renderer.js) try every way the two talk, note what each side saw, and
// print one JSON object, `OBSERVE_JSON {...}`, before the app exits by itself. `OBSERVE_OUT=file`
// also writes it pretty-printed. The user data folder is a temporary one, never the user's own.
//
// verve-electron's `tests/ipc.rs` freezes what this printed (39.8.10) and holds Verve to it.
const { app, BrowserWindow, ipcMain, MessageChannelMain } = require('electron');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const util = require('node:util');
const { shape, thrown, attempt } = require('./shape');
const { samples } = require('./samples');

// An event nobody answered is released when it is collected: run with `--js-flags=--expose-gc`.
const gc = global.gc;

app.setPath('userData', fs.mkdtempSync(path.join(os.tmpdir(), 'ipc-observe-')));

const results = {};
const record = (key, value) => {
  results[key] = value;
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// What the main process warns and complains of, without the machine's paths and stack frames.
const logged = [];
for (const level of ['warn', 'error']) {
  const write = console[level].bind(console);
  console[level] = (...args) => {
    const lines = util.format(...args).split(/\r?\n/);
    const frames = lines.filter((line) => /^\s+at /.test(line)).length;
    const text = lines.filter((line) => !/^\s+at /.test(line)).join('\n').split(__dirname).join('<app>');
    logged.push({ level, text, frames });
    write(...args);
  };
}

// The scenarios close their windows one after the other; the app ends in `finish`.
app.on('window-all-closed', () => {});

function open(scenario) {
  const win = new BrowserWindow({
    show: false,
    webPreferences: { nodeIntegration: true, contextIsolation: false },
  });
  win.loadFile(path.join(__dirname, 'index.html'), { query: { scenario } });
  return win;
}

class MyError extends Error {
  constructor(message) {
    super(message);
    this.name = 'MyError';
  }
}

// ---- the report channel, and a way to wait for a page's report ----
ipcMain.on('result', (event, key, json) => record(key, JSON.parse(json)));

// ---- dispatch order and the shape of an event ----
const order = [];
const eventShape = (event, extra) => ({
  keys: Object.keys(event),
  names: Object.getOwnPropertyNames(event),
  prototypeNames: Object.getOwnPropertyNames(Object.getPrototypeOf(event)),
  type: event.type,
  processIdType: typeof event.processId,
  frameId: event.frameId,
  senderIsContents: event.sender === extra.contents,
  senderFrameIsMainFrame: event.senderFrame === extra.contents.mainFrame,
  senderFrameCtor: event.senderFrame && event.senderFrame.constructor.name,
  ports: shape(event.ports),
  reply: typeof event.reply,
  hasReturnValue: 'returnValue' in event,
  returnValueBefore: shape(event.returnValue),
});

function watchPostOrder(contents) {
  const heard = [];
  contents.on('ipc-message', (event, channel) => channel === 'post-order' && heard.push('webContents ipc-message'));
  contents.mainFrame.ipc.on('post-order', () => heard.push('frame.ipc'));
  contents.ipc.on('post-order', () => heard.push('webContents.ipc'));
  ipcMain.on('post-order', () => {
    heard.push('ipcMain');
    record('postOrder', heard);
  });
}

function watchOrder(contents) {
  watchPostOrder(contents);
  contents.on('ipc-message', (event, channel) => channel === 'order' && order.push('webContents ipc-message'));
  contents.on('ipc-message-sync', (event, channel) => channel === 'order-sync' && order.push('webContents ipc-message-sync'));
  contents.on('ipc-message', (event, channel) => channel === 'order-invoke' && order.push('webContents ipc-message (invoke)'));
  contents.mainFrame.ipc.on('order', () => order.push('frame.ipc'));
  contents.ipc.on('order', () => order.push('webContents.ipc'));
  contents.mainFrame.ipc.on('order-sync', () => order.push('frame.ipc sync'));
  contents.ipc.on('order-sync', () => order.push('webContents.ipc sync'));
  ipcMain.on('order', (event, ...args) => {
    order.push('ipcMain');
    record('order', order.splice(0));
    record('sendEvent', eventShape(event, { contents }));
    record('sendArgs', shape(args));
  });
  ipcMain.on('order-sync', (event, ...args) => {
    order.push('ipcMain sync');
    record('orderSync', order.splice(0));
    record('syncEvent', eventShape(event, { contents }));
    event.returnValue = 'ok';
  });
  ipcMain.handle('order-invoke', (event, ...args) => {
    record('orderInvoke', order.splice(0));
    record('invokeEvent', eventShape(event, { contents }));
    return args;
  });
  record('classes', {
    ipcMain: ipcMain.constructor.name,
    contentsIpc: contents.ipc.constructor.name,
    frameIpc: contents.mainFrame.ipc.constructor.name,
    distinct: new Set([ipcMain, contents.ipc, contents.mainFrame.ipc]).size,
    sameFrame: contents.mainFrame === contents.mainFrame,
    frameCtor: contents.mainFrame.constructor.name,
    mainFrameIds: [typeof contents.mainFrame.processId, typeof contents.mainFrame.routingId, typeof contents.mainFrame.frameTreeNodeId],
    emitter: ipcMain instanceof require('node:events'),
    handleReturns: shape(ipcMain.handle('returns', () => 1)),
    removeReturns: shape(ipcMain.removeHandler('returns')),
    onReturnsSelf: ipcMain.on('x-never', () => {}) === ipcMain,
  });
}

// ---- invoke: priority, errors, registration ----
function handlers() {
  const winner = (who) => () => who;
  let contents;
  const chain = [];
  ipcMain.on('prio', () => record('prioPlainListenerRan', true));
  ipcMain.handle('prio-install', (event) => {
    contents = event.sender;
    chain.push(() => contents.mainFrame.ipc.removeHandler('prio'), () => contents.ipc.removeHandler('prio'), () => ipcMain.removeHandler('prio'));
    contents.mainFrame.ipc.handle('prio', winner('frame.ipc'));
    contents.ipc.handle('prio', winner('webContents.ipc'));
    ipcMain.handle('prio', winner('ipcMain'));
    contents.ipc.on('only-listener', () => {});
  });
  ipcMain.handle('prio-step', () => chain.shift()());

  const withProps = new RangeError('ranged');
  withProps.code = 'E_RANGE';
  const failures = {
    'throws-error': () => {
      throw withProps;
    },
    'throws-string': () => {
      throw 'just text';
    },
    'throws-object': () => {
      throw { a: 1 };
    },
    'throws-null': () => {
      throw null;
    },
    'throws-number': () => {
      throw 42;
    },
    'throws-custom': () => {
      throw new MyError('mine');
    },
    'throws-empty': () => {
      throw new Error();
    },
    'throws-cause': () => {
      throw new Error('outer', { cause: new Error('inner') });
    },
    rejects: async () => {
      throw new TypeError('async boom');
    },
    'returns-function': () => () => 1,
    'returns-promise-function': async () => () => 1,
    'returns-symbol': () => Symbol('s'),
  };
  for (const [channel, fn] of Object.entries(failures)) ipcMain.handle(channel, fn);
  ipcMain.handle('take-log', () => logged.splice(0));

  ipcMain.handle('echo-sample', (event, name) => samples()[name]);
  ipcMain.handle('seq-invoke', (event, i) => arrivals.push(['invoke', i]));
  ipcMain.handle('late', async (event, ms) => {
    await sleep(ms);
    return 'late';
  });
  ipcMain.handleOnce('once', () => 'first');

  record('registration', {
    twice: attempt(() => {
      ipcMain.handle('dup', () => 1);
      ipcMain.handle('dup', () => 2);
    }),
    nonStringChannel: attempt(() => ipcMain.handle(5, () => 1)),
    nonFunction: attempt(() => ipcMain.handle('bad', 5)),
    noArguments: attempt(() => ipcMain.handle()),
    removeMissing: attempt(() => ipcMain.removeHandler('never-registered')),
    removeNothing: attempt(() => ipcMain.removeHandler()),
    onceTwice: attempt(() => {
      ipcMain.handleOnce('dup-once', () => 1);
      ipcMain.handleOnce('dup-once', () => 2);
    }),
  });
}

// ---- sendSync, send, invoke: order of arrival ----
const arrivals = [];
ipcMain.on('seq', (event, i) => arrivals.push(['send', i]));
ipcMain.on('seq-sync', (event, i) => {
  arrivals.push(['sync', i]);
  event.returnValue = i;
});
ipcMain.on('seq-done', () => {
  const next = {};
  for (const [kind, i] of arrivals) {
    if (i < (next[kind] ?? 0)) next.broken = true;
    next[kind] = i;
  }
  const interleaved = arrivals.map(([kind, i]) => `${kind[0]}${i}`);
  record('fifo', { count: arrivals.length, perKindAscending: !next.broken, head: interleaved.slice(0, 12), sequentialInSend: arrivals.every(([, i], at) => i === at) });
});

// ---- clone: what each way of carrying a value does with every sample ----
ipcMain.on('clone', (event, name, value) => record(`toMain:${name}`, shape(value)));
ipcMain.on('return-sample', (event, name) => {
  const outcome = attempt(() => {
    event.returnValue = samples()[name];
  });
  if (outcome.threw) {
    record(`returnValueThrew:${name}`, outcome.threw);
    // Nobody will answer: the page is released when the event is collected.
    setTimeout(gc, 50);
  }
});
ipcMain.on('reply', (event, a) => {
  record('replyReturns', shape(event.reply('pong', a, 2)));
});

// ---- the steps the main page asks the main process to take, and wait for ----
const steps = {
  async sendToRenderer(event) {
    const contents = event.sender;
    contents.send('echo', 1, 'two');
    for (const [name, value] of Object.entries(samples())) {
      record(`mainSend:${name}`, attempt(() => contents.send('clone', name, value)));
    }
    record('mainSendMisc', {
      returns: attempt(() => contents.send('rcv', 1, 'two')),
      noChannel: attempt(() => contents.send()),
      numberChannel: attempt(() => contents.send(5)),
      toFrame: attempt(() => contents.sendToFrame(1, 'rcv', 'frame-1')),
      toFramePair: attempt(() => contents.sendToFrame([contents.mainFrame.processId, contents.mainFrame.frameTreeNodeId], 'rcv', 'frame-pair')),
      toFrameProcessRouting: attempt(() => contents.sendToFrame([contents.mainFrame.processId, contents.mainFrame.routingId], 'rcv', 'frame-routing')),
      toOtherFrame: attempt(() => contents.sendToFrame(99999, 'rcv', 'other-frame')),
      mainFrameSend: attempt(() => contents.mainFrame.send('rcv', 'via-main-frame')),
      frameIds: [contents.mainFrame.frameTreeNodeId, contents.mainFrame.routingId],
      badTransfer: attempt(() => contents.postMessage('rcv', 1, [{}])),
      numberTransfer: attempt(() => contents.postMessage('rcv', 1, [1])),
      stringTransfer: attempt(() => contents.postMessage('rcv', 1, 'str')),
      bufferTransfer: attempt(() => contents.postMessage('rcv', 1, [new ArrayBuffer(1)])),
      plainPost: attempt(() => contents.postMessage('rcv', { posted: 1 })),
      frameBadTransfer: attempt(() => contents.mainFrame.postMessage('rcv', 1, [{}])),
    });
    await sleep(400);
  },
  async ports(event) {
    const contents = event.sender;
    const one = new MessageChannelMain();
    record('channel', {
      ctor: one.constructor.name,
      keys: Object.keys(one),
      port1: one.port1.constructor.name,
      emitter: one.port1 instanceof require('node:events'),
      portMethods: Object.getOwnPropertyNames(Object.getPrototypeOf(one.port1)).sort(),
      distinct: one.port1 !== one.port2,
      newWithout: attempt(() => MessageChannelMain()),
    });
    // Is a port started by listening, or only by start()?
    const heard = [];
    one.port2.on('message', (e) => heard.push({ keys: Object.keys(e), names: Object.getOwnPropertyNames(e), data: shape(e.data), ports: shape(e.ports) }));
    one.port1.postMessage({ a: 1 });
    await sleep(300);
    record('portHeardBeforeStart', heard.length);
    one.port2.start();
    await sleep(300);
    record('portHeardAfterStart', heard.slice());
    // A port in the transfer list of a port message, and what else cannot be.
    const inner = new MessageChannelMain();
    const innerHeard = [];
    one.port2.on('message', (e) => e.ports.length && innerHeard.push(shape(e.ports)));
    one.port1.postMessage('with-port', [inner.port1]);
    await sleep(300);
    record('portInPortMessage', innerHeard);
    record('portPostMisc', {
      object: attempt(() => one.port1.postMessage('x', [{}])),
      number: attempt(() => one.port1.postMessage('x', [1])),
      string: attempt(() => one.port1.postMessage('x', 'str')),
      arrayBuffer: attempt(() => one.port1.postMessage('x', [new ArrayBuffer(1)])),
      noArguments: attempt(() => one.port1.postMessage()),
      function: attempt(() => one.port1.postMessage(() => 1)),
      afterTransfer: attempt(() => inner.port1.postMessage('after')),
    });
    // Transfer lists: a port twice, the port itself, and a port that is closed or given away.
    const [a, b, closed, away, fresh1, fresh2] = Array.from({ length: 6 }, () => new MessageChannelMain());
    closed.port1.close();
    contents.postMessage('rcv', 1, [away.port1]);
    record('transferLists', {
      twice: attempt(() => a.port1.postMessage('x', [b.port1, b.port1])),
      itself: attempt(() => a.port1.postMessage('x', [a.port1])),
      twiceToPage: attempt(() => contents.postMessage('rcv', 1, [b.port2, b.port2])),
      portAfterRefused: attempt(() => b.port2.postMessage('still usable')),
      closedCloneFails: attempt(() => closed.port1.postMessage(() => 1, [fresh1.port1])),
      freshAfterClosed: attempt(() => fresh1.port1.postMessage('usable')),
      neuteredCloneFails: attempt(() => away.port1.postMessage(() => 1, [fresh2.port1])),
      freshAfterNeutered: attempt(() => fresh2.port1.postMessage('usable')),
      closedBadList: attempt(() => closed.port1.postMessage('x', 'not a list')),
    });
    // close
    const closes = [];
    const two = new MessageChannelMain();
    two.port1.on('close', () => closes.push('port1'));
    two.port2.on('close', () => closes.push('port2'));
    two.port1.close();
    await sleep(300);
    record('closeEvents', closes.slice());
    record('afterClose', {
      postClosed: attempt(() => two.port1.postMessage('x')),
      postPeer: attempt(() => two.port2.postMessage('x')),
      closeAgain: attempt(() => two.port1.close()),
      startClosed: attempt(() => two.port1.start()),
    });
    // main -> page
    const down = new MessageChannelMain();
    down.port2.on('message', (e) => record('mainPortGot', { data: shape(e.data), ports: shape(e.ports) }));
    down.port2.on('close', () => record('mainPortClosedByPage', true));
    down.port2.start();
    contents.postMessage('port-down', { greeting: 'hello' }, [down.port1]);
    record('afterTransferDown', {
      post: attempt(() => down.port1.postMessage('x')),
      start: attempt(() => down.port1.start()),
      close: attempt(() => down.port1.close()),
      again: attempt(() => contents.postMessage('port-down', 1, [down.port1])),
    });
    await sleep(300);
    down.port2.postMessage({ from: 'main' });
    await sleep(500);
  },
};

ipcMain.handle('step', (event, name) => steps[name](event));
ipcMain.on('post-plain', (event, message) => {
  record('postPlain', { keys: Object.keys(event), ports: shape(event.ports), message: shape(message) });
});
ipcMain.on('port-up', (event, message) => {
  const port = event.ports[0];
  record('portUpEvent', { keys: Object.keys(event), message: shape(message), ports: shape(event.ports), portCtor: port && port.constructor.name });
  port.on('message', (e) => record('mainGotFromPage', shape(e.data)));
  port.start();
  port.postMessage('hello-from-main');
});

// ---- the scenarios in windows of their own ----
function syncScenarios() {
  ipcMain.on('late-return', (event) => {
    setTimeout(() => {
      event.returnValue = 'set later';
    }, 300);
  });
  ipcMain.on('twice-return', (event) => {
    event.returnValue = 'first';
    event.returnValue = 'second';
  });
  ipcMain.on('bad-return', (event) => {
    record('badReturnAssign', attempt(() => (event.returnValue = () => 1)));
    setTimeout(gc, 50);
  });
  ipcMain.on('unset', () => {});
}

async function unsetScenario() {
  const win = open('sync-unset');
  await sleep(2500);
  record('syncUnsetBlockedAfter2s', !('syncUnset' in results));
  gc();
  await sleep(1000);
  record('syncUnsetAfterGc', 'syncUnset' in results ? results.syncUnset : 'still blocked');
  win.destroy();
}

/** Which windows have a `require`: the sandbox and context isolation decide, not `nodeIntegration` alone. */
async function requireScenario() {
  const kinds = {
    default: {},
    nodeIntegration: { nodeIntegration: true },
    nodeIntegrationNoIsolation: { nodeIntegration: true, contextIsolation: false },
    nodeIntegrationSandboxed: { nodeIntegration: true, contextIsolation: false, sandbox: true },
    nodeIntegrationUnsandboxed: { nodeIntegration: true, contextIsolation: false, sandbox: false },
    unsandboxedNoIntegration: { sandbox: false, contextIsolation: false },
  };
  const seen = {};
  for (const [name, webPreferences] of Object.entries(kinds)) {
    const win = new BrowserWindow({ show: false, webPreferences });
    await win.loadURL('data:text/html,<p>x</p>');
    seen[name] = await win.webContents.executeJavaScript('typeof require');
    win.destroy();
  }
  record('requireByPreferences', seen);
}

async function destroyedScenario() {
  const win = open('plain');
  const heard = new Promise((resolve) => ipcMain.once('hello-plain', (event) => resolve(event)));
  const event = await heard;
  const contents = win.webContents;
  const frame = event.senderFrame;
  win.destroy();
  await sleep(300);
  record('destroyed', {
    isDestroyed: contents.isDestroyed(),
    send: attempt(() => contents.send('x')),
    sendToFrame: attempt(() => contents.sendToFrame(1, 'x')),
    postMessage: attempt(() => contents.postMessage('x', 1)),
    id: attempt(() => contents.id),
    ipc: attempt(() => typeof contents.ipc),
    mainFrame: attempt(() => typeof contents.mainFrame),
    reply: attempt(() => event.reply('x', 1)),
    eventSender: attempt(() => event.sender === contents),
    senderFrame: attempt(() => event.senderFrame === null),
    frameSend: attempt(() => frame.send('x')),
  });
}

async function mainScenario() {
  const win = open('main');
  watchOrder(win.webContents);
  handlers();
  syncScenarios();
  await new Promise((resolve) => ipcMain.once('main-done', resolve));
  win.destroy();
}

async function run() {
  await app.whenReady();
  const failsafe = setTimeout(() => {
    record('timedOut', true);
    finish();
  }, 60000);
  await mainScenario();
  const sync = open('sync');
  await new Promise((resolve) => ipcMain.once('sync-done', resolve));
  record('syncNoneWarnings', logged.splice(0));
  sync.destroy();
  await unsetScenario();
  await requireScenario();
  await destroyedScenario();
  clearTimeout(failsafe);
  finish();
}

function finish() {
  record('electron', process.versions.electron);
  const text = JSON.stringify(results);
  process.stdout.write(`OBSERVE_JSON ${text}\n`);
  if (process.env.OBSERVE_OUT) fs.writeFileSync(process.env.OBSERVE_OUT, JSON.stringify(results, null, 1));
  app.exit(0);
}

run();
