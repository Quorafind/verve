# P60 JS 基准与剖析方法

**依赖**：无  **规模**：S  **修改**：新建 `scripts/js-bench/`、`docs/electron/perf-js.md`；根 `Cargo.toml` 只加一个 `[profile.profiling]`

性能线（README 第 5 节「性能线」）的第一个包。后面的 P61、P64–P66 都用它测量、用它的剖析方法找热点，所以它先合。

## 目标

1. 把 [`perf-baseline.md`](../perf-baseline.md) 第 3 节的 JS 吞吐测量搬进仓库，任何人一条命令可重现，Verve 与 Node 同表对比。
2. 加一组**规模探针**：同一操作在 N 和 4N 规模下各跑一次，比值远超 4 的就是超线性（`split`、`Map`/`Set` 的 `delete`、正则扫描都是这么发现的）。
3. 写出 Windows 上剖析 release `verve` 的方法，让后续包的「热点在哪」有据可查。

## 背景（先读）

- [`perf-baseline.md`](../perf-baseline.md) 全文，特别是第 3 节与「Findings: super-linear built-ins」。基线用的 `bench.js` 附在本文末尾。
- `docs/jit.md`「Results (phase 2)」：仓库里已有的测量写法；「Phase 2, left over」里引用的 `callLoop` 剖析就是用采样剖析器得到的。
- `src/cli.rs` 开头的表：`--jitless`、`VERVE_JIT`、`VERVE_JIT_THRESHOLD`。
- 根 `Cargo.toml` 的 `[profile.release]`（P01）：`strip = "symbols"`，剖析需要另一个保留符号的 profile。

## 要做的事

### 1. 吞吐套件（`scripts/js-bench/bench.js`）

- 从附录的 `bench.js` 出发，保留现有 10 项，**名字与规模不变**，这样数字能和基线直接比。
- 另加 4 项更像应用代码的负载，全部自包含、不依赖 npm：
  - `vdom`：构造约 2,000 个节点的虚拟 DOM 树（普通对象 + 数组 + 字符串 key），与改了 10% 的新树做 diff，生成补丁列表。
  - `tokenize`：用 `sticky` 正则逐个取词，扫描一段约 200 KB 的生成源码。
  - `template`：模板字符串与 `Array.prototype.map`/`join` 拼一个 1,000 行的 HTML 表格。
  - `closure`：`map`/`filter`/`reduce` 链与闭包回调，100 万元素。
- 输出保持一行 JSON（`{"fib": 297.9, …}`），驱动程序解析。

### 2. 规模探针（`scripts/js-bench/scaling.js`）

- 每个探针是 `(n) => void`，驱动用 n 与 4n 各计时一次（先各热身一次），报告 `t(4n) / t(n)`。比值 > 8 记为**超线性**，> 5 记为**可疑**。
- 至少覆盖：`split`（单字符与多字符分隔符）、`indexOf`/`includes`/`lastIndexOf`、`replace`/`replaceAll`（字符串与正则模式）、
  `matchAll`、全局正则的 `exec` 循环、`Map`/`Set` 的 `set`/`delete`/迭代（含边迭代边删）、`Array` 的 `shift`/`unshift`/`splice`/`indexOf`/`includes`、
  `delete obj[k]` 后的字典模式对象读写、`JSON.stringify` 长字符串、`String.prototype.repeat`/`padStart`、Latin-1 与含非 Latin-1 字符的两种字符串各一遍。
- 规模由探针自己定，保证 n 在解释器下 10–200 ms。超时（单次 > 20 s）记为超线性并跳过 4n。

### 3. 驱动（`scripts/js-bench/run.sh`）

- `run.sh [--runs N] [--only test] [--scaling] <verve.exe> [node]`：每项一个新进程，默认 5 次，报告中位与最好；
  对 Verve 同时跑 JIT 开（默认）与 `VERVE_JIT=0` 两列（二进制没带 JIT 时只跑一列，从 `--jitless` 的报错或 `jit_stats` 判断均可）。
- `node` 参数可省；给了就加 Node 列和比值列。输出 Markdown 表，可直接贴进报告。
- bash 写，和 `scripts/size-report.sh` 一个风格；不依赖 Node 才能跑。

### 4. 剖析方法

- 根 `Cargo.toml` 加：
  ```toml
  # Release code with symbols and line tables, for a sampling profiler.
  [profile.profiling]
  inherits = "release"
  strip = "none"
  debug = "line-tables-only"
  ```
  它只影响 `--profile profiling` 的构建，不改 release 产物与体积。
