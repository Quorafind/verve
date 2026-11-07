// CodeMirror 6 (state, view, and the commands' history and default keymap) on the document, as
// `tests/editors.rs` drives it. Everything comes from npm, unmodified.
import { EditorState } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';

globalThis.cm = { EditorState, EditorView, keymap, defaultKeymap, history, historyKeymap };
