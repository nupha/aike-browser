// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `browser` ?a zapi native extension exposing the CDP-driven `Browser` /
//! `Page` API to aike. Import it from JS as `import { Browser } from "browser"`
//! (with the compiled `browser.aike` on the module search path).

mod cdp;
mod cdp_input;
mod cdp_network;
mod chrome;
mod class_browser;
mod class_page;
mod css;
mod dom;
mod emulation;
mod fetch;
mod helpers;
mod input;
mod io;
mod log;
mod network;
mod overlay;
mod runtime;
mod security;
mod storage;
mod target;
mod tracing;

pub use {class_browser::JSBrowser, class_page::JSPage};

use chrome::kill_all_spawned_browsers;

use std::cell::RefCell;
use std::collections::HashMap;

use zuqe_zapi::{ZqCtx, ZqError, ZqGcTraceCtx, ZqRawValue};
use zuqe_zapi_macros::zq_module;

use cdp::CdpClient;

/// Per-runtime browser-side state (JS callbacks), attached to the runtime via
/// its [`ZqRuntime::id`](zuqe_zapi::ZqRuntime::id) so the engine traces it and
/// two runtimes on one thread never share it.
#[derive(Default)]
pub(crate) struct BrowserState {
    /// `handleCertificateError` callback (last wins).
    pub(crate) cert_callback: Option<ZqRawValue>,
    /// CDP client backing the generated `accept` closure.
    pub(crate) cert_client: Option<CdpClient>,
    /// eventId of the certificate error currently being delivered.
    pub(crate) cert_event_id: Option<i64>,
    /// session id -> `onRequestPaused` callback (last wins).
    pub(crate) fetch_callbacks: HashMap<String, ZqRawValue>,
    /// session id -> `page.onLog` callback.
    pub(crate) log_console_callbacks: HashMap<String, ZqRawValue>,
    /// session id -> `page.onConsole` callback.
    pub(crate) runtime_console_callbacks: HashMap<String, ZqRawValue>,
}

impl BrowserState {
    /// Mark every JS value held outside the GC heap so it survives collection.
    fn trace(&self, gcx: &mut ZqGcTraceCtx) {
        let mark = |raw: ZqRawValue| {
            let _ = gcx.mark(raw);
        };
        if let Some(cb) = self.cert_callback {
            mark(cb);
        }
        for cb in self.fetch_callbacks.values() {
            mark(*cb);
        }
        for cb in self.log_console_callbacks.values() {
            mark(*cb);
        }
        for cb in self.runtime_console_callbacks.values() {
            mark(*cb);
        }
    }
}

// Per-runtime state, keyed by the opaque runtime identity handle. A module is
// loaded once process-wide, so a thread_local is enough to keep two runtimes on
// one thread apart.
thread_local! {
    static BROWSER_STATES: RefCell<HashMap<usize, BrowserState>> =
        RefCell::new(HashMap::new());
    /// Runtime identity handles currently registered via `gc_set_trace_func`.
    static TRACE_RUNTIMES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// `ctx.runtime().id()` for the runtime this context belongs to.
fn runtime_key(ctx: ZqCtx) -> usize {
    ctx.runtime().id()
}

/// Run `f` against the calling runtime's [`BrowserState`], creating it lazily,
/// and make sure the per-runtime GC trace is installed before any callback can
/// be stored.
pub(crate) fn with_state<R>(ctx: ZqCtx, f: impl FnOnce(&mut BrowserState) -> R) -> R {
    ensure_gc_trace(ctx);
    let key = runtime_key(ctx);
    BROWSER_STATES.with(|states| {
        let mut states = states.borrow_mut();
        f(states.entry(key).or_default())
    })
}

/// GC trace trampoline registered once per runtime. Runs on the JS thread and
/// reports every callback held in that runtime's [`BrowserState`].
unsafe extern "C" fn trace_browser_state(
    gcx: *mut zuqe_zapi::ffi::ZqGcTraceCtxS,
    _opaque: *mut std::ffi::c_void,
) {
    let mut gcx = ZqGcTraceCtx::from(gcx);
    // `try_with`: a GC trace must never fire during TLS teardown; ignore if it
    // somehow does rather than aborting the process.
    let _ = BROWSER_STATES.try_with(|states| {
        for state in states.borrow().values() {
            state.trace(&mut gcx);
        }
    });
}

/// Install the per-runtime GC trace once, before any callback is stored.
fn ensure_gc_trace(ctx: ZqCtx) {
    let key = runtime_key(ctx);
    let already = TRACE_RUNTIMES.with(|rts| {
        let mut rts = rts.borrow_mut();
        if rts.contains(&key) {
            true
        } else {
            rts.push(key);
            false
        }
    });
    if !already {
        let _ = ctx.gc_set_trace_func(Some(trace_browser_state), std::ptr::null_mut());
    }
}

/// Runtime-finalize hook: kill any browser processes this module spawned and
/// drop this runtime's state. Must not call any JS API.
///
/// Runs at runtime teardown, when thread-local destructors may already have run;
/// `try_with` keeps a torn-down TLS from aborting the process.
unsafe extern "C" fn finalize_runtime(rt: *mut zuqe_zapi::ffi::ZqRuntime) {
    let key = rt as usize;
    let _ = BROWSER_STATES.try_with(|states| {
        let empty = {
            let mut guard = states.borrow_mut();
            guard.remove(&key);
            guard.is_empty()
        };
        if empty {
            kill_all_spawned_browsers();
        }
    });
    let _ = TRACE_RUNTIMES.try_with(|rts| {
        rts.borrow_mut().retain(|k| *k != key);
    });
}

/// Module init: register the non-exported `BrowserProcess` class (like the
/// built-in module's `use_classes`), so `Browser.spawn()` can instantiate it.
/// Per-runtime GC state is installed lazily from the first browser call.
#[zq_module(name = "browser", exports = [JSBrowser, JSPage], finalize = finalize_runtime)]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    class_browser::JSBrowserProcess::register(*cx)
}
