# verve-dom

A headless web document for [Verve](../../README.md): the DOM, CSS's cascade and the window's
platform pieces, every one a native Rust function on Verve's Boa — so React and DOM-using
libraries run unchanged, with no browser and no webview. Scripts build and change the document
as in a browser; whoever draws it reads it from a snapshot, feeds in what a person does as the
DOM's events, and reports back where it painted each node.

It is opt-in. `verve run` and every other embedder have no `document` unless they ask:

```rust
let mut runtime = verve::Runtime::new();
let document = verve_dom::install(&mut runtime, verve_dom::Options {
    url: "https://app.example/".into(),
    viewport: (1280., 800.),
    ..verve_dom::Options::default()
})?;
runtime.eval("document.body.innerHTML = '<button>Go</button>'")?;
let update = document.update(); // a Snapshot, and what changed since the last one
```

`verve-dom run` is `verve run` with a document in the realm, for trying a script from the
command line.

## How it is put together

| Layer | Where | What it is |
|---|---|---|
| The arena | `src/dom.rs` | One document per realm, and every node made but not in it, in one arena of generational ids. Attributes, classes, inline style, namespaces, what is typed in a field (its caret and selection, `src/field.rs`), which box is ticked, which option chosen, focus, `:hover` and `:active`. It remembers what changed (for restyling and redrawing), which subtrees joined or left the document, and — while a `MutationObserver` watches — mutation records. Nodes are held by `Arc`, so a copy of the arena is cheap and shares them. |
| CSS | `src/css.rs` | Selectors level 4 less what needs layout — every combinator, attribute operator, `:not`/`:is`/`:where`/`:has`, `:nth-child(… of S)` and its family, the state pseudo-classes — parsed into a `SyntaxError` when they do not parse. The cascade: origin, importance, specificity and order, the inline style, inheritance, `inherit`/`initial`/`unset`, `var()` with fallbacks, font sizes computed to pixels, shorthands opened up, `@media` evaluated against the viewport and colour scheme, `@supports`/`@layer` applied; a user agent sheet (`display`, the headings, the lists). Kept incrementally: only the subtrees whose nodes changed are styled again, each node's style shared by `Arc` with the last snapshot. |
| Markup | `src/html.rs` | `innerHTML`, `outerHTML`, `insertAdjacentHTML`, `DOMParser`, `document.write` and `XMLSerializer`: raw text in `<script>`/`<style>`/`<textarea>`, the end tags HTML implies, `<tbody>` round bare rows, a whole document's `<html>`/`<head>`/`<body>`, SVG and MathML as foreign content, character references. |
| The classes | `src/bind/` | `Node` ← `Element` ← `HTMLElement` and its sixty-odd subclasses, `SVGElement`'s, `Text`, `Comment`, `DocumentFragment`, `Document`, `ShadowRoot`; the fields (`value`, `checked`, selections, `select`'s options, forms and their submission, labels), links' URL parts, tables, templates, dialogs, media; `classList`, `style`, `dataset`, `attributes`; `Range`, `Selection`, `TreeWalker`; the CSSOM (`sheet`, `styleSheets`, `new CSSStyleSheet`, `adoptedStyleSheets`), `getComputedStyle`; `MutationObserver`, `ResizeObserver`, `IntersectionObserver`; `XMLHttpRequest` (on Verve's `fetch`); custom elements; a canvas that measures text and paints nothing; `DOMMatrix`, `DOMRect`, `DataTransfer`. |
| Events | `src/bind/events.rs`, Verve's `dom_events` | A node's object *is* one of Verve's event targets, so `Node` extends the realm's own `EventTarget` and `AbortSignal`, `MessagePort` and the rest keep theirs; Verve's dispatch asks the document what each target is in and captures, targets and bubbles along that path up to the window. `UIEvent`, `MouseEvent`, `PointerEvent`, `KeyboardEvent`, `InputEvent`, `FocusEvent`, `WheelEvent` and the rest derive from Verve's `Event`; every element, the document and the window have the `on…` handler properties. What the document does itself with events: focus moving, a click ticking a box (and untouched again if prevented), radio groups, a label pressing its control, implicit submission, `<summary>` toggling its `<details>`. |
| The window | `src/bind/window.rs` | The global object made a `Window`: its sizes, `location` and `history` (`pushState`, `hashchange`, `popstate`), `localStorage` (in memory, or on disk through a `StorageBackend`) and `sessionStorage`, `requestAnimationFrame` (on the realm's timers, or the drawer's frames), `requestIdleCallback`, `matchMedia` (whose lists hear `change`), `scrollTo`, `screen`, `visualViewport`, `postMessage`, `navigator.clipboard`. What only the embedder can do — navigate, open a window, write the clipboard, alert — is a `Request`. |

