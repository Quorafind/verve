# P12 IPC

**依赖**：P02（消息类型、Hub）、P03（`verve::clone::{Cloned, clone_out, clone_in}` 公开；跨 isolate 的 `MessagePort`）  **规模**：M  **修改**：`crates/verve-electron/src/api/ipc_main.rs`、`api/ipc_renderer.rs`、`api/message_port_main.rs`（新）、`src/ipc.rs`（新：路由与序列化）、`main_guest.rs`/`renderer_guest.rs` 的入站分发

## 目标

`ipcMain` / `ipcRenderer` / `webContents.send` / `webContents.ipc` / `MessageChannelMain` 与 Electron 39 行为一致：
事件参数、处理器查找顺序、错误形态、同步消息、端口转移。主进程与渲染器在同一进程的不同线程，
所以 IPC 应当比 Electron 快一个数量级：`invoke` 往返 p50 ≤ 0.2 ms。

## 背景（先读）

- `src/worker.rs`：`Cloned`（第 81 行）、`clone_out`（第 103 行）、`clone_in`（第 224 行）—— 页面和 Worker 之间的结构化克隆；
  P03 把它们公开为 `verve::clone`。注意现有 `Cloned` **没有** `RegExp`、`ArrayBuffer`（与视图区分）、各类型化数组的具体类型、
  `Blob`/`File`、`MessagePort` 转移这些变体——见下文"序列化"。
- `src/messaging.rs`：`MessageChannel`/`MessagePort` 目前只在一个 realm 内；跨 isolate 的端口由 P03 追加（见"端口"）。
- `src/events.rs`：`emit`、监听者存储（`_events`），`ipcMain`/`ipcRenderer` 是 `EventEmitter` 实例。
- P02 `msg.rs`：`ToRenderer::{Ipc, InvokeReply}`、`FromRenderer::{Ipc, Invoke, SendSync}`。

## 接口契约

### 0. 对真实 Electron 39.8.10 观测后的修订（以本节为准，与下文冲突处以本节为准）

观测程序在 `tests/electron/ipc/observe/`（`npx electron --js-flags=--expose-gc .`），结果冻结在 `crates/verve-electron/tests/ipc.rs`。

- **事件对象**：`IpcMainEvent`（`send`）的可枚举键依次为 `type, sender, senderFrame, frameId, processId, frameTreeNodeId, reply`；
  **没有 `ports`**（只有 `postMessage` 来的消息才有 `ports`，其余无此键，不是 `[]`）。`sendSync` 的事件另有不可枚举的 `returnValue` 访问器。
  `IpcMainInvokeEvent` 没有 `reply`。`frameTreeNodeId` 数字，主框架为 `1`；`senderFrame` 就是 `webContents.mainFrame`（同一个对象），
  发送者销毁后 `event.senderFrame` 为 `null`、`event.reply` 静默。`IpcRendererEvent` 的键为 `sender, ports`，`ports` 恒为数组。
  Electron 把 `_replyChannel` 也漏成了可枚举键，Verve 不模仿。
- **分发**：`invoke` 不发 `ipc-message`；`ipc-message` → `webFrameMain.ipc` → `webContents.ipc` → `ipcMain`（`sendSync` 同，事件名 `ipc-message-sync`）；`ipcRenderer.postMessage` 的消息**不发**contents 的 `ipc-message`，只有后三者。
- **注册**：`ipcMain.handle('x', 5)` → `TypeError: Expected handler to be a function, but found type 'number'`；channel 不是字符串不检查；
  `handle` / `removeHandler` 返回 `undefined`；`ipcMain.on` 返回 `ipcMain`。
- **渲染器参数错误**是 gin 形态：`ipcRenderer.send()` 抛 `TypeError: Error processing argument at index 1, conversion failure from undefined`
  （channel 在索引 1；数字/对象则 `from ` 后为空）；`postMessage()` 在索引 0；`invoke(...)` 同形态但**拒绝**而不是同步抛。
  参数不可克隆：`Error: An object could not be cloned.`（`send`/`sendSync` 同步抛，`invoke` 拒绝）。`postMessage` 的 transfer 不是端口数组 →
  `TypeError: Invalid value for transfer`。`sendToHost` 在没有宿主 `<webview>` 时**静默无操作**（不抛 `ERR_NOT_SUPPORTED`）。
