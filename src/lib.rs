// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `browser` — a zapi native extension exposing the CDP-driven `Browser` /
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

use std::collections::{HashMap, HashSet};

use zuqe_zapi::{
    ZqCtx, ZqError, ZqGcTrace, ZqGcTraceCtx, ZqRawValue, ZqRuntime, ZqRuntimeState, ZqValue,
};
use zuqe_zapi_macros::zq_module;

use cdp::CdpClient;

/// Per-runtime browser-side state (the JS callbacks this module holds outside
/// the GC heap).
#[derive(Default)]
pub(crate) struct BrowserState {
    /// `handleCertificateError` callback (last wins).
    cert_callback: Option<ZqRawValue>,
    /// CDP client backing the generated `accept` closure.
    cert_client: Option<CdpClient>,
    /// session id -> `onRequestPaused` callback (last wins).
    fetch_callbacks: HashMap<String, ZqRawValue>,
    /// `(session, requestId)` pairs the JS side already continued / aborted /
    /// fulfilled, so the interception safety net does not resume them twice.
    fetch_handled: HashSet<(String, String)>,
    /// session id -> `page.onLog` callback.
    log_console_callbacks: HashMap<String, ZqRawValue>,
    /// session id -> `page.onConsole` callback.
    runtime_console_callbacks: HashMap<String, ZqRawValue>,
}

impl BrowserState {
    /// Record that JS already resolved `requestId` on `session_id`, so the
    /// interception safety net leaves it alone.
    fn mark_fetch_handled(&mut self, session_id: &str, request_id: &str) {
        self.fetch_handled
            .insert((session_id.to_string(), request_id.to_string()));
    }

    /// Claim the safety net for a paused request: `true` when nothing has
    /// handled it yet (so the caller must resume it).
    fn claim_fetch(&mut self, session_id: &str, request_id: &str) -> bool {
        !self
            .fetch_handled
            .remove(&(session_id.to_string(), request_id.to_string()))
    }
}

impl ZqGcTrace for BrowserState {
    /// Every JS value this module holds lives in one of the fields below.
    fn trace(&self, gcx: &mut ZqGcTraceCtx) {
        // `fetch_handled` holds only strings, so it is not marked.
        for map in [
            &self.fetch_callbacks,
            &self.log_console_callbacks,
            &self.runtime_console_callbacks,
        ] {
            for cb in map.values() {
                let _ = gcx.mark(*cb);
            }
        }
        if let Some(cb) = self.cert_callback {
            let _ = gcx.mark(cb);
        }
    }
}

/// Keyed by runtime identity, GC-traced, dropped by the finalize hook — see
/// [`ZqRuntimeState`].
static STATE: ZqRuntimeState<BrowserState> = ZqRuntimeState::new();

/// Run `f` against the calling runtime's [`BrowserState`].
pub(crate) fn with_state<R>(cx: ZqCtx, f: impl FnOnce(&mut BrowserState) -> R) -> R {
    STATE.with(cx, f)
}

/// Validate an event-callback argument and hand back the raw handle the traced
/// state map stores.
///
/// Storing a non-function would fail *silently*: the listener job would find it,
/// find it uncallable, and drop every event with no diagnostic at all.
pub(crate) fn require_callback(cx: ZqCtx, v: ZqValue) -> Result<ZqRawValue, ZqError> {
    if !v.is_callable() {
        return Err(cx.throw_type("callback argument must be a function"));
    }
    Ok(v.raw())
}

/// Runtime-finalize hook: drop this runtime's callbacks and, once no runtime is
/// left, kill any browser processes this module spawned. Must not call any JS
/// API.
///
/// Runs at runtime teardown, when thread-local destructors may already have run.
unsafe extern "C" fn finalize_runtime(rt: *mut ZqRuntime) {
    STATE.finalize(rt);
    // Spawned browsers are process-wide and may still serve another runtime, so
    // reap them only when the last one is gone.
    if STATE.is_empty() {
        kill_all_spawned_browsers();
    }
}

/// Module init: register the non-exported `BrowserProcess` class (like the
/// built-in module's `use_classes`), so `Browser.spawn()` can instantiate it.
/// The async executor and per-runtime state are named here, before any body
/// runs.
#[zq_module(name = "browser", exports = [JSBrowser, JSPage], finalize = finalize_runtime)]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    cx.set_default_executor(helpers::executor);
    class_browser::JSBrowserProcess::register(*cx)
}
