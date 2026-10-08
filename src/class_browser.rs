// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Browser class — the top-level browser instance.
//!
//! Holds the CDP `CdpClient` (browser-level, no session) and the handle to the
//! spawned Chrome child process. All `Browser.*` methods are defined here.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use zuqe_zapi::ffi::{ZQ_PROP_CONFIGURABLE, ZQ_PROP_ENUMERABLE, ZQ_PROP_WRITABLE};
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method};

use crate::{
    args, cdp, chrome,
    class_page::{
        JSPage, get_array_prop, get_bool_prop, get_enum_prop, get_i64_prop, get_str_prop,
        new_page_payload,
    },
};

/// How long `close()` / a failed launch waits for Chrome to actually exit
/// before giving up on reaping it.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `Browser.WindowState` values.
const WINDOW_STATES: &[&str] = &["normal", "minimized", "maximized", "fullscreen"];

/// Payload attached to every `Browser` instance.
pub(crate) struct BrowserPayload {
    pub(crate) client: cdp::CdpClient,
    pub(crate) chrome_child: chrome::ChildSlot,
    /// Auto-created Chrome profile dir, removed once the child is reaped.
    pub(crate) user_data_dir: Option<std::path::PathBuf>,
    /// Cleared by `close()`. Attach mode owns no process, so this is what
    /// `isRunning` can honestly report there.
    pub(crate) closed: Arc<AtomicBool>,
    /// `true` for `launch()` (owns the child), `false` for `connect()` (attach
    /// mode). `isRunning` needs this to tell "the child has been reaped" from
    /// "there never was a child", which are both `chrome_child == None`.
    pub(crate) owns_process: bool,
}

// No GC-managed JS values in the payload.
#[zq_class(name = "Browser", payload = BrowserPayload, payload_no_gc)]
pub struct JSBrowser;

