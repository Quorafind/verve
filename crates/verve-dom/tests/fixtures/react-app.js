// A small React 18 app: state, effects, a keyed list, conditional rendering and a controlled
// input, rendered with `createRoot` into the document. What it does is noted in `log`.
'use strict';

const React = require('react');
const { createRoot } = require('react-dom/client');
const h = React.createElement;
const log = (globalThis.log = []);

function Todo({ item, onRemove }) {
  React.useEffect(() => {
    log.push('mount ' + item);
    return () => log.push('unmount ' + item);
  }, []);
  return h(
    'li',
    { className: 'todo', 'data-item': item },
    h('span', { className: 'label' }, item),
    h('button', { className: 'remove', onClick: () => onRemove(item) }, 'x'),
  );
}

function App() {
  const [count, setCount] = React.useState(0);
  const [text, setText] = React.useState('');
  const [items, setItems] = React.useState(['a', 'b']);
  const [shown, setShown] = React.useState(true);
  const [checked, setChecked] = React.useState(false);
  const field = React.useRef(null);
  React.useEffect(() => {
    document.title = 'count ' + count;
  }, [count]);
  React.useLayoutEffect(() => {
    log.push('layout ' + (field.current instanceof HTMLInputElement));
  }, []);
  return h(
    'div',
    { id: 'app', style: { padding: 4, color: 'red' } },
    h('button', { id: 'inc', onClick: () => setCount((c) => c + 1) }, 'count: ' + count),
    h('input', {
      id: 'field',
      ref: field,
      value: text,
      placeholder: 'type',
      onChange: (e) => setText(e.target.value.toUpperCase()),
    }),
    h('p', { id: 'echo' }, text),
    h('label', null,
      h('input', { id: 'check', type: 'checkbox', checked, onChange: (e) => setChecked(e.target.checked) }),
      checked ? 'on' : 'off'),
    shown ? h('span', { id: 'maybe' }, 'shown') : null,
    h('button', { id: 'toggle', onClick: () => setShown((s) => !s) }, 'toggle'),
    h('ul', { id: 'list' },
      items.map((i) =>
        h(Todo, { key: i, item: i, onRemove: (r) => setItems((list) => list.filter((x) => x !== r)) }),
      ),
    ),
    h('button', { id: 'add', onClick: () => setItems((list) => [...list, 'c' + list.length]) }, 'add'),
  );
}

const container = document.createElement('div');
container.id = 'root';
document.body.appendChild(container);
const root = createRoot(container);
root.render(h(App));
globalThis.unmount = () => root.unmount();
