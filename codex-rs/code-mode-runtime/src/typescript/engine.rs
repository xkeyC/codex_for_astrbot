//! The checker thread: one long-lived V8 isolate that has loaded the
//! TypeScript compiler, fed one check at a time. It is created on the first
//! TypeScript cell, guarded by a watchdog and a heap limit, and recreated
//! after either stops it.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use serde::Deserialize;

use crate::v8_init::ensure_v8_initialized;

const TYPESCRIPT_JS_ZST: &[u8] = include_bytes!("../../vendor/typescript/typescript.js.zst");
const LIBS_JSON_ZST: &[u8] = include_bytes!("../../vendor/typescript/libs.json.zst");
const TS_BLANK_SPACE_JS: &str = include_str!("../../vendor/typescript/ts-blank-space.js");
const CHECKER_JS: &str = include_str!("checker.js");

/// A check (or loading the compiler) running longer is stopped.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// The checker isolate's heap limit; reaching it stops the check.
const HEAP_LIMIT_BYTES: usize = 256 * 1024 * 1024;
/// How long the checker is idle before it collects its garbage.
const IDLE_COLLECTION_DELAY: Duration = Duration::from_secs(2);
/// Room past the limit for V8 to unwind a stopped check.
const HEAP_HEADROOM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Diagnostic {
    /// `Some("code")` for the program; `None` for global and option errors.
    pub(crate) file: Option<String>,
    /// 1-based; 0 without a file.
    pub(crate) line: usize,
    /// 1-based, in UTF-16 code units; 0 without a file.
    pub(crate) column: usize,
    pub(crate) length: usize,
    pub(crate) code: u32,
    pub(crate) message: String,
}