- **主进程发往渲染器**：`webContents.send()` 没有 channel → `TypeError: Missing required channel argument`；参数不可克隆**不抛**，
  只 `console.error('Error sending from webFrameMain: ', new Error('Failed to serialize arguments'))`；`sendToFrame` 返回布尔
  （`1`、`[processId, 1]` 为 `true` 并投递，其余 `false`）；`postMessage` 的 transfer 无效 → `Error: Invalid value for transfer`；
  已销毁 → `TypeError: Object has been destroyed`（`send`/`sendToFrame`/`postMessage`/`mainFrame`），`id` 与 `ipc` 仍可读。
- **`invoke` 错误**：`String(e)` 的形态与下文一致；抛 `null` 时 Electron 自己的实现出错，渲染器得到 `reply was never sent`（Verve 照此给出，不打 `UnhandledPromiseRejectionWarning`）。
  主进程 `console.error("Error occurred in handler for '<channel>':", e)`；返回值不可克隆则处理器视为失败，`String(e)` 为 `Error: An object could not be cloned.`。
- **`sendSync` 无人答复**：Electron 不抛错也不返回 `undefined`——事件对象被回收后，渲染器得到 **`{ error: 'reply was never sent' }`**（这是它的 `ReplyChannel` 析构时的默认应答；
  未回收前一直阻塞）。无监听者时先 `console.warn("WebContents #<id> called ipcRenderer.sendSync() with '<channel>' channel without listeners.")`，之后同样如此。
  Verve 的差别只有时机：没有监听者时立刻给出这个值；有监听者而事件已不可达时，在下一次回收检查（约 100 ms 内）给出，不等 GC；仍在被持有（异步赋值）则继续等。
  `returnValue` 只认第一次赋值；赋不可克隆的值抛 `Error: An object could not be cloned.` 且视为未答复。上文「`sendSync reply was dropped`」与「立即回 `undefined`」作废。
- **克隆**与 HTML 结构化克隆一致，具体：`Buffer` 到达为 `Uint8Array`；`Error` 只保留标准名（其余变 `Error`，`stack` 仍是原文）、`message`、`stack`、`cause`，自定义属性丢失；
  类实例、`null` 原型对象变普通对象；`Symbol` 键丢；`RegExp.lastIndex` 归零。DOM 节点、`Blob`/`File`、`URL` 在 Electron 里变成 `{}`，`Event` 变 `{ isTrusted: false }`，`el.style`/`classList`/`dataset`/`children`/`attributes` 抛 `An object could not be cloned.`（用户的 `new Proxy({}, {})` 同）；Verve 的 `Blob`/`File` 仍整体克隆
  （比 Electron 有用，且无应用依赖 `{}`）。视图（如池里的 `Buffer`）Electron 带着整个底层 `ArrayBuffer` 与 `byteOffset`；Verve 只带视图的字节。
- **端口**：`MessagePortMain` 用 `on('message')` 不会自动开始，须 `start()`；`'message'` 事件键为 `data, ports`；`close()` 之后 `postMessage`/`start`/`close` 静默，
  本端立即发 `'close'`，对端只有已 `start()` 的才收到 `'close'`；端口被转移后 `postMessage`/`start`/`close` 静默，再转移 →
  主进程 `Error: Port at index 0 is already neutered.`、渲染器 `TypeError: Invalid value for transfer`；`port.postMessage(x, [非端口])` →
  `TypeError: Port at index 0 is not a valid port`，transfer 不是数组 → `TypeError: transferables must be an array of MessagePorts`；
  `MessageChannelMain()` 不带 `new` → `TypeError: Class constructor MessageChannelMain cannot be invoked without 'new'`；`MessagePortMain` 的原型方法只有 `postMessage/start/close`。
  转移名单里同一个端口两次 → `Error: Port at index 1 is a duplicate.`（`webContents.postMessage` 同）；名单含发送端口自己 → `Error: Port at index 0 contains the source port.`；已关闭或已转移的端口调 `postMessage`：什么都不做，不克隆消息、不动名单里的端口（坏名单也不抛）。
- **`require`**：页面只有在 `nodeIntegration: true`、`contextIsolation: false` 且未沙箱时有全局 `require`（`sandbox: true` 时没有；`nodeIntegration` 为真时 `sandbox` 默认假；只有 `nodeIntegration` 而默认隔离，或只有 `sandbox: false`，都没有）。


### 1. 主进程：`ipcMain`（EventEmitter 单例）

| 成员 | 行为 |
|---|---|
| `on/once/addListener/off/removeListener/removeAllListeners(channel, listener)` | 普通 EventEmitter；监听者签名 `(event: IpcMainEvent, ...args)` |
| `handle(channel, listener)` | 注册 `invoke` 处理器 `(event: IpcMainInvokeEvent, ...args) => any | Promise<any>`；同一 channel 已有处理器 → 抛 `Error: Attempted to register a second handler for '<channel>'` |
| `handleOnce(channel, listener)` | 处理一次后自动移除 |
| `removeHandler(channel)` | 移除；不存在时静默 |