- 在 `perf-js.md` 写出一份**实际跑通**的配方：用 [samply](https://github.com/mstange/samply)（Windows 上走 ETW，需管理员终端）
  或 Windows Performance Recorder 采样 `verve run scripts/js-bench/bench.js <test>`，怎么看自顶向下与按函数自耗时。
  配方里给出 `typed` 与 `regex` 两项的前 10 个自耗时函数作为示例。

### 5. 报告（`docs/electron/perf-js.md`）

照 `perf-baseline.md` 的格式（机器、条件、构建、最好/中位、噪声说明）写第一版：吞吐表（14 项）、规模探针表、剖析配方与示例。
后续包合并时在这里追加一行「某包合并后」的数字，不另开文件。

## 验收

- `bash scripts/js-bench/run.sh <release verve.exe> node` 跑出 14 项的表；原 10 项的解释器中位数与 `perf-baseline.md` 相差在 ±15% 内（同一台机器、空闲时）。
- `run.sh --scaling` 把 `split`、`Map`/`Set` `delete`、正则扫描三项标为超线性（P61 合并前）。
- `perf-js.md` 的剖析示例是真实采样得到的函数名，不是推测。
- `cargo build --profile profiling --bin verve` 成功；release 构建的体积不变（`scripts/size-report`）。

## 不做

不优化任何东西（发现的问题写进 `perf-js.md` 交给 P61、P64–P66）；不做应用级（窗口、首帧、IPC）测量，那是 P42 的 `scripts/electron-bench/`；
不引入 JS 基准框架或 npm 依赖。

## 附录：基线所用的 `bench.js`

```js
const N = process.argv[2];
const now = () => performance.now();
function fib(n){return n<2?n:fib(n-1)+fib(n-2);}
class A{constructor(x){this.x=x;} get(){return this.x;} add(o){return new A(this.x+o.get());}}
class B extends A{get(){return this.x+1;}}
let sink;
const tests = {
 fib: ()=>fib(30),
 alloc: ()=>{let a=[];for(let i=0;i<300000;i++){a.push({i,s:'k'+i,n:{v:i}});if(a.length>1000)a=[];}sink=a;},
 concat: ()=>{let s='';for(let i=0;i<20000;i++)s+='ab'+i;sink=s.length;},
 splitjoin: ()=>{const parts=[];for(let i=0;i<20000;i++)parts.push('ab'+i);const s=parts.join('a');const p=s.split('a');sink=p.join('-').length;},
 regex: ()=>{let t='';for(let i=0;i<5000;i++)t+='foo'+i+' bar@example.com baz 2024-01-'+(i%28+1)+'\n';let c=0;for(const m of t.matchAll(/(\w+)@(\w+)\.com/g))c++;c+=t.replace(/\d{4}-\d\d-\d+/g,'D').length;sink=c;},
 json: ()=>{const o=[];for(let i=0;i<70000;i++)o.push({id:i,name:'item'+i,tags:['a','b','c'],nested:{x:i*1.5,y:'yy'+i,z:[1,2,3]}});const s=JSON.stringify(o);if(s.length<5e6)throw 1;const p=JSON.parse(s);sink=p.length+JSON.stringify(p).length;},
 sort: ()=>{const a=new Array(1000000);let x=12345;for(let i=0;i<a.length;i++){x=(x*1103515245+12345)&0x7fffffff;a[i]=x;}a.sort((p,q)=>p-q);sink=a[0];},
 mapset: ()=>{const m=new Map(),s=new Set();for(let i=0;i<20000;i++){m.set('k'+i,i);s.add(i);}for(let i=0;i<20000;i+=2){m.delete('k'+i);s.delete(i);}let c=0;for(const [k,v] of m)c+=v;sink=c+s.size;},
 oo: ()=>{let a=new A(0);const b=new B(1);for(let i=0;i<1000000;i++){a=a.add(b);}sink=a.x;},
 typed: ()=>{const f=new Float64Array(1000000);for(let i=0;i<f.length;i++)f[i]=i*0.5;let s=0;for(let r=0;r<5;r++)for(let i=0;i<f.length;i++){f[i]=f[i]*1.0001+1;s+=f[i];}sink=s;},
};
const names = N? [N] : Object.keys(tests);
const out = {};
for (const n of names){
  tests[n](); const t0=now(); tests[n](); const t1=now();
  out[n]=+(t1-t0).toFixed(1);
}
console.log(JSON.stringify(out));
```

## 状态

（实现者完成后填写）
