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

完成（2026-11-03，分支 `electron/p60-js-bench`）。数字是在 `docs/electron-plan` 的 `cde79ef3` 上测的；
分支现在位于 `471082de`（已含 P61、P63 的合并），本包的文件在其上没有变化。

### 做了什么

- `scripts/js-bench/bench.js`：基线那 10 项，名字与规模逐字不变；另加 `vdom`（2,000 节点、每轮改 1/10、5 轮 diff）、
  `tokenize`（sticky 正则扫 200 KB 生成源码）、`template`（1,000 行 HTML 表格）、`closure`（100 万元素 map/filter/reduce）。
  一行 JSON 输出，`--list` 给名字数组，单项测试可单独跑。
- `scripts/js-bench/scaling.js`：44 个探针（字符串 26 个含 Latin-1 / 非 Latin-1 两遍、数组 5 个、Map/Set 7 个、字典对象 2 个、JSON 3 个、`concat` 1 个）。
  每个探针自带 n，t(n) 校准到解释器下 10–200 ms（实测 11.6–191.4 ms）；n 与 4n 同进程各测两遍取快，报告 `t(4n)/t(n)`。
  **n 或 4n 任一次跑超过 20 s** 记为超线性（`"superLinear": true`），并跳过 4n，不给比值；t(n) 落在
  1,250 ms 到 20 s 之间的探针报 `"skipped"` 且不给判定（不是超线性），因为 4n 可能越过 20 s，
  而规模选大了的线性操作不该被记成 bug。
- `scripts/js-bench/run.sh`：`[--runs N] [--only test] [--scaling] <verve.exe> [node]`；每项每引擎一个新进程，中位与最好；
  按二进制里有无 `VERVE_JIT_THRESHOLD` 判断是否带 JIT，带了就同时跑 JIT 与 `VERVE_JIT=0` 两列，没带只跑解释器列；
  给 `node` 就有 Node 列与比值列；输出 Markdown 表。只用 bash + awk/sed，不依赖 Node。
- 根 `Cargo.toml` 加 `[profile.profiling]`（`inherits = "release"`、`strip = "none"`、`debug = "line-tables-only"`）。
- `docs/electron/perf-js.md`：14 项吞吐表、44 项规模表、发现（交给 P61/P64/P65）、剖析配方（samply 与 WPR/xperf 两条路）。
  规模表是 P61 之前的基线，文中已注明 P61 修好了 `split`、`Map`/`Set` 删除与正则扫描，它的数字走「After each package」。

### 验收

- **14 项吞吐表：跑过**（`bash scripts/js-bench/run.sh <release verve.exe> node`，`--runs 5`）。
  原 10 项的解释器中位数与 `perf-baseline.md` 第 3 节相差最大 13%（`splitjoin` +13%、`alloc` +10%、`fib` +6%，其余 ≤8%），**都在 ±15% 内**。
  `alloc` 第一遍 5 次读数偏高 24%，用 `--runs 11` 单独复测到 +10%，报告里用的是复测值并写明了这一点。
- **规模探针：跑过**（`run.sh --scaling`）。`split`（4 个变体 16–17×）、`Map`/`Set` 的 `delete`（25–29×）、
  正则扫描（`matchAll` 13×、`exec` 循环 17×、`replace` 正则 17×）都标为 **super-linear**，验收这一条达成。
  另外还发现 `replaceAll`（字符串模式）、`delete obj[k]` 后的字典对象、字符串 `+=` 也是超线性，一并写进报告。
  两轮测量都没有探针触发超时，也没有探针落到 `skipped`。
- **`cargo build --profile profiling --bin verve --no-default-features --features wasm,jit`：成功**
  （退出码 0，`profiling/verve.exe` 23,580,160 字节，旁边 `verve.pdb` 68,198,400 字节）。带默认 feature 的同名构建也能过，
  但那样剖析的不是被测量的程序（无 `jit`、带 npm/typescript），配方与状态里用的都是上面这条。
