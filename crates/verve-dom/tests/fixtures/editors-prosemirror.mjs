// ProseMirror (prosemirror-view with example-setup's rich-text plugins) on the document, as
// `tests/editors.rs` drives it. Everything comes from npm, unmodified.
import { EditorState } from 'prosemirror-state';
import { EditorView } from 'prosemirror-view';
import { schema } from 'prosemirror-schema-basic';
import { exampleSetup } from 'prosemirror-example-setup';

globalThis.pm = { EditorState, EditorView, schema, exampleSetup };