**`IpcMainEvent`**（每条消息新建一个普通对象）：

| 字段 | 值 |
|---|---|
| `type` | `'frame'` |
| `processId` | 渲染器的"进程号"：用 `ContentsId` 生成的稳定整数（与 `webContents.getProcessId()` 相同） |
| `frameId` | 主框架 `1`（v1 无子框架） |
| `sender` | 发送者的 `WebContents` 对象（P13） |
| `senderFrame` | `WebFrameMain`（P13 的最小实现；webContents 已销毁时为 `null`） |
| `ports` | `MessagePortMain[]`（`ipcRenderer.postMessage` 转移来的，否则 `[]`） |
| `returnValue` | 仅 `sendSync`：设置即答复（见下） |
| `reply(channel, ...args)` | 等同于向 `senderFrame` 发送（webContents 已销毁时静默丢弃） |

**`IpcMainInvokeEvent`**：`type`、`processId`、`frameId`、`sender`、`senderFrame`（无 `reply`、`returnValue`、`ports`）。

### 2. 分发顺序（照 Electron `lib/browser/api/web-contents.ts`）

渲染器 `send(channel, ...args)` 到达主进程后：
1. `webContents.emit('ipc-message', event, channel, ...args)`
2. `webFrameMain.ipc.emit(channel, event, ...args)`（v1 主框架）
3. `webContents.ipc.emit(channel, event, ...args)`
4. `ipcMain.emit(channel, event, ...args)`

`sendSync` 同上，第 1 步事件名为 `ipc-message-sync`。全程无任何监听者 → 主进程 `console.warn`：
`WebContents #<id> called ipcRenderer.sendSync() with '<channel>' channel without listeners.`（返回值规则见第 6 节）。

`invoke(channel, ...args)`：处理器查找顺序 `webFrameMain.ipc` → `webContents.ipc` → `ipcMain`，取第一个有处理器的；
都没有 → 拒绝，错误为 `No handler registered for '<channel>'`。

`webContents.ipc` 与 `webFrameMain.ipc` 是**独立的** `IpcMain` 实例（同样有 `on/handle/...`），只收该 webContents/frame 的消息。

### 3. 渲染器：`ipcRenderer`（EventEmitter 单例，仅渲染器的 `require('electron')` 有）

| 成员 | 行为 |
|---|---|
| `on/once/addListener/off/removeListener/removeAllListeners` | 监听者签名 `(event: IpcRendererEvent, ...args)` |
| `send(channel, ...args)` | 异步，不等待；参数逐个 `clone_out`，失败（如函数）同步抛 `Error: An object could not be cloned.` |
| `invoke(channel, ...args)` | Promise；成功 resolve 处理器返回值（Promise 则等其完成）；失败见"错误" |
| `sendSync(channel, ...args)` | **阻塞**渲染器线程直到主进程设置 `event.returnValue`；返回其克隆 |
| `postMessage(channel, message, transfer?)` | `transfer` 只能是 `MessagePort`；主进程收到 `event.ports` |
| `sendToHost(channel, ...args)` | `<webview>` 专用，v1 抛 `ERR_NOT_SUPPORTED` |

**注意**：`ipcRenderer.sendTo` 已在 Electron 28 移除，不实现（P02 的 `FromRenderer::SendTo` 变体保留但不由 `ipcRenderer` 产生，
留给可能的 `webContents` 间转发；建议 P02 文档标注）。`IpcRendererEvent`：`sender`（`ipcRenderer` 本身）、`ports`（`MessagePort[]`）。
Electron 39 已移除 `event.senderId`，不要提供。

### 4. 主进程 → 渲染器

| API | 行为 |
|---|---|
| `webContents.send(channel, ...args)` | 克隆后 `ToRenderer::Ipc`；webContents 已销毁 → 抛 `Error: Object has been destroyed` |
| `webContents.sendToFrame(frameId, channel, ...args)` | v1：`frameId` 为 1 或 `[processId, 1]` 时同 `send`，其他静默丢弃 |
| `webContents.postMessage(channel, message, transfer?)` | `transfer` 为 `MessagePortMain[]`；渲染器 `event.ports` 得到 `MessagePort` |
| `webFrameMain.send` / `postMessage` | 同上 |
| `event.reply(...)` | 同 `senderFrame.send` |

### 5. 错误（`invoke` 失败的跨线程形态）