/// What the checker made of a program.
#[derive(Debug, PartialEq)]
pub(crate) enum CheckResult {
    /// The program type-checks; `javascript` is it with its types blanked out
    /// (same lines and columns).
    Passed {
        javascript: String,
    },
    Failed {
        diagnostics: Vec<Diagnostic>,
    },
    /// The declarations do not parse at these (0-based) lines.
    DeclarationErrors {
        lines: Vec<usize>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckReply {
    js: Option<String>,
    diagnostics: Option<Vec<Diagnostic>>,
    sdk_error_lines: Option<Vec<usize>>,
}

/// What a job runs in the isolate.
pub(crate) enum JobKind {
    Check {
        declarations: Arc<str>,
        source: String,
    },
    /// Test hook: evaluates a script (to make the isolate hang or run out of memory).
    #[cfg(test)]
    Script(&'static str),
}

struct Job {
    kind: JobKind,
    timeout: Duration,
    reply: mpsc::SyncSender<Result<CheckResult, String>>,
}

static CHECKER: LazyLock<Mutex<Option<mpsc::Sender<Job>>>> = LazyLock::new(Mutex::default);

/// Checks `source` against `declarations` on the checker thread, starting it
/// if needed, and waits for the result. `Err` is a failure of the checker
/// itself, never of the program.
pub(crate) fn check(declarations: Arc<str>, source: String) -> Result<CheckResult, String> {
    run_job(
        JobKind::Check {
            declarations,
            source,
        },
        CHECK_TIMEOUT,
    )
}

pub(crate) fn run_job(kind: JobKind, timeout: Duration) -> Result<CheckResult, String> {
    let (reply, result) = mpsc::sync_channel(1);
    let mut job = Job {
        kind,
        timeout,
        reply,
    };
    // A thread that died (it never should) is replaced once.
    for _ in 0..2 {
        let sender = {
            let mut checker = CHECKER.lock().unwrap_or_else(PoisonError::into_inner);
            match checker.as_ref() {
                Some(sender) => sender.clone(),
                None => {
                    let sender = spawn_checker_thread()?;
                    *checker = Some(sender.clone());
                    sender
                }
            }
        };
        match sender.send(job) {
            Ok(()) => {
                return result
                    .recv()
                    .unwrap_or_else(|_| Err("the TypeScript checker stopped unexpectedly".into()));
            }
            Err(mpsc::SendError(returned)) => {
                job = returned;
                *CHECKER.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
        }
    }
    Err("the TypeScript checker is unavailable".to_string())
}

fn spawn_checker_thread() -> Result<mpsc::Sender<Job>, String> {
    ensure_v8_initialized()?;
    let (sender, jobs) = mpsc::channel::<Job>();
    // No check runs without its watchdog.
    let watchdog = Watchdog::new()?;
    std::thread::Builder::new()
        .name("code-mode-typescript".into())
        .spawn(move || {
            let mut engine: Option<Engine> = None;
            // Checks leave garbage V8 collects lazily; once the checker is
            // idle, a full collection hands the memory back.
            let mut collected = true;
            loop {
                let job = if collected {
                    jobs.recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                } else {
                    jobs.recv_timeout(IDLE_COLLECTION_DELAY)
                };
                let job = match job {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(engine) = engine.as_mut() {
                            engine.isolate.low_memory_notification();
                        }
                        collected = true;
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                collected = false;
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    run_in_engine(&mut engine, &watchdog, job.kind, job.timeout)
                }))
                .unwrap_or_else(|_| {
                    engine = None;
                    Err("the TypeScript checker panicked".to_string())
                });
                let _ = job.reply.send(outcome);
            }
        })
        .map_err(|error| format!("failed to start the TypeScript checker: {error}"))?;
    Ok(sender)
}

fn run_in_engine(
    engine: &mut Option<Engine>,
    watchdog: &Watchdog,
    kind: JobKind,
    timeout: Duration,
) -> Result<CheckResult, String> {
    if engine.is_none() {
        let started = Instant::now();
        let created = Engine::new(watchdog);
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            ok = created.is_ok(),
            "code-mode TypeScript checker loaded"
        );
        *engine = Some(created?);
    }
    let Some(current) = engine.as_mut() else {
        return Err("the TypeScript checker is unavailable".to_string());
    };
    let call = watchdog.guard(current.isolate.thread_safe_handle(), timeout, || {
        current.run(&kind)
    });
    let out_of_memory = current.oom.hit.load(Ordering::SeqCst);
    match call {
        Ok(Ok(reply)) if !out_of_memory => parse_reply(&reply),
        outcome => {
            // A stopped isolate is not reused.
            *engine = None;
            Err(if out_of_memory {
                format!(
                    "the TypeScript check ran out of memory ({} MB)",
                    HEAP_LIMIT_BYTES / (1024 * 1024)
                )
            } else {
                match outcome {
                    Err(TimedOut) => format!(
                        "the TypeScript check took longer than {} s",
                        timeout.as_secs_f64()
                    ),
                    Ok(Err(error)) => error,
                    Ok(Ok(_)) => "the TypeScript checker failed".to_string(),
                }
            })
        }
    }
}

fn parse_reply(reply: &str) -> Result<CheckResult, String> {
    let reply: CheckReply = serde_json::from_str(reply)
        .map_err(|error| format!("the TypeScript checker returned an invalid reply: {error}"))?;
    match reply {
        CheckReply {
            js: Some(javascript),
            ..
        } => Ok(CheckResult::Passed { javascript }),
        CheckReply {
            diagnostics: Some(diagnostics),
            ..
        } => Ok(CheckResult::Failed { diagnostics }),
        CheckReply {
            sdk_error_lines: Some(lines),
            ..
        } => Ok(CheckResult::DeclarationErrors { lines }),
        _ => Err("the TypeScript checker returned an empty reply".to_string()),
    }
}

/// Set from V8's near-heap-limit callback.
struct OomState {
    hit: AtomicBool,
    handle: v8::IsolateHandle,
}

unsafe extern "C" fn near_heap_limit(
    data: *mut c_void,
    current_heap_limit: usize,
    _initial_heap_limit: usize,
) -> usize {
    // SAFETY: `data` is the engine's boxed `OomState`, which outlives its isolate.
    let state = unsafe { &*(data as *const OomState) };
    state.hit.store(true, Ordering::SeqCst);
    state.handle.terminate_execution();
    current_heap_limit.saturating_add(HEAP_HEADROOM_BYTES)
}

/// The compiler's source, decompressed once and kept for the process
/// lifetime: isolates reference it as an external string instead of copying.
fn typescript_js() -> Result<&'static [u8], String> {
    static SOURCE: OnceLock<Result<&'static [u8], String>> = OnceLock::new();
    SOURCE
        .get_or_init(|| {
            let source = zstd::stream::decode_all(TYPESCRIPT_JS_ZST)
                .map_err(|error| format!("failed to unpack typescript.js: {error}"))?;
            if !source.is_ascii() {
                return Err("typescript.js is not ASCII".to_string());
            }
            Ok(Box::leak(source.into_boxed_slice()))
        })
        .clone()
}