### Objects and memory

One node has one object, held weakly (`JsObject::downgrade`, a Boa change recorded in
`boa/SU.md`), so a node a script let go of is collected. Each object holds its parent's and
its children's objects (through Verve's `dom_events::hold`), so a tree a script holds any part
of — the root or a leaf — stays whole, with what was put on its nodes and their listeners; the
document's own object is held by `window.document`, so everything in the document lives as long
as the page. A detached tree none of whose objects is alive any more is freed from the arena by
`Document::sweep`, which an embedder calls now and then (when idle, say).

## The interface for whoever draws it

The `Document` handle lives on the runtime's thread (it is not `Send`). A drawer on another
thread runs both in a `verve::isolate` guest — `tests/isolate.rs` is one — and reads the
`Snapshot`s it publishes; what a person does goes to it as jobs.

### What to draw

- `Document::update() -> Update { snapshot, changes }` brings the styles up to date and answers
  both. `Snapshot` is `Send + Sync` and cheap to clone: `snapshot.dom` is the arena (nodes,
  attributes, text, fields' drafts, focus, hover) and `snapshot.styles` every connected node's
  computed style as text (`snapshot.style(node, "padding-left")`); `css::length` and
  `css::color` turn values into pixels and colours. `snapshot.is_rendered(node)` says whether
  `display: none` hides it.
- `Changes` says what moved since the last update: `nodes` changed (their own state or their
  children), `restyled` (their computed style differs), `restyled_all` (a stylesheet or the
  viewport changed), `removed` (freed by a sweep), and `partitions`.
- **Partitions** are subtrees the drawer draws as views of its own
  (`Document::set_partition(node, true)`): a change inside one marks only it in
  `changes.partitions`, so every other view is reused from the last frame, layout and paint
  included — su-editor's way of redrawing one row of a settings page as it is typed into.
- `Document::hover_sensitive(node)` says a `:hover`/`:active` rule would change it (follow the
  pointer there); `Document::listens(runtime, node, &["click"])` that something listens on it.
- `Document::set_stylesheet(name, css)` adds a sheet of the embedder's (a theme's variables)
  under the page's.

### What a person does

Each takes the runtime, dispatches trusted events in a browser's order with a browser's
default actions, runs the microtasks the listeners queued, and leaves the document settled.

- Pointer: `pointer_move`, `pointer_down`, `pointer_up`, `wheel`, `pointer_leave`, with a
  `Pointer { x, y, button, count, modifiers }`. They hit-test against the boxes last reported
  (`Document::hit_test`); `pointer_down_on`, `pointer_up_on`, `hover` and `press(node)` take the
  node instead, for a drawer that hit-tests itself. Moves give `pointerover`/`pointerenter`/
  `mouseover`/`mouseenter` (and their `out`/`leave`s) and `:hover`; a press gives `pointerdown`,
  `mousedown`, `:active` and the focus; a release `pointerup`, `mouseup` and the button's
  `click` (with its activation), `dblclick`, `auxclick` or `contextmenu`.
