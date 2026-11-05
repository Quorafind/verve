//! What Electron adds to the main process's `process`: its creation time, CPU and memory
//! figures, the memory of the machine, the system version, and the calls that stop the process
//! (`crash`, `hang`). Electron 39 has no `getIOCounters`, and `setFdLimit` only off Windows.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, js_text};

use super::app::platform;
use super::define;
use super::shared;

/// When the process began, in milliseconds since the epoch, and the clock the CPU share is
/// measured against.
fn started() -> &'static (f64, Instant) {
    static STARTED: OnceLock<(f64, Instant)> = OnceLock::new();
    STARTED.get_or_init(|| {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        (since_epoch.as_secs_f64() * 1000., Instant::now())
    })
}

/// `process.getCreationTime()`.
pub(crate) fn creation_time() -> f64 {
    started().0.floor()
}

/// `{ percentCPUUsage, cumulativeCPUUsage, idleWakeupsPerSecond }`: the share of one core used
/// since the process began, and the seconds of CPU.
pub(crate) fn cpu_usage(context: &mut Context) -> JsResult<JsObject> {
    let cumulative = platform::cpu_seconds();
    let elapsed = started().1.elapsed().as_secs_f64().max(f64::EPSILON);
    define::record(
        context,
        &[
            ("percentCPUUsage", (cumulative / elapsed * 100.).into()),
            ("cumulativeCPUUsage", cumulative.into()),
            ("idleWakeupsPerSecond", 0.into()),
        ],
    )
}

/// `{ workingSetSize, peakWorkingSetSize, privateBytes }` in kilobytes: `getAppMetrics`' memory.
pub(crate) fn process_memory_figures(context: &mut Context) -> JsResult<JsObject> {
    let memory = platform::process_memory();
    define::record(
        context,
        &[
            ("workingSetSize", memory.resident.into()),
            ("peakWorkingSetSize", memory.peak_resident.into()),
            ("privateBytes", memory.private.into()),
        ],
    )
}

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("getCreationTime", 0, |_, _, _| Ok(creation_time().into())),
    ("getCPUUsage", 0, |_, _, cx| Ok(cpu_usage(cx)?.into())),
    ("getHeapStatistics", 0, heap_statistics),
    ("getBlinkMemoryInfo", 0, |_, _, cx| {
        let memory = platform::process_memory();
        let fields = [
            ("allocated", memory.private.into()),
            ("total", memory.resident.into()),
        ];
        Ok(define::record(cx, &fields)?.into())
    }),
    ("getProcessMemoryInfo", 0, process_memory_info),
    ("getSystemMemoryInfo", 0, system_memory),
    ("getSystemVersion", 0, |_, _, _| {
        Ok(js_text(&platform::system_version()))
    }),
    ("takeHeapSnapshot", 1, |_, _, _| Ok(false.into())),
    ("crash", 0, |_, _, _| std::process::abort()),
    ("hang", 0, hang),
];

/// Gives the main process's `process` what Electron adds to it.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let process = native::global(context, "process")?;
    let Some(process) = process.as_object() else {
        return Ok(());
    };
    started();
    native::exports(context, &process, METHODS)?;
    // Electron's `lib/browser/init.ts` does `process.exit = app.exit`: the windows are closed,
    // `exit` and `quit` are heard, and the script goes on to the end of its turn.
    native::exports(context, &process, &[("exit", 1, super::app::exit)])?;
    if !cfg!(windows) {
        let no_op: NativeFunctionPointer = |_, _, _| Ok(JsValue::undefined());
        native::exports(context, &process, &[("setFdLimit", 1, no_op)])?;
    }
    Ok(())
}

/// V8's heap figures in kilobytes. Boa does not report them, so these are the process's own
/// private memory against V8's default limit: the keys and units are Electron's, the values an
/// estimate.
fn heap_statistics(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let used = platform::process_memory().private;
    let limit: u64 = 4_194_048;
    let fields = [
        ("totalHeapSize", used.into()),
        ("totalHeapSizeExecutable", 0.into()),
        ("totalPhysicalSize", used.into()),
        ("totalAvailableSize", limit.saturating_sub(used).into()),
        ("usedHeapSize", used.into()),
        ("heapSizeLimit", limit.into()),
        ("mallocedMemory", 0.into()),
        ("peakMallocedMemory", 0.into()),
        ("doesZapGarbage", false.into()),
    ];
    Ok(define::record(context, &fields)?.into())
}

/// `process.getProcessMemoryInfo()`: a promise for `{ residentSet, private, shared }`.
fn process_memory_info(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let memory = platform::process_memory();
    let fields = [
        ("residentSet", memory.resident.into()),
        ("private", memory.private.into()),
        (
            "shared",
            memory.resident.saturating_sub(memory.private).into(),
        ),
    ];
    let info = define::record(context, &fields)?;
    native::settled(Ok(info.into()), context)
}

/// `process.getSystemMemoryInfo()`: kilobytes of memory and swap.
fn system_memory(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let memory = platform::system_memory();
    let fields = [
        ("total", memory.total.into()),
        ("free", memory.free.into()),
        ("swapTotal", memory.swap_total.into()),
        ("swapFree", memory.swap_free.into()),
    ];
    Ok(define::record(context, &fields)?.into())
}

/// `process.hang()`: the main process stops answering until the app ends.
fn hang(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let hub = std::sync::Arc::clone(&shared(context).hub);
    while hub.exit_code().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(JsValue::undefined())
}
