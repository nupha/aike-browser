// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Browser class — the top-level browser instance.
//!
//! Holds the CDP `CdpClient` (browser-level, no session) and the handle to the
//! spawned Chrome child process. All `Browser.*` methods are defined here.

use std::sync::Arc;

use zuqe_zapi::ffi::{ZQ_PROP_CONFIGURABLE, ZQ_PROP_ENUMERABLE, ZQ_PROP_WRITABLE};
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method, zq_static_method};

use crate::{
    cdp, chrome,
    class_page::{
        JSPage, PageNetworkState, PagePayload, get_bool_prop, get_number_prop, get_str_prop,
    },
    helpers,
};

/// How long `close()` / a failed launch waits for Chrome to actually exit
/// before giving up on reaping it.
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Payload attached to every `Browser` instance.
pub(crate) struct BrowserPayload {
    pub(crate) client: cdp::CdpClient,
    pub(crate) chrome_child: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
    /// Auto-created Chrome profile dir, removed once the child is reaped.
    pub(crate) user_data_dir: Option<std::path::PathBuf>,
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
            req.proxy_url = get_str_prop(cx, &opts, "proxyUrl");
            req.chrome_path = get_str_prop(cx, &opts, "chromePath");
            req.cdp_url = get_str_prop(cx, &opts, "cdpUrl");
            if let Some(v) = get_number_prop(cx, &opts, "port") {
                req.port = v as u16;
            }
            if let Some(h) = get_bool_prop(cx, &opts, "headless") {
                req.headless = h;
            }
        }
        browser_promise(cx, req)
    }

    /// `Browser.connect(cdpUrl)` → Promise<Browser>
    #[zq_static_method]
    fn connect<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let url = cc.arg(0).try_string_read_utf8(cx);
        let Some(cdp_url) = url else {
            return Err(cx.throw_type("`connect` requires a `cdpUrl` string (ws://...)"));
        };
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
            bin = get_str_prop(cx, &opts, "bin");
            proxy_url = get_str_prop(cx, &opts, "proxyUrl");
            if let Some(v) = get_number_prop(cx, &opts, "port") {
                port = v as u16;
            }
            if let Some(h) = get_bool_prop(cx, &opts, "headless") {
                headless = h;
            }
            if let Ok(Some(arr)) = opts.get_array_str(cx, "args")
                && let Ok(len) = arr.length(cx)
            {
                let mut out = Vec::with_capacity(len);
                for i in 0..len {
                    let s = arr.get_index(cx, i as u32)?.coerce_string(cx)?;
                    out.push(s);
                }
                args = Some(out);
            }
        }

        if args.is_some() && bin.is_none() {
            return Err(cx.throw_type("`spawn`: `args` requires `bin`"));
        }

        helpers::spawn_result(
            cx,
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
                let handle: ChildArcSlot = Arc::new(tokio::sync::Mutex::new(Some(s.child)));
                chrome::register_spawned_child(&handle);
                Ok((s.bin, s.cdp_url, handle, s.user_data_dir))
            },
            |cx, (bin, cdp_url, child, user_data_dir)| {
                JSBrowserProcess::new_instance(
                    cx,
                    BrowserProcessPayload {
                        child,
                        bin,
                        cdp_url,
                        user_data_dir,
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
        if !cc.is_empty() {
            if let Some(s) = cc.arg(0).try_string_read_utf8(cx) {
                url = s;
            } else if let Some(obj) = cc.arg(0).as_object() {
                if let Some(u) = get_str_prop(cx, &obj, "url") {
                    url = u;
                }
                context_id = get_str_prop(cx, &obj, "contextId");
            }
        }

        let client_cb = client.clone();
        helpers::spawn_result(
            cx,
            async move {
                let mut params = serde_json::json!({ "url": url });
                if let Some(cid) = context_id {
                    params["browserContextId"] = serde_json::Value::String(cid);
                }
                let resp = client
                    .send("Target.createTarget", Some(params), None)
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
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
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
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
                    PagePayload {
                        client: client_cb.clone(),
                        session_id,
                        target_id: target_id.clone(),
                        network: PageNetworkState::new(),
                    },
                )?;
                // `targetId` own property (readonly in the d.ts; kept writable
                // here so the native install always wins).
                let id_val = ZqValue::new_string(cx, &target_id)?;
                page.define_str(
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
    #[zq_method]
    fn close<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_browser_payload(cx, cc.this())?;
        let client = payload.client.clone();
        let chrome_child = payload.chrome_child.clone();
        let user_data_dir = payload.user_data_dir.clone();
        helpers::spawn_result(
            cx,
            async move {
                // Attach mode (no child process): don't send `Browser.close`.
                let owns_process = chrome_child
                    .try_lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false);
                if owns_process {
                    let _ = client.send("Browser.close", None, None).await;
                }
                if let Ok(mut guard) = chrome_child.try_lock()
                    && let Some(mut child) = guard.take()
                {
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
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `browser.isRunning` getter
    #[zq_method(name = "isRunning", get)]
    fn is_running<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_browser_payload(cx, cc.this())?;
        let running = payload
            .chrome_child
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(false);
        Ok(ZqValue::bool(running))
    }

    /// `browser.getVersion()`
    #[zq_method(name = "getVersion")]
    fn get_version<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_browser_payload(cx, cc.this())?.client.clone();
        helpers::spawn_result(
            cx,
            async move {
                client
                    .send("Browser.getVersion", None, None)
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `browser.getWindowBounds()`
    #[zq_method(name = "getWindowBounds")]
    fn get_window_bounds<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_browser_payload(cx, cc.this())?.client.clone();
        helpers::spawn_result(
            cx,
            async move {
                let wid = browser_window_id(&client).await?;
                client
                    .send(
                        "Browser.getWindowBounds",
                        Some(serde_json::json!({ "windowId": wid })),
                        None,
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `browser.setWindowBounds(bounds)`
    #[zq_method(name = "setWindowBounds")]
    fn set_window_bounds<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut bounds = serde_json::Map::new();
        if !cc.is_empty()
            && let Some(obj) = cc.arg(0).as_object()
        {
            if let Some(v) = get_number_prop(cx, &obj, "left") {
                bounds.insert("left".into(), serde_json::Value::from(v as i32));
            }
            if let Some(v) = get_number_prop(cx, &obj, "top") {
                bounds.insert("top".into(), serde_json::Value::from(v as i32));
            }
            if let Some(v) = get_number_prop(cx, &obj, "width") {
                bounds.insert("width".into(), serde_json::Value::from(v as i32));
            }
            if let Some(v) = get_number_prop(cx, &obj, "height") {
                bounds.insert("height".into(), serde_json::Value::from(v as i32));
            }
            if let Some(s) = get_str_prop(cx, &obj, "windowState") {
                bounds.insert("windowState".into(), serde_json::Value::String(s));
            }
        }

        let client = get_browser_payload(cx, cc.this())?.client.clone();
        helpers::spawn_result(
            cx,
            async move {
                let wid = browser_window_id(&client).await?;
                client
                    .send(
                        "Browser.setWindowBounds",
                        Some(serde_json::json!({ "windowId": wid, "bounds": bounds })),
                        None,
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
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
        let path = cc.arg(0).coerce_string(cx).unwrap_or_default();
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

    helpers::spawn_result(
        cx,
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
            JSBrowser::new_instance(
                cx,
                BrowserPayload {
                    client,
                    chrome_child: Arc::new(tokio::sync::Mutex::new(child)),
                    user_data_dir,
                },
            )
        },
    )
}

// ---------------------------------------------------------------------------
// BrowserProcess handle (Browser.spawn)
// ---------------------------------------------------------------------------

type ChildArcSlot = Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>;

pub(crate) struct BrowserProcessPayload {
    /// Shared with the spawn registry so runtime-exit cleanup can kill the
    /// process even if this handle was GC-collected.
    pub(crate) child: ChildArcSlot,
    /// Resolved browser binary path (surfaced via the `bin` getter).
    pub(crate) bin: String,
    /// Ready browser-level CDP WebSocket URL (surfaced via `cdpUrl`).
    pub(crate) cdp_url: String,
    /// Auto-created Chrome profile dir, removed once the child is reaped.
    pub(crate) user_data_dir: Option<std::path::PathBuf>,
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
        helpers::spawn_result(
            cx,
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
        let pid = p
            .child
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|c| c.id()));
        match pid {
            Some(pid) => Ok(ZqValue::int32(pid as i32)),
            None => Ok(ZqValue::null()),
        }
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
    helpers::spawn_result(
        cx,
        async move {
            client
                .send(method, params, None)
                .await
                .unwrap_or_else(|_| Err("channel closed".into()))?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

/// Resolve the browser's main window id via `Browser.getWindowForTarget`.
async fn browser_window_id(client: &cdp::CdpClient) -> Result<String, String> {
    let resp = client
        .send(
            "Browser.getWindowForTarget",
            Some(serde_json::json!({})),
            None,
        )
        .await
        .unwrap_or_else(|_| Err("channel closed".into()))?;
    resp.get("windowId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no windowId".to_string())
}
