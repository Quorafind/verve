// Asks for the single-instance lock and writes what happened, one JSON object per line, to the file
// named by --log. --role=first stays until a second instance has been heard of; --role=second
// quits as soon as it is refused. Read by crates/verve-electron/tests/app.rs.
const { app } = require('electron');
const fs = require('fs');

const role = app.commandLine.getSwitchValue('role');
const log = (value) => fs.appendFileSync(app.commandLine.getSwitchValue('log'), JSON.stringify(value) + '\n');

const data = { hello: 'world', list: [1, 2, { nested: null }], when: new Date(0) };
const got = app.requestSingleInstanceLock(data);
log({ role, got, has: app.hasSingleInstanceLock() });

if (!got) {
  app.quit();
} else {
  app.on('second-instance', (event, argv, workingDirectory, additionalData) => {
    log({ heard: true, argv, workingDirectory, additionalData, eventKeys: Object.keys(event) });
    app.releaseSingleInstanceLock();
    log({ released: !app.hasSingleInstanceLock() });
    app.exit(0);
  });
}