主进程处理器抛出或返回被拒绝的 Promise，值为 `e`：
- 主进程侧：Electron 会在主进程打印 `Error occurred in handler for '<channel>':` 加 `e` 的堆栈（`console.error`）——照做。
- 传回渲染器：只传 **字符串** `String(e)` 的结果（Error 即 `"Error: boom"`，自定义类即 `"MyError: msg"`，非 Error 值即其字符串形式）。
  属性、`code`、`stack` **不**传（Electron 行为）。
- 渲染器侧：Promise 以 `new Error("Error invoking remote method '<channel>': " + 那个字符串)` 拒绝。
  例：`throw new Error('boom')` → 渲染器得到 message `Error invoking remote method 'ch': Error: boom`。
- 无处理器：`Error invoking remote method 'ch': Error: No handler registered for 'ch'`。
- 返回值不可克隆：主进程侧变成拒绝，`String(e)` 为 `Error: An object could not be cloned.`。

所以 `ToRenderer::InvokeReply.result` 的 `Err` 实际只需要携带字符串；`Cloned` 形态保持契约不变，内容为 `Cloned::String`。

### 6. `sendSync` 的阻塞通道

- 渲染器：`let (tx, rx) = std::sync::mpsc::sync_channel::<Cloned>(1)`；发 `FromRenderer::SendSync { reply: tx, .. }`；然后
  `rx.recv()` 阻塞。等待期间渲染器**不**处理任何其他消息、定时器、输入（与 Electron 一致：渲染器卡住）。
  P02 中写的 `oneshot::Sender<Cloned>` 用 `std::sync::mpsc::SyncSender<Cloned>` 实现即可，不引入新依赖。
- 主进程：按 FIFO 顺序处理到该消息，分发事件；给 `event.returnValue` 赋值时立即 `clone_out` 并 `tx.send`
  （只认第一次赋值；**允许在之后的异步回调里赋值**，Electron 支持，渲染器一直等）。
  - 完全没有监听者 → 打印上文的警告，立即回 `Cloned::Undefined`。
  - 有监听者但始终不赋值 → 渲染器一直阻塞（与 Electron 一致）；`IpcMainEvent` 对象被回收时（`tx` 随之丢弃）渲染器抛错
    `Error: sendSync reply was dropped`，避免永久挂起。这两点都用真实 Electron 观测确认后冻结。
- 主进程 isolate 结束、`tx` 被丢弃 → 渲染器 `recv` 出错 → `sendSync` 抛 `Error: Main process exited`。
- 渲染器正在被 `Isolate::interrupt` 停止（窗口关闭）时，阻塞必须可打断：用 `recv_timeout(50ms)` 循环并检查中断标志。
- 同步规则（P02）：主进程永远不等渲染器，因此不会死锁。

### 7. 序列化

一律用 `verve::clone`。Electron 的 IPC 走 HTML 结构化克隆，与 Verve 现有 `Cloned` 的差距（由 P03 在 `Cloned` 里**追加**，
本包写测试覆盖）：

| 类型 | 要求 |
|---|---|
| 原始值、`Date`、普通对象、数组、`Map`、`Set`、循环引用 | 现有已支持 |
| `Error` | 现有 `Cloned::Error`；Electron 只保留 `name`/`message`（以及 `stack`？以观测冻结） |
| `RegExp` | 追加：`source` + `flags` |
| `ArrayBuffer` | 追加：与 `Uint8Array` 区分，另一侧还原成 `ArrayBuffer` |
| 类型化数组 / `DataView` | 追加：保留具体类型（`Float32Array` 还原成 `Float32Array`）；现有 `Cloned::Bytes` 统一变 `Uint8Array` 不够 |
| `Buffer`（Node） | Electron 中到达另一侧是 `Uint8Array`（不是 Buffer）——以观测为准；现有 `Cloned::Buffer` 需确认行为 |
| `Blob` / `File` | 追加（可在 v1 后期）：字节 + 类型 + 名字 |
| 函数、`Symbol`、`Promise`、`WeakMap`、DOM 节点、类实例的原型 | 函数/Symbol → 抛 `An object could not be cloned.`；类实例 → 只保留自有可枚举属性（原型丢失） |
| `MessagePort` | 只能放在 `transfer` 里 |

一次消息的所有参数作为**一个**克隆（`Cloned::Array`），保证参数之间共享引用的一致性。

### 8. 端口：`MessageChannelMain` / `MessagePortMain`