#[zq_class_impl]
impl JSBrowser {
    /// `Browser` cannot be constructed directly.
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Err(cx.throw_type(
            "Browser cannot be constructed directly; use Browser.launch() to start a browser",
        ))
    }

    // ── statics ──

    /// `Browser.launch(options?)` → Promise<Browser>
    #[zq_static_method]
    fn launch<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut req = LaunchRequest::default();
        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
        {
            req.proxy_url = get_str_prop(cx, &opts, "proxyUrl")?;
            req.chrome_path = get_str_prop(cx, &opts, "chromePath")?;
            req.cdp_url = get_str_prop(cx, &opts, "cdpUrl")?;
            if let Some(v) = get_i64_prop(cx, &opts, "port")? {
                // A port is 0 (auto-assign) or a real TCP port; `as u16` used to
                // wrap `-1` to 0 and saturate `70000` to 65535.
                req.port = args::in_range(cx, v, "port", 0, 65535)? as u16;
            }
            if let Some(h) = get_bool_prop(cx, &opts, "headless")? {
                req.headless = h;
            }
        }
        browser_promise(cx, req)
    }

    /// `Browser.connect(cdpUrl)` → Promise<Browser>
    #[zq_static_method]
    fn connect<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let cdp_url = args::arg_string(cx, &cc, 0, "cdpUrl")?;
        browser_promise(
            cx,
            LaunchRequest {
                cdp_url: Some(cdp_url),
                ..LaunchRequest::default()
            },
        )
    }

    /// `Browser.spawn(options?)` → Promise<BrowserProcess>
    #[zq_static_method]
    fn spawn<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut bin: Option<String> = None;
        let mut args: Option<Vec<String>> = None;
        let mut headless = true;
        let mut port = 0u16;
        let mut proxy_url: Option<String> = None;

        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
        {
            bin = get_str_prop(cx, &opts, "bin")?;
            proxy_url = get_str_prop(cx, &opts, "proxyUrl")?;
            if let Some(v) = get_i64_prop(cx, &opts, "port")? {
                port = args::in_range(cx, v, "port", 0, 65535)? as u16;
            }
            if let Some(h) = get_bool_prop(cx, &opts, "headless")? {
                headless = h;
            }
            if let Some(arr) = get_array_prop(cx, &opts, "args")? {
                let len = arr.length(cx)?;
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let s =
                        args::to_string(cx, arr.get_index(cx, i as u32)?, &format!("args[{i}]"))?;
                    out.push(s);
                }
                args = Some(out);
            }
        }

        // `args: []` is "no custom command line", not a custom one with zero
        // arguments (which would leave out `--remote-debugging-port` and hang
        // the startup poll for 30s).
        if args.as_ref().is_some_and(|a| !a.is_empty()) && bin.is_none() {
            return Err(cx.throw_type("`spawn`: `args` requires `bin`"));
        }

        cx.promise_fut_result(
            async move {
                let s = chrome::spawn(
                    bin.as_deref(),
                    args.as_deref(),
                    headless,
                    port,
                    proxy_url.as_deref(),
                    false,
                )
                .await?;
                // `Child::id()` is only available while the child is unwaited,
                // and the slot is contended during any kill; capture it now.
                let pid = s.child.id().unwrap_or(0);
                let handle: chrome::ChildSlot = Arc::new(tokio::sync::Mutex::new(Some(s.child)));
                chrome::register_spawned_child(&handle);
                Ok((s.bin, s.cdp_url, handle, s.user_data_dir, pid))
            },
            |cx, (bin, cdp_url, child, user_data_dir, pid)| {
                JSBrowserProcess::new_instance(
                    cx,
                    BrowserProcessPayload {
                        child,
                        bin,
                        cdp_url,
                        user_data_dir,
                        pid,
                        killed: Arc::new(AtomicBool::new(false)),
                    },
                )
            },
        )
    }

    // ── instance methods ──

    /// `browser.newPage(url?, contextId?)` → Promise<Page>
    #[zq_method(name = "newPage")]
    fn new_page<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_browser_payload(cx, cc.this())?;
        let client = payload.client.clone();

        let mut url = "about:blank".to_string();
        let mut context_id: Option<String> = None;
        let first = cc.arg(0);
        // Only a string or an options object is accepted. `newPage(42)` and
        // `newPage(null)` used to fall through to `about:blank` silently.
        if !first.is_undefined() {
            if let Some(obj) = first.as_object() {
                if let Some(u) = get_str_prop(cx, &obj, "url")? {
                    url = u;
                }
                if let Some(cid) = get_str_prop(cx, &obj, "contextId")? {
                    if cid.is_empty() {
                        return Err(cx.throw_range("`contextId` must not be empty"));
                    }
                    context_id = Some(cid);
                }
            } else {
                url = args::to_string(cx, first, "urlOrOpts")?;
            }
        }

        let client_cb = client.clone();
        cx.promise_fut_result(
            async move {
                let mut params = serde_json::json!({ "url": url });
                if let Some(cid) = context_id {
                    params["browserContextId"] = serde_json::Value::String(cid);
                }
                let resp = client
                    .send("Target.createTarget", Some(params), None)
                    .await?;
                let target_id = resp
                    .get("targetId")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "no targetId".to_string())?
                    .to_string();

                let attach = client
                    .send(
                        "Target.attachToTarget",
                        Some(serde_json::json!({ "targetId": target_id, "flatten": true })),
                        None,
                    )
                    .await?;
                let session_id = attach
                    .get("sessionId")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "no sessionId".to_string())?
                    .to_string();
                Ok((target_id, session_id))
            },
            move |cx, (target_id, session_id)| {
                let page = JSPage::new_instance(
                    cx,
                    new_page_payload(client_cb.clone(), session_id, target_id.clone()),
                )?;
                // `targetId` own property (readonly in the d.ts; kept writable
                // here so the native install always wins).
                let id_val = ZqValue::new_string(cx, &target_id)?;
                page.define_property_str(
                    cx,
                    "targetId",
                    id_val,
                    ZQ_PROP_CONFIGURABLE | ZQ_PROP_WRITABLE | ZQ_PROP_ENUMERABLE,
                )?;
                Ok(page)
            },
        )
    }

    /// `browser.close()`
    ///
    /// Owning mode: ask Chrome to exit, then reap it and delete its profile
    /// dir. Attach mode: just disconnect — the external process is not ours to
    /// kill, but the WebSocket and its reader task must still go.
    #[zq_method]
    fn close<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_browser_payload(cx, cc.this())?;
        let client = payload.client.clone();
        let chrome_child = payload.chrome_child.clone();
        let user_data_dir = payload.user_data_dir.clone();
        let closed = payload.closed.clone();
        cx.promise_fut_result(
            async move {
                // `lock().await`, never `try_lock()`: losing the race used to
                // skip the reap *and* the profile-dir cleanup, silently leaking
                // a Chrome process and its directory.
                let owns_process = chrome_child.lock().await.is_some();
                if owns_process {
                    // Chrome usually drops the connection before answering
                    // `Browser.close`, so a failure here is expected rather than
                    // exceptional — but it must not be invisible either.
                    if let Err(e) = client.send("Browser.close", None, None).await {
                        log::warn!("Browser.close did not acknowledge: {e}");
                    }
                }
                if let Some(mut child) = chrome_child.lock().await.take() {
                    // On Windows, `Browser.close` and child.start_kill() are
                    // asynchronous; wait for actual exit before removing the
                    // user-data-dir, otherwise Chrome would rewrite files in
                    // a directory we are deleting.
                    match user_data_dir {
                        Some(dir) => {
                            chrome::shutdown_and_cleanup(&mut child, &dir, CLOSE_TIMEOUT).await;
                        }
                        None => chrome::kill_and_reap(&mut child, CLOSE_TIMEOUT).await,
                    }
                }
                // Tear the connection down in both modes: without this, attach
                // mode left the socket and its reader task alive for the rest
                // of the process lifetime.
                client.disconnect();
                closed.store(true, Ordering::SeqCst);
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `browser.isRunning` getter
    ///
    /// Owning mode reports the child process. Attach mode owns no process, so
    /// there it reports whether this `Browser` still holds an open CDP
    /// connection — the only liveness answerable without a round trip.
    #[zq_method(name = "isRunning", get)]
    fn is_running<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_browser_payload(cx, cc.this())?;
        if payload.closed.load(Ordering::SeqCst) || payload.client.is_closed() {
            return Ok(ZqValue::bool(false));
        }
        if !payload.owns_process {
            // Attach mode owns no child; an open connection is the only
            // liveness answer (the `close()` / `is_closed()` checks above
            // already ruled out a dead one).
            return Ok(ZqValue::bool(true));
        }
        // Contended means another caller is mid-reap, so the child is still
        // there: answering `false` would report a live browser as dead.
        let child_present = payload
            .chrome_child
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(true);
        Ok(ZqValue::bool(child_present))
    }

    /// `browser.getVersion()`
    #[zq_method(name = "getVersion")]
    fn get_version<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_browser_payload(cx, cc.this())?.client.clone();
        cx.promise_fut_result(
            async move { client.send("Browser.getVersion", None, None).await },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `browser.getWindowBounds()`
    #[zq_method(name = "getWindowBounds")]
    fn get_window_bounds<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_browser_payload(cx, cc.this())?.client.clone();
        cx.promise_fut_result(
            async move {
                let wid = browser_window_id(&client).await?;
                client
                    .send(
                        "Browser.getWindowBounds",
                        Some(serde_json::json!({ "windowId": wid })),
                        None,
                    )
                    .await
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `browser.setWindowBounds(bounds)`
    #[zq_method(name = "setWindowBounds")]
    fn set_window_bounds<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let obj = cc
            .arg(0)
            .as_object()
            .ok_or_else(|| cx.throw_type("`bounds` must be an object"))?;
        let mut bounds = serde_json::Map::new();
        // Geometry is integer: `as i32` used to truncate `2.7` to 2 (which the
        // renderer would have rejected) and accept a string like `"300"`.
        let mut has_geometry = false;
        for key in ["left", "top", "width", "height"] {
            if let Some(v) = get_i64_prop(cx, &obj, key)? {
                bounds.insert(key.into(), serde_json::Value::from(v));
                has_geometry = true;
            }
        }
        // An unknown state is not merely a rejected command: Chrome aborts the
        // whole browser process, taking every page with it. Validate first.
        if let Some(state) = get_enum_prop(cx, &obj, "windowState", WINDOW_STATES)? {
            // The protocol forbids combining a non-`normal` state with geometry.
            if has_geometry && state != "normal" {
                return Err(cx.throw_range(
                    "`windowState` cannot be combined with `left`/`top`/`width`/`height` unless it is \"normal\"",
                ));
            }
            bounds.insert("windowState".into(), serde_json::Value::String(state));
        }

        let client = get_browser_payload(cx, cc.this())?.client.clone();
        cx.promise_fut_result(
            async move {
                let wid = browser_window_id(&client).await?;
                client
                    .send(
                        "Browser.setWindowBounds",
                        Some(serde_json::json!({ "windowId": wid, "bounds": bounds })),
                        None,
                    )
                    .await?;
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `browser.setDownloadBehavior(path)`
    #[zq_method(name = "setDownloadBehavior")]
    fn set_download_behavior<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        let path = args::arg_string(cx, &cc, 0, "path")?;
        let params = if path.is_empty() {
            serde_json::json!({ "behavior": "deny" })
        } else {
            serde_json::json!({ "behavior": "allow", "downloadPath": path })
        };
        send_browser_void_cmd(cx, &cc, "Browser.setDownloadBehavior", Some(params))
    }

    // ── Target domain ──

    #[zq_method(name = "listPages")]
    fn list_pages<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::target::js_browser_list_pages(cx, cc)
    }

    #[zq_method(name = "getTargets")]
    fn get_targets<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::target::js_browser_get_targets(cx, cc)
    }

    #[zq_method(name = "newContext")]
    fn new_context<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::target::js_browser_new_context(cx, cc)
    }

    #[zq_method(name = "disposeContext")]
    fn dispose_context<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::target::js_browser_dispose_context(cx, cc)
    }

    // ── Security domain ──

    #[zq_method(name = "setIgnoreHTTPSErrors")]
    fn set_ignore_https_errors<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        crate::security::js_browser_set_ignore_https_errors(cx, cc)
    }

    #[zq_method(name = "handleCertificateError")]
    fn handle_certificate_error<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        crate::security::js_browser_handle_certificate_error(cx, cc)
    }
}

