import { readA } from './cycle-b.mjs';
export let a = 1;
export function readB() { return readA(); }
