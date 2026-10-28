// More of what React apps do: a portal, SVG, a controlled select and textarea, hover through
// React's own enter/leave, focus and blur, key handlers, a form's submit, raw HTML, and a
// layout read in a layout effect.
'use strict';

const React = require('react');
const { createPortal } = require('react-dom');
const { createRoot } = require('react-dom/client');
const h = React.createElement;
const log = (globalThis.log = []);

function Form() {
  const [fruit, setFruit] = React.useState('pear');
  const [notes, setNotes] = React.useState('first');
  const [hovered, setHovered] = React.useState(false);
  const [focused, setFocused] = React.useState('none');
  const [width, setWidth] = React.useState(-1);
  const box = React.useRef(null);
  React.useLayoutEffect(() => {
    setWidth(box.current.getBoundingClientRect().width);
  });
  return h(
    'form',
    {
      id: 'form',
      onSubmit: (e) => {
        e.preventDefault();
        log.push('submit ' + fruit + ' ' + notes);
      },
    },
    h('select', { id: 'fruit', value: fruit, onChange: (e) => setFruit(e.target.value) },
      ['apple', 'pear', 'plum'].map((f) => h('option', { key: f, value: f }, f))),
    h('textarea', {
      id: 'notes',
      value: notes,
      onChange: (e) => setNotes(e.target.value),
      onFocus: () => setFocused('notes'),
      onBlur: () => setFocused('left notes'),
      onKeyDown: (e) => {
        if (e.key === 'Escape') log.push('escape');
      },
    }),
    h('div', {
      id: 'hover',
      ref: box,
      onMouseEnter: () => setHovered(true),
      onMouseLeave: () => setHovered(false),
    }, hovered ? 'in' : 'out'),
    h('p', { id: 'status' }, fruit + '|' + notes + '|' + focused + '|' + width),
    h('div', { id: 'raw', dangerouslySetInnerHTML: { __html: '<b>bold</b> &amp; plain' } }),
    h('svg', { id: 'icon', viewBox: '0 0 10 10', width: 10 },
      h('path', { d: 'M0 0L10 10', strokeWidth: 2, className: 'line' })),
    h('button', { id: 'submit', type: 'submit' }, 'Send'),
    createPortal(h('div', { id: 'portal' }, 'over everything'), document.body),
  );
}

const container = document.createElement('div');
container.id = 'root';
document.body.appendChild(container);
createRoot(container).render(h(Form));