// ---------------------------------------------------------------------------
// Launch request
// ---------------------------------------------------------------------------

/// Normalized launch request shared by `launch` / `connect`.
struct LaunchRequest {
    headless: bool,
    port: u16,
    proxy_url: Option<String>,
    chrome_path: Option<String>,
    cdp_url: Option<String>,
}

impl Default for LaunchRequest {
    fn default() -> Self {
        Self {
            headless: true,
            port: 0, // 0 = auto-assign a free port
            proxy_url: None,
            chrome_path: None,
            cdp_url: None,
        }
    }
}

/// Shared promise machinery for launch/connect: run the request, build the
/// Browser object when the CDP connection is ready.
fn browser_promise<'a>(cx: ZqCtx<'a>, req: LaunchRequest) -> Result<ZqValue<'a>, ZqError> {
    let LaunchRequest {
        headless,
        port,
        proxy_url,
        chrome_path,
        cdp_url,
    } = req;

    cx.promise_fut_result(
        async move {
            if let Some(url) = cdp_url {
                cdp::CdpClient::connect(&url)
                    .await
                    .map(|(c, _)| (c, None, None))
            } else {
                match chrome::launch(headless, port, proxy_url.as_deref(), chrome_path.as_deref())
                    .await
                {
                    Ok((mut child, ws_url, data_dir)) => {
                        match cdp::CdpClient::connect(&ws_url).await {
                            Ok((c, _)) => Ok((c, Some(child), data_dir)),
                            Err(e) => {
                                // Never leave the browser we just started behind:
                                // reap it (and drop its profile dir) before
                                // reporting why the connection failed.
                                match data_dir {
                                    Some(dir) => {
                                        chrome::shutdown_and_cleanup(
                                            &mut child,
                                            &dir,
                                            CLOSE_TIMEOUT,
                                        )
                                        .await;
                                    }
                                    None => chrome::kill_and_reap(&mut child, CLOSE_TIMEOUT).await,
                                }
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        },
        |cx, (client, child, user_data_dir)| {
            // `child` is `Some` only for `launch()`; `connect()` attaches with
            // no process of its own.
            let owns_process = child.is_some();
            JSBrowser::new_instance(
                cx,
                BrowserPayload {
                    client,
                    chrome_child: Arc::new(tokio::sync::Mutex::new(child)),
                    user_data_dir,
                    closed: Arc::new(AtomicBool::new(false)),
                    owns_process,
                },
            )
        },
    )
}

// ---------------------------------------------------------------------------
// BrowserProcess handle (Browser.spawn)
// ---------------------------------------------------------------------------

pub(crate) struct BrowserProcessPayload {
    /// Shared with the spawn registry so runtime-exit cleanup can kill the
    /// process even if this handle was GC-collected.
    pub(crate) child: chrome::ChildSlot,
    /// Resolved browser binary path (surfaced via the `bin` getter).
    pub(crate) bin: String,
    /// Ready browser-level CDP WebSocket URL (surfaced via `cdpUrl`).
    pub(crate) cdp_url: String,
    /// Auto-created Chrome profile dir, removed once the child is reaped.
    pub(crate) user_data_dir: Option<std::path::PathBuf>,
    /// Pid captured at spawn. Read from the live `Child` it could only be
    /// reported while nothing else held the lock, so `pid` read back `null` on
    /// a perfectly healthy process.
    pub(crate) pid: u32,
    /// Set by `kill()`; `pid` reports `null` afterwards.
    pub(crate) killed: Arc<AtomicBool>,
}

#[zq_class(name = "BrowserProcess", payload = BrowserProcessPayload, payload_no_gc)]
pub struct JSBrowserProcess;

#[zq_class_impl]
impl JSBrowserProcess {
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Err(cx.throw_type("BrowserProcess cannot be constructed directly; use Browser.spawn()"))
    }

    /// `process.kill()` — kill the spawned browser process; resolves after exit.
    #[zq_method]
    fn kill<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = JSBrowserProcess::payload(cx, cc.this())
            .ok_or_else(|| cx.throw_type("not a BrowserProcess instance"))?;
        let child = payload.child.clone();
        let user_data_dir = payload.user_data_dir.clone();
        let killed = payload.killed.clone();
        cx.promise_fut_result(
            async move {
                let mut guard = child.lock().await;
                if let Some(mut child) = guard.take() {
                    match user_data_dir {
                        Some(dir) => {
                            chrome::shutdown_and_cleanup(&mut child, &dir, CLOSE_TIMEOUT).await;
                        }
                        None => chrome::kill_and_reap(&mut child, CLOSE_TIMEOUT).await,
                    }
                }
                drop(guard);
                killed.store(true, Ordering::SeqCst);
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    #[zq_method(name = "bin", get)]
    fn bin<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JSBrowserProcess::payload(cx, cc.this())
            .ok_or_else(|| cx.throw_type("not a BrowserProcess instance"))?;
        ZqValue::new_string(cx, &p.bin)
    }

    #[zq_method(name = "cdpUrl", get)]
    fn cdp_url<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JSBrowserProcess::payload(cx, cc.this())
            .ok_or_else(|| cx.throw_type("not a BrowserProcess instance"))?;
        ZqValue::new_string(cx, &p.cdp_url)
    }

    #[zq_method(name = "pid", get)]
    fn pid<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = JSBrowserProcess::payload(cx, cc.this())
            .ok_or_else(|| cx.throw_type("not a BrowserProcess instance"))?;
        if p.killed.load(Ordering::SeqCst) {
            return Ok(ZqValue::null());
        }
        // A Windows pid is an unsigned 32-bit value; `as i32` could report a
        // perfectly live process as a negative pid.
        Ok(ZqValue::number(p.pid as f64))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the `BrowserPayload` from a `Browser` JS value.
pub(crate) fn get_browser_payload<'a>(
    cx: ZqCtx<'a>,
    val: ZqValue<'a>,
) -> Result<&'a BrowserPayload, ZqError> {
    JSBrowser::payload(cx, val).ok_or_else(|| cx.throw_type("not a Browser instance"))
}

/// Shared helper: void-returning CDP command sent at the Browser level.
pub(crate) fn send_browser_void_cmd<'a>(
    cx: ZqCtx<'a>,
    cc: &ZqCallInfo<'a>,
    method: &'static str,
    params: Option<serde_json::Value>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    cx.promise_fut_result(
        async move {
            client.send(method, params, None).await?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

/// Resolve the browser's main window id via `Browser.getWindowForTarget`.
async fn browser_window_id(client: &cdp::CdpClient) -> Result<i64, String> {
    let resp = client
        .send(
            "Browser.getWindowForTarget",
            Some(serde_json::json!({})),
            None,
        )
        .await?;
    // `Browser.WindowId` is an integer, not a string: reading it as one made
    // every window method fail with "no windowId".
    resp.get("windowId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "no windowId".to_string())
}