- **release 体积不变**：`cargo build --release --bin verve --no-default-features --features wasm,jit` 产物 23,577,600 字节，
  与 `perf-baseline.md` 记的完全相同（`size.md` 的 `size-baseline` 是 35,812,352，那是另一套 feature，未动）。
- **剖析示例：未完成，留给总控。** 配方写成了实际可用的命令，但「`typed` 与 `regex` 前 10 个自耗时函数」两表没有数据：
  早先跑 samply 时它在用户桌面上弹了提权与浏览器窗口，被总控叫停，此后不再运行任何会弹窗的工具。这一节在 `perf-js.md` 里
  标着「待总控实测」，没有编造函数名。

### 已知问题

- 机器是共享的：测量期间另有两个实现者在做 release 构建，有整段时间负载 100%（此时全表比空闲时慢 1.6–2.0×）。
  报告里每张表都写了测量前后的负载读数，数字取自负载较低的那几轮（§1：17.2–35.0% → 8.4–21.5%；§2：4.1–11.9% → 2.5–7.1%）。
  引用绝对值前建议重测；比值稳健得多。`regex` 与 `array-shift` 各单独重测 5 次：`regex` 负载
  12.8% → 12.3%，`array-shift` 32–70% → 25–34%（本文件里负载最高的一轮，报告里也写了）。
- `template` 的 Node 侧只有 0.2 ms，比值被调度噪声放大（±0.1 ms 就是 ±50%），只能当「够快」看。
- `array-shift` **不是**超线性，先前那条 29× 是缓存断崖：Boa 稠密数组的 `shift` 是 `Vec::remove(0)`（一次 memmove），
  单次 O(n)，数据在缓存内时比值就是 4。探针改到 n=50,000（i32 下 200 KB，4n 800 KB，都在 1 MB L2 内）后比值 4.2×；
  n=100,000 时 4n 的 1.6 MB 出了 L2，比值跳到 12.5× 而算法没变。交给 P65 的是「单次 memmove 的成本」，不是规模缺陷。
- 探针里刻意不用 `slice()` 复制数组：Verve 下 `slice` 20 万元素本身就是几十毫秒，会把要测的 `shift` 淹掉；
  代价是同探针的两次计时共享被改动的数组，固定调用次数下影响 ≤2%（unshift/splice 是 100/5,000），已在代码注释里写明。
- 探针没有按尺寸单独预热：`fastest` 把同一次计时做两遍取较快的一遍，第一遍兼作预热，这一点写在 `perf-js.md` 的条件里。
- `regex` 的第一版套件把基线的 `/\d{4}-\d\d-\d+/g` 写成了 `/\d{4}-\d\d-\d\d/g`，只匹配 5,000 个日期里的 3,389 个。
  任务书要求逐字不变，已恢复 `\d+` 并单独跑 5 次（Node 1.7、解释器 475.7 ms），与基线那一行相差 +3%。

### 给其他包的备注（P64–P66）

- **测量工具用法**：`bash scripts/js-bench/run.sh [--runs N] [--only test] [--scaling] <verve.exe> [node]`。
  吞吐项名：`fib alloc concat splitjoin regex json sort mapset oo typed vdom tokenize template closure`；
  探针名见 `scaling.js --list`。两边都支持 `--only` 单跑。
- 新增探针或测试：在 `scaling.js`/`bench.js` 里按现有 `probe(...)`/`stringProbe(...)`/`tests` 的写法加一条即可，
  `run.sh` 通过 `--list` 自动发现，不用改驱动。
- **数字写进 `docs/electron/perf-js.md`**：按文末「After each package」的两张模板表追加行，格式（`median (best)`、列名）与 §1/§2 一致，不要另开文件。
- 最值得先看的两个方向（报告里有数据）：正则扫描在 Latin-1 与非 Latin-1 编码下行为不同（P64 的重点线索）；
  `array-shift` 的单次 memmove 成本（P65，属常数而非规模）。报告里 `split`、`Map`/`Set` 删除与正则扫描那几行
  是 P61 之前的基线，P61 已把它们改成线性，数字见 `P61-superlinear-builtins.md`「状态」。
