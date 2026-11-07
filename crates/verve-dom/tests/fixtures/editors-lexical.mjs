// Lexical with its rich-text and history plugins on the document, as `tests/editors.rs` drives
// it. Everything comes from npm, unmodified.
import { createEditor, $getRoot, $createParagraphNode } from 'lexical';
import { registerRichText } from '@lexical/rich-text';
import { createEmptyHistoryState, registerHistory } from '@lexical/history';

globalThis.lx = {
  createEditor,
  $getRoot,
  $createParagraphNode,
  registerRichText,
  createEmptyHistoryState,
  registerHistory,
};
