// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — the main browser tab/page abstraction.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex as AsyncMutex, broadcast};
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method};

use crate::{cdp, helpers, input, network};

// ---------------------------------------------------------------------------
// Network capture state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct PageNetworkState {
    pub(crate) capture_handle: Arc<AsyncMutex<Option<tokio::task::JoinHandle<()>>>>,
    pub(crate) captured_requests: Arc<AsyncMutex<Vec<serde_json::Value>>>,
}

impl PageNetworkState {
    pub(crate) fn new() -> Self {
        Self {
            capture_handle: Arc::new(AsyncMutex::new(None)),
            captured_requests: Arc::new(AsyncMutex::new(Vec::new())),
        }
    }
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

pub(crate) struct PagePayload {
    pub(crate) client: cdp::CdpClient,
    pub(crate) session_id: String,
    pub(crate) target_id: String,
    pub(crate) network: PageNetworkState,
}

// The payload holds no GC-managed JS values (only the `targetId` JS property,
// installed eagerly in `browser.newPage`), so `payload_no_gc` is safe here.
#[zq_class(name = "Page", payload = PagePayload, payload_no_gc)]
pub struct JSPage;

#[zq_class_impl]
impl JSPage {
    /// Pages are only created by `browser.newPage()`.
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Err(cx.throw_type("Page cannot be constructed directly; use browser.newPage()"))
    }

    // ── Page domain ──

