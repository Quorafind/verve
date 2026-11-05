# react-vite

A React counter built by Vite. Only `dist/` is committed: tests do not build anything.

Built with React and ReactDOM 19.3.1, Vite 7.3.7 and `@vitejs/plugin-react` 5.2.0, from a folder
holding `index.html` (a `<div id="root">` and `<script type="module" src="/src/main.jsx">`),
`vite.config.js` and `src/main.jsx`:

```js
// vite.config.js
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
export default defineConfig({ base: './', plugins: [react()] });
```

```jsx
// src/main.jsx
function Counter() {
  const [count, setCount] = useState(0);
  return (
    <main>
      <h1>React on Verve</h1>
      <button id="inc" onClick={() => setCount(count + 1)}>count is {count}</button>
    </main>
  );
}
createRoot(document.getElementById('root')).render(<StrictMode><Counter /></StrictMode>);
```

```sh
npm install react@19 react-dom@19 vite@7 @vitejs/plugin-react@5
npx vite build        # writes dist/ with relative URLs (base './')
```
