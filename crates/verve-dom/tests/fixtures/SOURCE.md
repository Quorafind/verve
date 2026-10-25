# Fixtures

`node_modules/` holds packages exactly as npm publishes them, each with its licence, trimmed to
the files the tests load:

| Package | Version | Files kept |
|---|---|---|
| `react` | 18.3.1 | `index.js`, `jsx-runtime.js`, the production builds in `cjs/` |
| `react-dom` | 18.3.1 | `index.js`, `client.js`, `cjs/react-dom.production.min.js` |
| `scheduler` | 0.23.2 | `index.js`, `cjs/scheduler.production.min.js` |
| `turndown` | 7.2.0 | `lib/turndown.cjs.js`, `lib/turndown.browser.cjs.js` |

They were fetched with `npm pack <name>@<version>` and copied unchanged. Only the production
builds are kept, so the scripts that load them set `process.env.NODE_ENV = 'production'` first;
`tests/react_dev.rs` runs React's development builds, fetched from npm when it runs.

`react-app.js` and `dsh-page.mjs` are the tests' own.