    /// `page.navigate(url)`
    #[zq_method]
    fn navigate<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let url = cc.arg(0).coerce_string(cx).unwrap_or_default();
        send_page_void_cmd(
            cx,
            &cc,
            "Page.navigate",
            Some(serde_json::json!({ "url": url })),
        )
    }

    /// `page.waitForSelector(selector, timeoutMs?)`
    #[zq_method(name = "waitForSelector")]
    fn wait_for_selector<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
        let timeout_ms: i64 = if cc.len() > 1 {
            cc.arg_f64(cx, 1).map(|n| n as i64).unwrap_or(30000)
        } else {
            30000
        };

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        let selector_js = selector.replace('\'', "\\'");

        helpers::spawn_result(cx, 
            async move {
                // Retry with a short per-attempt timeout so that, if `navigate`
                // returns before the new document is ready, a stale-context
                // evaluate is retried once the navigation commits.
                let start = std::time::Instant::now();
                let overall = Duration::from_millis(timeout_ms.max(0) as u64);
                loop {
                    let remaining = overall.saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        return Err("selector not found or timed out".to_string());
                    }
                    let attempt_ms = remaining.as_millis().min(1000) as i64;
                    let js_code = format!(
                        "new Promise((resolve, reject) => {{\
                         let el = document.querySelector('{selector_js}');\
                         if (el) {{ resolve(true); return; }}\
                         let mo = new MutationObserver(() => {{\
                         if (document.querySelector('{selector_js}')) {{\
                         mo.disconnect(); resolve(true);\
                         }}\
                         }});\
                         mo.observe(document.body, {{ childList: true, subtree: true }});\
                         setTimeout(() => {{ mo.disconnect(); reject(new Error('timeout')); }}, {attempt_ms});\
                         }})"
                    );
                    let rx = client.send(
                        "Runtime.evaluate",
                        Some(serde_json::json!({
                            "expression": js_code,
                            "awaitPromise": true,
                        })),
                        Some(&session_id),
                    );
                    match rx.await {
                        Ok(Ok(val)) => {
                            if val.get("exceptionDetails").is_some() || val.get("error").is_some() {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                continue;
                            }
                            return Ok(());
                        }
                        Ok(Err(e)) => return Err(e),
                        Err(_) => return Err("channel closed".into()),
                    }
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.waitForFunction(expression, timeoutMs?)`
    #[zq_method(name = "waitForFunction")]
    fn wait_for_function<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let expr = cc.arg(0).coerce_string(cx).unwrap_or_default();
        let timeout_ms: i64 = if cc.len() > 1 {
            cc.arg_f64(cx, 1).map(|n| n as i64).unwrap_or(30000)
        } else {
            30000
        };

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let js = format!(
                    "new Promise((resolve, reject) => {{\
                     const deadline = Date.now() + {timeout_ms};\
                     const tick = () => {{\
                         let ok = false;\
                         try {{ ok = !!(( {expr} )); }} catch (e) {{ ok = false; }}\
                         if (ok) {{ resolve(true); return; }}\
                         if (Date.now() > deadline) {{ reject(new Error('timeout')); return; }}\
                         setTimeout(tick, 100);\
                     }};\
                     tick();\
                     }})"
                );
                let rx = client.send(
                    "Runtime.evaluate",
                    Some(serde_json::json!({ "expression": js, "awaitPromise": true })),
                    Some(&session_id),
                );
                let val = rx.await.unwrap_or_else(|_| Err("channel closed".into()))?;
                if val.get("exceptionDetails").is_some() {
                    Err("waitForFunction: condition not met or timed out".to_string())
                } else {
                    Ok(())
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.evaluate(jsCode)`
    #[zq_method]
    fn evaluate<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let js_code = cc.arg(0).coerce_string(cx).unwrap_or_default();
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let rx = client.send(
                    "Runtime.evaluate",
                    Some(serde_json::json!({
                        "expression": js_code,
                        "awaitPromise": true,
                        "returnByValue": true,
                    })),
                    Some(&session_id),
                );
                let val = rx.await.unwrap_or_else(|_| Err("channel closed".into()))?;
                if let Some(exception) = val.get("exceptionDetails") {
                    let text = exception
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("JS exception")
                        .to_string();
                    return Err(text);
                }
                if let Some(result) = val.get("result") {
                    if let Some(value) = result.get("value") {
                        return Ok(Some(value.clone()));
                    }
                    if let Some(desc) = result.get("description").and_then(|v| v.as_str()) {
                        return Ok(Some(serde_json::Value::String(desc.to_string())));
                    }
                }
                Ok(None)
            },
            |cx, v: Option<serde_json::Value>| match v {
                Some(v) => ZqValue::from_json(cx, &v),
                None => Ok(ZqValue::undefined()),
            },
        )
    }

    /// `page.getOuterHTML()`
    #[zq_method(name = "getOuterHTML")]
    fn get_outer_html<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                let val = client
                    .send(
                        "Runtime.evaluate",
                        Some(serde_json::json!({
                            "expression": "document.documentElement.outerHTML",
                        })),
                        Some(&session_id),
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                Ok(val
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string())
            },
            |cx, html: String| ZqValue::new_string(cx, &html),
        )
    }

    /// `page.captureScreenshot(options?)`
    #[zq_method(name = "captureScreenshot")]
    fn capture_screenshot<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut format = "png".to_string();
        let mut quality = None::<i32>;
        let mut clip = None::<serde_json::Value>;
        let mut capture_beyond_viewport = None::<bool>;
        let mut from_surface = None::<bool>;
        let mut optimize_for_speed = None::<bool>;

        if cc.len() > 0 && let Some(opts) = cc.arg(0).as_object() {
            if let Some(f) = get_str_prop(cx, &opts, "format") {
                format = f;
            }
            if let Some(q) = get_number_prop(cx, &opts, "quality") {
                quality = Some(q as i32);
            }
            if let Ok(Some(clip_obj)) = opts.get_object_str(cx, "clip")
                && let (Some(x), Some(y), Some(w), Some(h)) = (
                    get_number_prop(cx, &clip_obj, "x"),
                    get_number_prop(cx, &clip_obj, "y"),
                    get_number_prop(cx, &clip_obj, "width"),
                    get_number_prop(cx, &clip_obj, "height"),
                )
            {
                let mut c = serde_json::json!({ "x": x, "y": y, "width": w, "height": h });
                if let Some(s) = get_number_prop(cx, &clip_obj, "scale") {
                    c["scale"] = serde_json::json!(s);
                }
                clip = Some(c);
            }
            if let Some(fp) = get_bool_prop(cx, &opts, "fullPage") {
                capture_beyond_viewport = Some(fp);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "captureBeyondViewport") {
                capture_beyond_viewport = Some(b);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "fromSurface") {
                from_surface = Some(b);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "optimizeForSpeed") {
                optimize_for_speed = Some(b);
            }
        }

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let mut params = serde_json::json!({ "format": format });
                if let Some(q) = quality {
                    params["quality"] = serde_json::json!(q);
                }
                if let Some(c) = clip {
                    params["clip"] = c;
                }
                if let Some(b) = capture_beyond_viewport {
                    params["captureBeyondViewport"] = serde_json::json!(b);
                }
                if let Some(b) = from_surface {
                    params["fromSurface"] = serde_json::json!(b);
                }
                if let Some(b) = optimize_for_speed {
                    params["optimizeForSpeed"] = serde_json::json!(b);
                }
                let val = client
                    .send("Page.captureScreenshot", Some(params), Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                let b64 = val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "no screenshot data".to_string())?;
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("bad screenshot base64: {e}"))
            },
            |cx, bytes: Vec<u8>| ZqValue::typed_array(cx, &bytes),
        )
    }

    /// `page.setViewport(width, height)`
    #[zq_method(name = "setViewport")]
    fn set_viewport<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let width = cc.arg_f64(cx, 0).map(|n| n as i32).unwrap_or(1280).max(1);
        let height = cc.arg_f64(cx, 1).map(|n| n as i32).unwrap_or(720).max(1);
        send_page_void_cmd(
            cx,
            &cc,
            "Emulation.setDeviceMetricsOverride",
            Some(serde_json::json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": 1,
                "mobile": false,
            })),
        )
    }

    /// `page.waitForNavigation(timeoutMs?)`
    #[zq_method(name = "waitForNavigation")]
    fn wait_for_navigation<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let timeout_ms: u64 = if cc.len() > 0 {
            cc.arg_f64(cx, 0)
                .map(|n| n as i64)
                .unwrap_or(30000)
                .max(1000) as u64
        } else {
            30000
        };

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let _ = client.send("Page.enable", None, Some(&session_id)).await;
                let mut event_rx = client.subscribe_events();
                let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
                tokio::pin!(timeout);
                loop {
                    tokio::select! {
                        event = event_rx.recv() => {
                            match event {
                                Ok(evt) => {
                                    if evt.method == "Page.loadEventFired"
                                        && evt.session_id.as_deref() == Some(&session_id)
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => {
                                    return Err("CDP connection closed".to_string());
                                }
                            }
                        }
                        _ = &mut timeout => return Err("navigation timeout".to_string()),
                    }
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.handleDialog(accept, promptText?)`
    #[zq_method(name = "handleDialog")]
    fn handle_dialog<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let accept = cc.arg_bool(cx, 0).unwrap_or(false);
        let prompt_text = if cc.len() > 1 && !cc.arg(1).is_undefined() {
            cc.arg(1).try_string_read_utf8(cx)
        } else {
            None
        };
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let mut params = serde_json::json!({ "accept": accept });
                if let Some(text) = prompt_text {
                    params["promptText"] = serde_json::Value::String(text);
                }
                client
                    .send(
                        "Page.handleJavaScriptDialog",
                        Some(params),
                        Some(&session_id),
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.printToPDF(options?)`
    #[zq_method(name = "printToPDF")]
    fn print_to_pdf<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut params = serde_json::json!({ "printBackground": true });
        if cc.len() > 0 && let Some(opts) = cc.arg(0).as_object() {
            if let Some(v) = get_bool_prop(cx, &opts, "landscape") {
                params["landscape"] = serde_json::Value::Bool(v);
            }
            if let Some(v) = get_number_prop(cx, &opts, "paperWidth") {
                params["paperWidth"] = serde_json::json!(v);
            }
            if let Some(v) = get_number_prop(cx, &opts, "paperHeight") {
                params["paperHeight"] = serde_json::json!(v);
            }
            if let Some(v) = get_bool_prop(cx, &opts, "printBackground") {
                params["printBackground"] = serde_json::Value::Bool(v);
            }
        }
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let val = client
                    .send("Page.printToPDF", Some(params), Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                let b64 = val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "no PDF data".to_string())?;
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("bad PDF base64: {e}"))
            },
            |cx, bytes: Vec<u8>| ZqValue::typed_array(cx, &bytes),
        )
    }

    /// `page.uploadFile(selector, filePaths)`
    #[zq_method(name = "uploadFile")]
    fn upload_file<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();

        let mut file_paths: Vec<String> = Vec::new();
        if cc.len() > 1
            && let Ok(Some(arr)) = cc.arg(1).as_array(cx)
            && let Ok(len) = arr.length(cx)
        {
            for i in 0..len {
                if let Some(s) = arr.get_index(cx, i as u32).ok().and_then(|x| x.try_string_read_utf8(cx)) {
                    file_paths.push(s);
                }
            }
        }

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let set_files_js = file_paths
                    .iter()
                    .map(|p| {
                        format!(
                            "dt.items.add(new File([new Blob()], '{}'))",
                            p.replace('\'', "\\'")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(";\n");

                let js_code = format!(
                    "(() => {{\
                     const input = document.querySelector('{}');\
                     if (!input) throw new Error('input not found');\
                     const dt = new DataTransfer();\
                     {};\
                     input.files = dt.files;\
                     input.dispatchEvent(new Event('change', {{ bubbles: true }}));\
                     }})()",
                    selector.replace('\'', "\\'"),
                    set_files_js,
                );

                let val = client
                    .send(
                        "Runtime.evaluate",
                        Some(serde_json::json!({ "expression": js_code })),
                        Some(&session_id),
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                if val.get("exceptionDetails").is_some() {
                    Err("upload: JS exception".to_string())
                } else {
                    Ok(())
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.getLayoutMetrics()`
    #[zq_method(name = "getLayoutMetrics")]
    fn get_layout_metrics<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                client
                    .send("Page.getLayoutMetrics", None, Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `page.addInitScript(source)`
    #[zq_method(name = "addInitScript")]
    fn add_init_script<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let source = cc.arg(0).coerce_string(cx).unwrap_or_default();
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                let val = client
                    .send(
                        "Page.addScriptToEvaluateOnNewDocument",
                        Some(serde_json::json!({ "source": source })),
                        Some(&session_id),
                    )
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                Ok(val
                    .get("identifier")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string())
            },
            |cx, id: String| ZqValue::new_string(cx, &id),
        )
    }

    /// `page.getFrameTree()`
    #[zq_method(name = "getFrameTree")]
    fn get_frame_tree<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                client
                    .send("Page.getFrameTree", None, Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `page.enableRuntime()`
    #[zq_method(name = "enableRuntime")]
    fn enable_runtime<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        send_page_void_cmd(cx, &cc, "Runtime.enable", None)
    }

    /// `page.callFunctionOn(objectId, fn, args?)`
    #[zq_method(name = "callFunctionOn")]
    fn call_function_on<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::runtime::js_page_call_function_on(cx, cc)
    }

    /// `page.getProperties(objectId)`
    #[zq_method(name = "getProperties")]
    fn get_properties<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::runtime::js_page_get_properties(cx, cc)
    }

    /// `page.evaluateHandle(expr)`
    #[zq_method(name = "evaluateHandle")]
    fn evaluate_handle<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::runtime::js_page_evaluate_handle(cx, cc)
    }

    /// `page.onConsole(callback)`
    #[zq_method(name = "onConsole")]
    fn on_console<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::runtime::js_page_on_console(cx, cc)
    }

    /// `page.close()`
    #[zq_method]
    fn close<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let payload = get_page_payload(cx, cc.this())?;
        let client = payload.client.clone();
        let target_id = payload.target_id.clone();
        helpers::spawn_result(cx, 
            async move { client.close_target(&target_id).await },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    // ── Page domain — extended ──

    /// `page.setContent(html)`
    #[zq_method(name = "setContent")]
    fn set_content<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let html = cc.arg(0).coerce_string(cx).unwrap_or_default();
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        helpers::spawn_result(cx, 
            async move {
                let tree = client
                    .send("Page.getFrameTree", None, Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Ok(serde_json::Value::Null));
                let frame_id = tree
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("frameTree"))
                    .and_then(|ft| ft.get("frame"))
                    .and_then(|f| f.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                match frame_id {
                    Some(fid) => {
                        client
                            .send(
                                "Page.setDocumentContent",
                                Some(serde_json::json!({ "frameId": fid, "html": html })),
                                Some(&session_id),
                            )
                            .await
                            .unwrap_or_else(|_| Err("channel closed".into()))?;
                        Ok(())
                    }
                    None => Err("setContent: cannot resolve main frame".to_string()),
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.reload(opts?)`
    #[zq_method]
    fn reload<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut params = serde_json::json!({});
        if cc.len() > 0
            && let Some(opts) = cc.arg(0).as_object()
            && let Some(v) = get_bool_prop(cx, &opts, "ignoreCache")
        {
            params["ignoreCache"] = serde_json::Value::Bool(v);
        }
        send_page_void_cmd(cx, &cc, "Page.reload", Some(params))
    }

    /// `page.stopLoading()`
    #[zq_method(name = "stopLoading")]
    fn stop_loading<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        send_page_void_cmd(cx, &cc, "Page.stopLoading", None)
    }

    /// `page.back()`
    #[zq_method]
    fn back<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        navigate_history(cx, &cc, -1)
    }

    /// `page.forward()`
    #[zq_method]
    fn forward<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        navigate_history(cx, &cc, 1)
    }

    /// `page.getNavigationHistory()`
    #[zq_method(name = "getNavigationHistory")]
    fn get_navigation_history<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                client
                    .send("Page.getNavigationHistory", None, Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `page.bringToFront()`
    #[zq_method(name = "bringToFront")]
    fn bring_to_front<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        send_page_void_cmd(cx, &cc, "Page.bringToFront", None)
    }

    /// `page.captureSnapshot(opts?)`
    #[zq_method(name = "captureSnapshot")]
    fn capture_snapshot<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut params = serde_json::json!({ "format": "mhtml" });
        if cc.len() > 0
            && let Some(opts) = cc.arg(0).as_object()
            && let Some(f) = get_str_prop(cx, &opts, "format")
        {
            params["format"] = serde_json::Value::String(f);
        }
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        helpers::spawn_result(cx, 
            async move {
                let val = client
                    .send("Page.captureSnapshot", Some(params), Some(&session_id))
                    .await
                    .unwrap_or_else(|_| Err("channel closed".into()))?;
                Ok(val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string())
            },
            |cx, data: String| ZqValue::new_string(cx, &data),
        )
    }

    /// `page.setDownloadBehavior(path)`
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
        send_page_void_cmd(cx, &cc, "Page.setDownloadBehavior", Some(params))
    }

    /// `page.setBypassCSP(enabled)`
    #[zq_method(name = "setBypassCSP")]
    fn set_bypass_csp<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
        send_page_void_cmd(
            cx,
            &cc,
            "Page.setBypassCSP",
            Some(serde_json::json!({ "enabled": enabled })),
        )
    }

    // ── Input domain ──

    #[zq_method(name = "mouseClick")]
    fn mouse_click<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_mouse_click(cx, cc)
    }

    #[zq_method(name = "mouseMove")]
    fn mouse_move<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_mouse_move(cx, cc)
    }

    #[zq_method(name = "mouseDown")]
    fn mouse_down<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_mouse_down(cx, cc)
    }

    #[zq_method(name = "mouseUp")]
    fn mouse_up<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_mouse_up(cx, cc)
    }

    #[zq_method(name = "dragAndDrop")]
    fn drag_and_drop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_drag_and_drop(cx, cc)
    }

    #[zq_method(name = "keyPress")]
    fn key_press<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_key_press(cx, cc)
    }

    #[zq_method(name = "insertText")]
    fn insert_text<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        input::js_page_insert_text(cx, cc)
    }

    // ── DOM domain ──

    #[zq_method(name = "querySelector")]
    fn query_selector<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_query_selector(cx, cc)
    }

    #[zq_method(name = "querySelectorAll")]
    fn query_selector_all<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_query_selector_all(cx, cc)
    }

    #[zq_method(name = "getAttribute")]
    fn get_attribute<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_get_attribute(cx, cc)
    }

    #[zq_method]
    fn focus<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_focus(cx, cc)
    }

    #[zq_method(name = "getBoxModel")]
    fn get_box_model<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_get_box_model(cx, cc)
    }

    #[zq_method]
    fn remove<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_remove(cx, cc)
    }

    #[zq_method(name = "getNodeValue")]
    fn get_node_value<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_get_node_value(cx, cc)
    }

    #[zq_method]
    fn click<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_click(cx, cc)
    }

    #[zq_method(name = "type")]
    fn type_<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_type(cx, cc)
    }

    #[zq_method(name = "ariaSnapshot")]
    fn aria_snapshot<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_aria_snapshot(cx, cc)
    }

    #[zq_method(name = "clickBackendNode")]
    fn click_backend_node<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_click_backend_node(cx, cc)
    }

    #[zq_method(name = "typeBackendNode")]
    fn type_backend_node<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::dom::js_page_type_backend_node(cx, cc)
    }

    // ── Emulation domain ──

    #[zq_method(name = "setGeolocation")]
    fn set_geolocation<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_geolocation(cx, cc)
    }

    #[zq_method(name = "setMediaFeatures")]
    fn set_media_features<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_media_features(cx, cc)
    }

    #[zq_method(name = "setTimezone")]
    fn set_timezone<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_timezone(cx, cc)
    }

    #[zq_method(name = "setLocale")]
    fn set_locale<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_locale(cx, cc)
    }

    #[zq_method(name = "setTouchEnabled")]
    fn set_touch_enabled<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_touch_enabled(cx, cc)
    }

    #[zq_method(name = "setCPUThrottling")]
    fn set_cpu_throttling<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::emulation::js_page_set_cpu_throttling(cx, cc)
    }

    // ── Fetch domain ──

    #[zq_method(name = "setRequestInterception")]
    fn set_request_interception<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        crate::fetch::js_page_set_request_interception(cx, cc)
    }

    #[zq_method(name = "continueRequest")]
    fn continue_request<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::fetch::js_page_continue_request(cx, cc)
    }

    #[zq_method(name = "abortRequest")]
    fn abort_request<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::fetch::js_page_abort_request(cx, cc)
    }

    #[zq_method(name = "respondRequest")]
    fn respond_request<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::fetch::js_page_respond_request(cx, cc)
    }

    #[zq_method(name = "onRequestPaused")]
    fn on_request_paused<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::fetch::js_page_on_request_paused(cx, cc)
    }

    // ── Network domain ──

    #[zq_method(name = "getCookies")]
    fn get_cookies<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_get_cookies(cx, cc)
    }

    #[zq_method(name = "setCookie")]
    fn set_cookie<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_cookie(cx, cc)
    }

    #[zq_method(name = "getResponseBody")]
    fn get_response_body<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_get_response_body(cx, cc)
    }

    #[zq_method(name = "getRequestPostData")]
    fn get_request_post_data<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_get_request_post_data(cx, cc)
    }

    #[zq_method(name = "startNetworkCapture")]
    fn start_network_capture<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_start_network_capture(cx, cc)
    }

    #[zq_method(name = "stopNetworkCapture")]
    fn stop_network_capture<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_stop_network_capture(cx, cc)
    }

    #[zq_method(name = "getCapturedRequests")]
    fn get_captured_requests<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_get_captured_requests(cx, cc)
    }

    #[zq_method(name = "clearCapturedRequests")]
    fn clear_captured_requests<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_clear_captured_requests(cx, cc)
    }

    #[zq_method(name = "setUserAgent")]
    fn set_user_agent<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_user_agent(cx, cc)
    }

    #[zq_method(name = "setExtraHTTPHeaders")]
    fn set_extra_http_headers<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_extra_http_headers(cx, cc)
    }

    #[zq_method(name = "setOfflineMode")]
    fn set_offline_mode<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_offline_mode(cx, cc)
    }

    #[zq_method(name = "setCacheDisabled")]
    fn set_cache_disabled<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_cache_disabled(cx, cc)
    }

    #[zq_method(name = "setCookies")]
    fn set_cookies<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_set_cookies(cx, cc)
    }

    #[zq_method(name = "deleteCookie")]
    fn delete_cookie<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_delete_cookie(cx, cc)
    }

    #[zq_method(name = "clearBrowserCookies")]
    fn clear_browser_cookies<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_clear_browser_cookies(cx, cc)
    }

    // ── Log domain ──

    #[zq_method(name = "onLog")]
    fn on_log<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::log::js_page_on_log(cx, cc)
    }

    #[zq_method(name = "getConsoleMessages")]
    fn get_console_messages<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::log::js_page_get_console_messages(cx, cc)
    }

    // ── Storage domain ──

    #[zq_method(name = "clearData")]
    fn clear_data<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::storage::js_page_clear_data(cx, cc)
    }

    #[zq_method(name = "getStorageKeyForFrame")]
    fn get_storage_key_for_frame<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        crate::storage::js_page_get_storage_key_for_frame(cx, cc)
    }

    // ── Overlay domain ──

    #[zq_method]
    fn highlight<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::overlay::js_page_highlight(cx, cc)
    }

    #[zq_method(name = "hideHighlight")]
    fn hide_highlight<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::overlay::js_page_hide_highlight(cx, cc)
    }

    // ── CSS domain ──

    #[zq_method(name = "getComputedStyle")]
    fn get_computed_style<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::css::js_page_get_computed_style(cx, cc)
    }

    #[zq_method(name = "getMatchedStyles")]
    fn get_matched_styles<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::css::js_page_get_matched_styles(cx, cc)
    }

    // ── IO domain ──

    #[zq_method(name = "readStream")]
    fn read_stream<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::io::js_page_read_stream(cx, cc)
    }

    // ── Tracing domain ──

    #[zq_method(name = "startTracing")]
    fn start_tracing<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::tracing::js_page_start_tracing(cx, cc)
    }

    #[zq_method(name = "stopTracing")]
    fn stop_tracing<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::tracing::js_page_stop_tracing(cx, cc)
    }
}