- `new MessageChannelMain()` → `{ port1, port2 }`，均为 `MessagePortMain`（**EventEmitter**，不是 EventTarget）。
- `MessagePortMain`：`postMessage(message, transfer?)`、`start()`、`close()`；事件 `'message'`（参数 `MessageEvent`：
  `{ data, ports }`）、`'close'`。未 `start()` 前消息排队。
- 跨 isolate 端口：一对端口 = 两条 `crossbeam`/`mpsc` 通道 + 各端所在 isolate 的唤醒器；端口对象转移到另一个 isolate 后，
  在那边重建为 `MessagePortMain`（主进程）或 Web `MessagePort`（渲染器）。渲染器侧的 Web `MessagePort` 需要 P03 在
  `messaging.rs` 中追加"由远端通道驱动的端口"（`verve::messaging::RemotePort` 或等价物），投递仍作为宿主时钟上的任务。
- 一个端口被转移后，原对象失效（`postMessage` 静默丢弃；Electron 中被转移端口的使用会抛 `DataCloneError`——以观测为准）。
- 端口两端都在同一个渲染器内（`new MessageChannel()` 后自己用）→ 走现有 realm 内路径，不经通道。
- 关闭：任一端 `close()` 或所在 isolate 结束 → 对端发 `'close'`。

### 9. 投递与顺序

- 同一 (渲染器, 主进程) 对之间所有消息（`send`/`invoke`/`sendSync`/`postMessage`）严格 FIFO；同一端口上 FIFO。
- 主进程收到消息的分发在主进程 isolate 的 `tick` 里进行，每条消息分发后跑一次微任务检查点（与 Electron 每个任务后微任务一致）。
- 批量：一次 `tick` 处理入站队列里**所有**已到达的消息（上限 1000 条后让出），避免每条消息一次唤醒。
- 唤醒：发送方 push 后调用接收 isolate 的唤醒器；连续发送时唤醒合并（原子标志）。
- 大消息：`Cloned` 按值移动，不再复制；`Buffer`/`ArrayBuffer` 的字节只在 `clone_out`/`clone_in` 各拷一次。

## 步骤

1. `ipc.rs`：参数打包/解包、`IpcMainEvent`/`IpcMainInvokeEvent`/`IpcRendererEvent` 构造、invoke 调用表（`call: u64 → resolver`）。
2. `ipc_main.rs`：`IpcMain` 类（`ipcMain` 单例、`webContents.ipc`、`webFrameMain.ipc` 都是它的实例），handle 表。
3. `ipc_renderer.rs`。
4. `sendSync` 阻塞通道 + 中断。
5. `message_port_main.rs` + 与 P03 的远端端口对接。
6. `webContents.send/postMessage/sendToFrame`（P13 的 `WebContents` 类里调用本包函数；P13 未合并时在 P02 的最小 webContents 上挂）。
7. 观测冻结：`tests/electron/ipc/observe/` 是一个真实 Electron 39 小应用，跑遍上面每种情况，把渲染器和主进程看到的值/错误输出成 JSON；
   测试与之逐项比较。

## 验收

- `cargo test -p verve-electron --test ipc`（无头）：
  - send/on、invoke/handle（同步返回值、Promise、抛错、无处理器、重复注册）、handleOnce、removeHandler；
  - 分发顺序：`ipc-message` → `webContents.ipc` → `ipcMain`；invoke 优先级；
  - sendSync 返回值、无监听者、主进程退出时渲染器抛错；
  - 结构化克隆的每种类型往返；函数参数抛 `An object could not be cloned.`；
  - `event.reply`、`webContents.send` 到已销毁 webContents 抛错；
  - `MessageChannelMain`：主→渲染器转移端口、渲染器→主转移端口、双向通信、`close` 事件；
  - 错误消息与冻结观测逐字一致。
- 基准 `cargo test -p verve-electron --release --test ipc_bench -- --ignored --nocapture`：
  - 10 000 次 `invoke` 小负载（`{a:1}`）往返：p50 ≤ 0.2 ms，p99 ≤ 1 ms；
  - 1 MiB `Uint8Array` 单向 `send`：≤ 2 ms；
  - 渲染器连续 `send` 100 000 条：主进程全部按序收到，期间主进程线程唤醒次数 ≤ 消息数的 10%。
  结果写进本文档"状态"。

## 不做

`contextBridge`（P21：preload 如何把 `ipcRenderer` 暴露给页面）；`<webview>` 的 `sendToHost`/`ipc-message` 宿主事件；
Service Worker IPC（`IpcMainServiceWorkerEvent`）；`utilityProcess` 的 `parentPort`（P17，复用本包的端口实现）。

## 状态

（实现者完成后填写）