- Keys: `key_down`, `key_up` with a `Key { key, code, modifiers, repeat }` (`Key::char('a')`,
  `Key::named("Enter")`). Unless prevented, a character types into the focused field
  (`keypress`, `beforeinput`, `input`), Backspace/Delete and the caret's keys edit it, Enter
  submits its form or presses a button, Space presses a button or box, Tab moves the focus.
  `type_text(text)` is those keys for each character.
- Text: `insert_text(text)` puts what an input method committed (or a paste) into the focused
  field at once; `edit_field(node, |draft| …)` lets a drawer that edits fields itself change
  the `Draft` and dispatches `input`; `draft(node)` reads text, caret and selection;
  `commit_field` sends `change`; `choose(select, value)` picks an option.
- Focus: `focus(Some(node))`, `focused()`.

### Where things landed

- `Document::set_layout(runtime, [(node, Rect)])` reports the border box the drawer painted
  each node in, in viewport coordinates, replacing the last report (`update_layout` merges).
  From then `getBoundingClientRect`, `offsetWidth`/`clientHeight`/`scrollHeight`,
  `elementFromPoint` and the pointer's hit tests answer from it; `ResizeObserver`s whose
  targets changed size and `IntersectionObserver`s whose targets came into or out of sight
  hear it. A node not measured answers the box round what of it was.
- `set_viewport(runtime, w, h)`: `innerWidth`, `@media`, `matchMedia`'s `change`, `resize`.
  `set_color_scheme(runtime, dark)`: `prefers-color-scheme`.
- `set_scroll_offsets(runtime, [(node, [left, top])])`: what `scrollTop` reads, and `scroll`
  for each box that moved. A script's own `scrollTo` or `scrollTop = …` is a
  `Request::Scroll` for the drawer to carry out.
- Frames: with `Options::frames = FrameClock::Host`, `requestAnimationFrame` waits for
  `Document::frame(runtime, now)` (and asks for one with `Request::Frame`); after its
  callbacks the observers hear what moved. With `FrameClock::Timer` (the default) frames run on
  the realm's timers, for a realm nothing draws.

### What only the embedder can do

`Document::take_requests()`: `Navigate`, `Reload`, `Open`, `CopyText`, `Alert`, `Title`,
`Scroll`, `Fullscreen`, `Submit`, `Frame`.

## Extending the classes

`verve_dom::host` is what an embedder adds its own methods with — su-editor's Obsidian host its
`createEl`, `setText` and `empty` (`tests/extend.rs` does exactly that):
`host::extend(context, "Element", methods, accessors)` lays natives over any class this
document has; `host::node_of`, `host::wrap`, `host::with_dom` (which keeps the objects' links
and the mutation observers following), `host::read_dom` and `host::fire` are what they are
written with.

## What was lifted, and what was left out