// ---------------------------------------------------------------------------
// Payload accessor
// ---------------------------------------------------------------------------

pub(crate) fn get_page_payload<'a>(
    cx: ZqCtx<'a>,
    val: ZqValue<'a>,
) -> Result<&'a PagePayload, ZqError> {
    JSPage::payload(cx, val).ok_or_else(|| cx.throw_type("not a Page instance"))
}

/// Clone the CDP client out of a Page instance.
pub(crate) fn get_page_client(cx: ZqCtx, val: ZqValue) -> Result<cdp::CdpClient, ZqError> {
    get_page_payload(cx, val).map(|p| p.client.clone())
}

/// Clone the session id out of a Page instance.
pub(crate) fn get_page_session(cx: ZqCtx, val: ZqValue) -> Result<String, ZqError> {
    get_page_payload(cx, val).map(|p| p.session_id.clone())
}

// ---------------------------------------------------------------------------
// Shared helper: void-returning CDP command
// ---------------------------------------------------------------------------

pub(crate) fn send_page_void_cmd<'a>(
    cx: ZqCtx<'a>,
    cc: &ZqCallInfo<'a>,
    method: &'static str,
    params: Option<serde_json::Value>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    helpers::spawn_result(cx, 
        async move {
            client
                .send(method, params, Some(&session_id))
                .await
                .unwrap_or_else(|_| Err("channel closed".into()))?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

pub(crate) fn get_str_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Option<String> {
    obj.get_string_str(cx, key).ok().flatten()
}

pub(crate) fn get_number_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Option<f64> {
    obj.get_f64_str(cx, key).ok().flatten()
}

pub(crate) fn get_bool_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Option<bool> {
    obj.get_bool_str(cx, key).ok().flatten()
}

/// Shared helper for `back()` / `forward()`: step `delta` entries through the
/// navigation history.
fn navigate_history<'a>(
    cx: ZqCtx<'a>,
    cc: &ZqCallInfo<'a>,
    delta: i64,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    helpers::spawn_result(cx, 
        async move {
            let hist = client
                .send("Page.getNavigationHistory", None, Some(&session_id))
                .await
                .unwrap_or_else(|_| Err("channel closed".into()))?;
            let current = hist
                .get("currentIndex")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let entries = hist.get("entries").and_then(|v| v.as_array());
            let target = current + delta;
            let id = entries
                .and_then(|arr| arr.get(target as usize))
                .and_then(|e| e.get("id"))
                .and_then(|v| v.as_i64());
            let Some(entry_id) = id else {
                return Err("no history entry in that direction".to_string());
            };
            client
                .send(
                    "Page.navigateToHistoryEntry",
                    Some(serde_json::json!({ "entryId": entry_id })),
                    Some(&session_id),
                )
                .await
                .unwrap_or_else(|_| Err("channel closed".into()))?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}
