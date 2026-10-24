import {nanoid, customAlphabet} from 'nanoid';
import {Chalk} from 'chalk';
import {addDays, format} from 'date-fns';
import {z} from 'zod';

const id = customAlphabet('ab', 8)();
const chalk = new Chalk({level: 0});
const schema = z.object({name: z.string().min(2), age: z.number().int()});
const result = [
  nanoid(12).length === 12 && id.length === 8 && /^[ab]+$/.test(id),
  chalk.red.bold('native') === 'native',
  format(addDays(new Date(2024, 0, 31), 2), 'yyyy-MM-dd'),
  schema.safeParse({name: 'Rust', age: 4}).success,
  schema.safeParse({name: 'x', age: 1.5}).success,
];
console.log(JSON.stringify(result));