The arena, the cascade and its incremental restyling, `var()`, the HTML reader and writer, and
most of the bindings — elements, fields, forms, `DOMParser`, `Range`/`Selection`, custom
elements, the CSSOM, `getComputedStyle`, the observers, canvas measuring, geometry, `Image`,
`Option` — come from su-editor's Obsidian host (`crates/obsidian-host`), where one arena holds
every plugin's DOM. Rewritten on the way: the selector engine (su-editor's read words; this one
parses selectors whole), the events (su-editor kept listeners in its own table; here they are
Verve's, on the nodes' objects, travelling Verve's dispatch), the objects' lifetimes (links
between objects instead of strong references for connected nodes), `MutationObserver` (real
records instead of a node list once a turn), `XMLHttpRequest` (on Verve's `fetch` instead of
su-editor's network thread), the parser (raw text, implied end tags, foreign content).

Left in su-editor, as Obsidian's: `createEl` and the rest of Obsidian's DOM extensions,
`instanceOf`, `onClickEvent`, its user agent and `app://obsidian.md` (now `Options`), and
everything of its app — CodeMirror, Markdown, the vault, views, menus, its generated API,
`moment`.

## Checking it

| Test | What it holds |
|---|---|
| `tests/react.rs` | React 18 and react-dom 18, unmodified from npm (`tests/fixtures/node_modules`, production builds with their licences): `createRoot().render()` of state, effects, layout effects, a keyed list, conditional rendering, a controlled input and a controlled checkbox; presses re-rendering; typing (and Backspace, and an input method's text) into the controlled input; unmounting with every effect cleaned up and the nodes reclaimed |
| `tests/react_dev.rs` (ignored; fetches React with `verve::npm`) | the development builds doing the same with nothing on the console |
| `tests/dsh.rs` (ignored; fetches DSH's packages with `verve::npm`) | `@deepseek-ai/dsh-client-ui-primitives`' `Button` and `SettingsValueField` rendering and answering presses and typing, their CSS modules in the cascade; the web-search provider's settings page (`dsh-client-ui-settings-web-search/client`) loaded through `window.__ModuleLoader__`, registered into a stand-in slot, typed into and saved through its form's `mutate` |
| `tests/turndown.rs` | turndown 7 converting HTML to Markdown through `DOMParser`, through `createHTMLDocument` in its browser build, and from an element — matching what it answers under Node |
| `tests/css.rs` | a `<style>`'s rules and inline styles cascading with `var()`, inheritance, `!important` and computed font sizes; `:hover` from the pointer; `@media` and `matchMedia` from the viewport; a sheet added later; only the changed partition dirtied |
| `tests/input.rs` | the pointer's events in order, `:hover`, `:active`, double clicks and the other buttons, the wheel; geometry from the painted boxes; scrolling both ways; typing, the caret's keys, select-all, Enter submitting (with `FormData(form)`), Tab, Space and labels on a box, focus events |
| `tests/observers.rs` | `MutationObserver` records (child lists, attributes with old values and filters, text) in a microtask, `takeRecords`, `disconnect`; `ResizeObserver` and `IntersectionObserver` from reported layouts |
| `tests/arena.rs` | ten thousand discarded rows reclaimed, what the document or a script holds kept with its listeners and expandos, a tree held by a leaf kept whole — ported from su-editor's |
| `tests/platform.rs` | the platform's pieces libraries test for (ported from su-editor's), and `XMLHttpRequest` through `fetch` |
| `tests/basics.rs`, `tests/extend.rs`, `tests/isolate.rs`, `tests/verve_globals.rs` | building and querying, events through the tree, parsers; an embedder's methods; the document in an isolate, snapshots read from another thread; Verve's own globals unchanged with a document, and `verve-dom run` |

`cargo test -p verve-dom` runs all but the two that fetch packages; `cargo test -p verve-dom
--test dsh --test react_dev -- --ignored` runs those (set `VERVE_DOM_DSH`/`VERVE_DOM_REACT_DEV`
to keep the installed packages somewhere of your own; about 30 MB for DSH's).

## Not yet

- No layout: where things are is the drawer's to work out and report. Until it does, boxes
  are empty (the viewport for `<html>` and `<body>`).
- Lists of nodes (`childNodes`, `children`, `getElementsBy…`) are arrays taken when asked, not
  live; a script that keeps one and changes the tree sees the tree as it was.
- No composition events (an input method's text arrives committed, through `insert_text`), no
  typing into `contenteditable` regions, no drag and drop or touch input from the interface.
- Custom elements: an element of a defined tag made by `createElement` is of its class, and
  hears `connectedCallback`/`disconnectedCallback`; one the parser makes is not upgraded, and
  `attributeChangedCallback` is not called.
- Shadow roots are held and queried, but their styles are not scoped and events are not
  retargeted across them.
- CSS: `calc()`, `env()` and other functions stay as written; pseudo-elements (`::before`)
  match nothing, so generated content is not in the snapshot; `@container`, animations and
  transitions are not run (`animate()` answers a finished animation).
- Nothing loads: `<img>`, `<link rel=stylesheet>`, `<script src>`, `<iframe>` and media fire
  no `load`; a canvas measures text and paints nothing; no WebGL.
- `document.cookie` lives in memory; `XMLHttpRequest`'s synchronous requests are sent
  asynchronously.