fn libs_json() -> Result<String, String> {
    let libs = zstd::stream::decode_all(LIBS_JSON_ZST)
        .map_err(|error| format!("failed to unpack the TypeScript lib files: {error}"))?;
    String::from_utf8(libs).map_err(|_| "the TypeScript lib files are not UTF-8".to_string())
}

struct Engine {
    // Handles drop before their isolate.
    check: v8::Global<v8::Function>,
    context: v8::Global<v8::Context>,
    isolate: v8::OwnedIsolate,
    oom: Box<OomState>,
}

impl Engine {
    fn new(watchdog: &Watchdog) -> Result<Self, String> {
        let typescript_js = typescript_js()?;
        let libs_json = libs_json()?;
        let mut isolate =
            v8::Isolate::new(v8::CreateParams::default().heap_limits(0, HEAP_LIMIT_BYTES));
        let oom = Box::new(OomState {
            hit: AtomicBool::new(false),
            handle: isolate.thread_safe_handle(),
        });
        isolate
            .add_near_heap_limit_callback(near_heap_limit, &*oom as *const OomState as *mut c_void);
        let handle = isolate.thread_safe_handle();
        let loaded = watchdog.guard(handle, CHECK_TIMEOUT.saturating_mul(3), || {
            v8::scope!(let scope, &mut isolate);
            let context = v8::Context::new(scope, Default::default());
            let scope = &mut v8::ContextScope::new(scope, context);
            let tc = std::pin::pin!(v8::TryCatch::new(scope));
            let mut tc = tc.init();
            let typescript = v8::String::new_external_onebyte_static(&tc, typescript_js)
                .ok_or_else(|| "failed to allocate typescript.js".to_string())?;
            run_script(&mut tc, typescript, "typescript.js")?;
            let blank_space = v8::String::new(&tc, TS_BLANK_SPACE_JS)
                .ok_or_else(|| "failed to allocate ts-blank-space.js".to_string())?;
            run_script(&mut tc, blank_space, "ts-blank-space.js")?;
            let global = context.global(&tc);
            let libs_key = v8::String::new(&tc, "__codeModeLibs")
                .ok_or_else(|| "failed to allocate the lib key".to_string())?;
            let libs = v8::String::new(&tc, &libs_json)
                .ok_or_else(|| "failed to allocate the lib files".to_string())?;
            if global.set(&tc, libs_key.into(), libs.into()) != Some(true) {
                return Err("failed to install the lib files".to_string());
            }
            let checker = v8::String::new(&tc, CHECKER_JS)
                .ok_or_else(|| "failed to allocate checker.js".to_string())?;
            run_script(&mut tc, checker, "checker.js")?;
            let check_key = v8::String::new(&tc, "__codeModeCheck")
                .ok_or_else(|| "failed to allocate the check key".to_string())?;
            let check = global
                .get(&tc, check_key.into())
                .and_then(|check| v8::Local::<v8::Function>::try_from(check).ok())
                .ok_or_else(|| "checker.js defined no check function".to_string())?;
            Ok((v8::Global::new(&tc, check), v8::Global::new(&tc, context)))
        });
        let (check, context) = match loaded {
            Ok(Ok(loaded)) => loaded,
            Ok(Err(error)) => return Err(format!("failed to load TypeScript: {error}")),
            Err(TimedOut) => return Err("loading TypeScript took too long".to_string()),
        };
        if oom.hit.load(Ordering::SeqCst) {
            return Err("loading TypeScript ran out of memory".to_string());
        }
        Ok(Self {
            check,
            context,
            isolate,
            oom,
        })
    }

