// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! `browser` — a zapi native extension exposing the CDP-driven `Browser` /
//! `Page` API to aike. Import it from JS as `import { Browser } from "browser"`
//! (with the compiled `browser.aike` on the module search path).

mod args;
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

use std::collections::{HashMap, HashSet};

use zuqe_zapi::{
    ZqCtx, ZqError, ZqGcTrace, ZqGcTraceCtx, ZqRawValue, ZqRuntime, ZqRuntimeState, ZqValue,
};
use zuqe_zapi_macros::zq_module;

use cdp::CdpClient;
use chrome::kill_all_spawned_browsers;

/// Which CDP event stream a listener belongs to.
///
/// Every listener owns one forwarding task and one event job, so a second
/// registration for the same scope has to *replace* them rather than stack a
/// second pair (which would deliver each event twice).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Listener {
    /// `page.onLog` (scoped by session id).
    Log,
    /// `page.onConsole` (scoped by session id).
    Console,
    /// `page.setRequestInterception` / `onRequestPaused` (scoped by session id).
    Fetch,
    /// `browser.handleCertificateError` (scoped by CDP connection id).
    Cert,
}

/// One registered `handleCertificateError` listener.
///
/// `accept` is a native function and cannot capture anything, so the connection
/// it acts on travels in the trampoline's `magic` value — which is why this is
/// a struct rather than a bare callback.
pub(crate) struct CertListener {
    /// The JS callback.
    pub(crate) callback: ZqRawValue,
    /// The CDP connection `accept()` drives.
    pub(crate) client: CdpClient,
}

/// Per-runtime browser-side state (the JS callbacks this module holds outside
/// the GC heap).
#[derive(Default)]
pub(crate) struct BrowserState {
    /// `handleCertificateError` listeners, keyed by their connection id.
    ///
    /// One slot per runtime let two browsers deliver each other's certificate
    /// errors, and keying by a per-registration counter instead let every
    /// re-registration leave the previous callback — and its forwarding task —
    /// running, so both fired for every event.
    cert_listeners: HashMap<u32, CertListener>,
    /// session id -> `onRequestPaused` callback (last wins).
    fetch_callbacks: HashMap<String, ZqRawValue>,
    /// `(session, requestId)` pairs the JS side already continued / aborted /
    /// fulfilled, so the interception safety net does not resume them twice.
    fetch_handled: HashSet<(String, String)>,
    /// session id -> `page.onLog` callback.
    log_console_callbacks: HashMap<String, ZqRawValue>,
    /// session id -> `page.onConsole` callback.
    runtime_console_callbacks: HashMap<String, ZqRawValue>,
    /// The tokio task forwarding CDP events into each listener's event job.
    ///
    /// Held so that re-registering a listener stops its predecessor and
    /// switching it off stops it entirely; without this, `onLog(cb)` twice
    /// delivers every message to both callbacks, and every
    /// `setRequestInterception` toggle leaks a task.
    listener_tasks: HashMap<(Listener, String), tokio::task::JoinHandle<()>>,
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

    /// Register (or replace) this browser's certificate-error listener.
    pub(crate) fn set_cert_listener(&mut self, id: u32, callback: ZqRawValue, client: CdpClient) {
        self.cert_listeners
            .insert(id, CertListener { callback, client });
    }

    /// The connection an `accept` call belongs to.
    pub(crate) fn cert_client(&self, magic: i32) -> Option<CdpClient> {
        // The trampoline magic *is* the connection id (see `NEXT_CLIENT_ID`), so
        // widening it back is exact rather than a guess.
        self.cert_listeners
            .get(&(magic as u32))
            .map(|l| l.client.clone())
    }

    /// Install a listener's forwarding task, aborting whatever it replaces.
    ///
    /// `scope` is the session id for page-level listeners and the connection id
    /// rendered as a string for the browser-level certificate listener. The
    /// superseded `ZqWakeSender` is dropped with the old task, which disconnects
    /// the old event job.
    pub(crate) fn set_listener_task(
        &mut self,
        listener: Listener,
        scope: &str,
        task: tokio::task::JoinHandle<()>,
    ) {
        if let Some(old) = self
            .listener_tasks
            .insert((listener, scope.to_string()), task)
        {
            old.abort();
        }
    }

    /// Stop a listener's forwarding task: `setRequestInterception(false)`.
    pub(crate) fn clear_listener_task(&mut self, listener: Listener, scope: &str) {
        if let Some(task) = self.listener_tasks.remove(&(listener, scope.to_string())) {
            task.abort();
        }
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
        for cb in self.cert_listeners.values() {
            let _ = gcx.mark(cb.callback);
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
