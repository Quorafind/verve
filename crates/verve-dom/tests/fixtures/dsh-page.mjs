// A DSH web client's page, as little of one as runs a client half: React and the primitives
// from npm, `window.__ModuleLoader__` running the web-search settings page's `lib/client.js`,
// and stand-ins for the context it injects (slots, locale, remote credentials, config forms)
// and the slot that renders it. Each stand-in is what that page reaches of DSH's own; see
// `tests/dsh.rs`.
import { createRequire } from 'node:module';
import * as primitives from '@deepseek-ai/dsh-client-ui-primitives';

const require = createRequire(import.meta.url);
const React = require('react');
const jsxRuntime = require('react/jsx-runtime');
const { createRoot } = require('react-dom/client');
const h = React.createElement;

const log = (globalThis.log = []);

// What the loader hands a client half's `require`: the libraries the web client injects.
const provided = {
  react: React,
  'react/jsx-runtime': jsxRuntime,
  'react-dom': require('react-dom'),
  '@deepseek-ai/dsh-client-ui-primitives': primitives,
};
const loaded = (globalThis.loaded = {});
window.__ModuleLoader__ = {
  load({ id, factory }) {
    loaded[id] = factory((name) => {
      if (!(name in provided)) throw new Error('the page does not provide ' + name);
      return provided[name];
    });
  },
};

// The `web-search-deepseek` settings namespace, as the config forms serve it.
let snapshot = { status: 'ready', writable: true, revision: 1, value: { maxUses: 5 }, base: { maxUses: 5 }, user: {} };
const listeners = new Set();
const scope = {
  getSnapshot: () => snapshot,
  subscribe(listener) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  async mutate(ops, revision) {
    log.push('mutate ' + JSON.stringify(ops) + ' at ' + revision);
    const value = { ...snapshot.value };
    const user = { ...snapshot.user };
    for (const op of ops) {
      if (op.op === 'set') value[op.path[0]] = user[op.path[0]] = op.value;
      else delete user[op.path[0]];
    }
    snapshot = { ...snapshot, revision: snapshot.revision + 1, value, user };
    for (const listener of listeners) listener();
    return true;
  },
};

// The page's context: what its `inject` list names.
const registered = (globalThis.registered = {});
let dictionary = {};
const ctx = {
  locale: {
    bind: (ns) => (key) => dictionary[ns]?.en?.[key] ?? key,
    register(ns, bundles) {
      dictionary[ns] = bundles;
      return () => delete dictionary[ns];
    },
  },
  effect(start, label) {
    log.push('effect ' + label);
    start();
  },
  configForms: {
    get: () => scope,
    whileServed: (_namespaces, serve) => serve(),
  },
  remote: {
    $on: () => () => {},
    credentials: {
      async describe(refs) {
        return { ok: true, value: { [refs[0]]: { configured: false, writable: true } } };
      },
      async set(ref, value) {
        log.push('credential ' + ref + ' ' + value.length);
        return { ok: true };
      },
    },
  },
  slots: {
    inject: (_slot, register) => register(),
    register(options, component) {
      registered[options.id] = { options, component };
      return () => delete registered[options.id];
    },
  },
};

// The slot that renders a page: its injected hooks as `use<Name>(selector)`.
function SlotEntry({ entry, view }) {
  const injected = React.useMemo(() => entry.options.inject(), [entry]);
  const hooks = {};
  for (const [name, store] of Object.entries(injected.hooks ?? {})) {
    hooks['use' + name[0].toUpperCase() + name.slice(1)] = (selector) =>
      React.useSyncExternalStore(store.subscribe, () => selector(store.getSnapshot()));
  }
  const t = ctx.locale.bind(entry.options.locale);
  return h(entry.component, { ...injected, ...hooks, t, view });
}

globalThis.dsh = {
  primitives,
  async loadClient() {
    await import('@deepseek-ai/dsh-client-ui-settings-web-search/client');
    const plugin = loaded['@deepseek-ai/dsh-client-ui-settings-web-search'];
    plugin.apply(ctx);
    return Object.keys(registered).join(',');
  },
  render(element) {
    const container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);
    root.render(element);
    return { container, root };
  },
  renderPage(id, view) {
    return this.render(h(SlotEntry, { entry: registered[id], view }));
  },
  h,
  React,
};