    /// The reply string of the job's call.
    fn run(&mut self, kind: &JobKind) -> Result<String, String> {
        v8::scope!(let scope, &mut self.isolate);
        let context = v8::Local::new(scope, &self.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        let tc = std::pin::pin!(v8::TryCatch::new(scope));
        let mut tc = tc.init();
        let result = match kind {
            JobKind::Check {
                declarations,
                source,
            } => {
                let check = v8::Local::new(&tc, &self.check);
                let declarations = v8::String::new(&tc, declarations)
                    .ok_or_else(|| "failed to allocate the declarations".to_string())?;
                let source = v8::String::new(&tc, source)
                    .ok_or_else(|| "failed to allocate the program".to_string())?;
                let receiver = v8::undefined(&tc).into();
                check.call(&tc, receiver, &[declarations.into(), source.into()])
            }
            #[cfg(test)]
            JobKind::Script(script) => {
                let script = v8::String::new(&tc, script)
                    .ok_or_else(|| "failed to allocate the script".to_string())?;
                v8::Script::compile(&tc, script, None).and_then(|script| script.run(&tc))
            }
        };
        match result {
            Some(value) => Ok(value.to_rust_string_lossy(&tc)),
            None => Err(exception_text(&mut tc)),
        }
    }
}

fn run_script(
    tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    source: v8::Local<v8::String>,
    name: &str,
) -> Result<(), String> {
    let name_value = v8::String::new(tc, name)
        .ok_or_else(|| format!("failed to allocate the name of {name}"))?;
    let origin = v8::ScriptOrigin::new(
        tc,
        name_value.into(),
        /*resource_line_offset*/ 0,
        /*resource_column_offset*/ 0,
        /*resource_is_shared_cross_origin*/ false,
        /*script_id*/ 0,
        /*source_map_url*/ None,
        /*resource_is_opaque*/ false,
        /*is_wasm*/ false,
        /*is_module*/ false,
        /*host_defined_options*/ None,
    );
    v8::Script::compile(tc, source, Some(&origin))
        .and_then(|script| script.run(tc))
        .map(|_| ())
        .ok_or_else(|| format!("{name}: {}", exception_text(tc)))
}

fn exception_text(tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>) -> String {
    if tc.has_terminated() {
        return "the check was stopped".to_string();
    }
    match tc.exception() {
        Some(exception) => exception.to_rust_string_lossy(tc),
        None => "unknown exception".to_string(),
    }
}

#[derive(Debug)]
struct TimedOut;

/// Stops the isolate whose call runs past its deadline.
struct Watchdog {
    shared: Arc<(Mutex<WatchdogState>, Condvar)>,
}

#[derive(Default)]
struct WatchdogState {
    active: Option<(v8::IsolateHandle, Instant)>,
    fired: bool,
    stopped: bool,
}

impl Watchdog {
    fn new() -> Result<Self, String> {
        let shared = Arc::new((Mutex::new(WatchdogState::default()), Condvar::new()));
        let observer = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("code-mode-typescript-watchdog".into())
            .spawn(move || {
                let (lock, changed) = &*observer;
                let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
                while !state.stopped {
                    match &state.active {
                        Some((handle, deadline)) if !state.fired => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                handle.terminate_execution();
                                state.fired = true;
                            } else {
                                state = changed
                                    .wait_timeout(state, remaining)
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .0;
                            }
                        }
                        _ => {
                            state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
                        }
                    }
                }
            })
            .map_err(|error| format!("failed to start the TypeScript checker watchdog: {error}"))?;
        Ok(Self { shared })
    }

    /// Runs `call`, stopping `handle`'s isolate after `timeout`.
    fn guard<T>(
        &self,
        handle: v8::IsolateHandle,
        timeout: Duration,
        call: impl FnOnce() -> T,
    ) -> Result<T, TimedOut> {
        let (lock, changed) = &*self.shared;
        {
            let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            state.active = Some((handle, Instant::now() + timeout));
            state.fired = false;
        }
        changed.notify_one();
        let result = call();
        let fired = {
            let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            state.active = None;
            state.fired
        };
        changed.notify_one();
        if fired { Err(TimedOut) } else { Ok(result) }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let (lock, changed) = &*self.shared;
        lock.lock().unwrap_or_else(PoisonError::into_inner).stopped = true;
        changed.notify_one();
    }
}
