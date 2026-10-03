// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::broadcast;
use zuqe_zapi::*;
use zuqe_zapi_macros::{zq_class, zq_class_impl, zq_method};

use crate::{args, cdp, helpers, input, log::LogBuffer, network};

// ---------------------------------------------------------------------------
// Network capture state
// ---------------------------------------------------------------------------

/// Retained capture entries. `Network.dataReceived` / `dataReceived`-class
/// events fire thousands of times for a single page load, so an unbounded
/// vector turned "capture a page" into an allocation that never stops growing.
pub(crate) const CAPTURE_CAP: usize = 5000;

/// `Page.captureScreenshot.format` values.
const SCREENSHOT_FORMATS: &[&str] = &["png", "jpeg", "webp"];

/// Network capture state for one page.
///
/// A plain `Mutex`, not `tokio::sync::Mutex`: every critical section here is a
/// few instructions with no `await` inside, and `try_lock` (which
/// `stopNetworkCapture` used) silently skipped the very stop it was asked for.
#[derive(Clone)]
pub(crate) struct PageNetworkState {
    /// The event-forwarding task. Each `startNetworkCapture` replaces (and
    /// aborts) it, `stopNetworkCapture` takes it.
    capture_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    captured_requests: Arc<Mutex<VecDeque<serde_json::Value>>>,
}

impl PageNetworkState {
    pub(crate) fn new() -> Self {
        Self {
            capture_handle: Arc::new(Mutex::new(None)),
            captured_requests: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Install a capture task, aborting the one it replaces 鈥?a second
    /// `startNetworkCapture` used to drop the first handle on the floor and
    /// leave its task running forever.
    pub(crate) fn start_capture(&self, task: tokio::task::JoinHandle<()>) {
        let replaced = self
            .capture_handle
            .lock()
            .map(|mut slot| slot.replace(task))
            .unwrap_or(None);
        if let Some(old) = replaced {
            old.abort();
        }
    }

    pub(crate) fn stop_capture(&self) {
        let stopped = self
            .capture_handle
            .lock()
            .map(|mut slot| slot.take())
            .unwrap_or(None);
        if let Some(task) = stopped {
            task.abort();
        }
    }

    pub(crate) fn record(&self, entry: serde_json::Value) {
        if let Ok(mut buf) = self.captured_requests.lock() {
            buf.push_back(entry);
            while buf.len() > CAPTURE_CAP {
                buf.pop_front();
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<serde_json::Value> {
        self.captured_requests
            .lock()
            .map(|buf| buf.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut buf) = self.captured_requests.lock() {
            buf.clear();
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
    /// Console/log ring, owned here so `page.close()` frees it (see B-050).
    pub(crate) log_buffer: LogBuffer,
}

/// A fresh `PagePayload` for `session_id`, with empty capture and log state.
pub(crate) fn new_page_payload(
    client: cdp::CdpClient,
    session_id: String,
    target_id: String,
) -> PagePayload {
    PagePayload {
        client,
        session_id,
        target_id,
        network: PageNetworkState::new(),
        log_buffer: Arc::new(Mutex::new(VecDeque::new())),
    }
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

    /// `page.navigate(url)` 鈥?rejects when the navigation itself failed.
    #[zq_method]
    fn navigate<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let url = args::arg_string(cx, &cc, 0, "url")?;
        let payload = get_page_payload(cx, cc.this())?;
        let client = payload.client.clone();
        let session_id = payload.session_id.clone();
        cx.promise_fut_result(
            async move {
                let resp = client
                    .send(
                        "Page.navigate",
                        Some(serde_json::json!({ "url": url })),
                        Some(&session_id),
                    )
                    .await?;
                // `Page.navigate` answers with success even when the browser
                // could not load the URL 鈥?the failure is in `errorText`
                // (measured: a refused port yields `net::ERR_UNSAFE_PORT`).
                // Discarding the result made a dead navigation resolve.
                if let Some(err) = resp
                    .get("errorText")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    return Err(format!("navigate failed: {err}"));
                }
                if resp.get("isDownload").and_then(|v| v.as_bool()) == Some(true) {
                    return Err("navigate resulted in a download, not a page".to_string());
                }
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.waitForSelector(selector, timeoutMs?)`
    #[zq_method(name = "waitForSelector")]
    fn wait_for_selector<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let selector = args::arg_string(cx, &cc, 0, "selector")?;
        let timeout_ms = timeout_ms_or_default(cx, &cc, 1, 30_000)?;

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        // A JS string literal, JSON-encoded: splicing the selector into `'...'`
        // broke on a quote in it and turned a syntax error into a full-timeout
        // wait.
        let selector_lit = serde_json::to_string(&selector)
            .map_err(|e| cx.throw_type(&format!("selector is not representable: {e}")))?;

        cx.promise_fut_result(async move {
                // `document.body` is null until the parser reaches <body>, so
                // the watcher polls until there is something to observe.
                // Without that guard `mo.observe(null)` threw a TypeError on
                // every document that had not finished parsing yet.
                let js_code = |attempt_ms: i64| {
                    format!(
                        "new Promise((resolve, reject) => {{\
                         const SEL = {selector_lit};\
                         let mo = null;\
                         const done = () => {{ if (mo) mo.disconnect(); resolve(true); }};\
                         const watch = () => {{\
                         if (document.querySelector(SEL)) {{ done(); return; }}\
                         if (document.body) {{\
                         mo = new MutationObserver(() => {{\
                         if (document.querySelector(SEL)) done();\
                         }});\
                         mo.observe(document.body, {{ childList: true, subtree: true }});\
                         }} else {{ setTimeout(watch, 50); }}\
                         }};\
                         watch();\
                         setTimeout(() => {{ if (mo) mo.disconnect(); reject(new Error('timeout')); }}, {attempt_ms});\
                         }})"
                    )
                };

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
                    // Bound the round-trip by the caller's own budget. The
                    // global 30s timeout meant a single stalled evaluate blew
                    // through `timeoutMs` and surfaced a 30s error instead of
                    // just retrying; a dead connection is still fatal.
                    let bound = remaining.min(Duration::from_millis(2000));
                    let val = match client
                        .send_timeout(
                            "Runtime.evaluate",
                            Some(serde_json::json!({
                                "expression": js_code(attempt_ms),
                                "awaitPromise": true,
                            })),
                            Some(&session_id),
                            bound,
                        )
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            if client.is_closed() {
                                return Err(e);
                            }
                            log::debug!("waitForSelector: evaluate did not answer ({e}); retrying");
                            if start.elapsed() >= overall {
                                return Err("selector not found or timed out".to_string());
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    };
                    let Some(details) = val.get("exceptionDetails") else {
                        return Ok(());
                    };
                    // Only a selector that no amount of waiting can satisfy is
                    // reported early; everything else (a lost execution context,
                    // a document still parsing) is retried until the caller's
                    // own deadline.
                    let msg = helpers::format_exception(details);
                    if is_bad_selector(&msg) {
                        return Err(format!("waitForSelector: {msg}"));
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.waitForFunction(expression, timeoutMs?)`
    #[zq_method(name = "waitForFunction")]
    fn wait_for_function<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let expr = args::arg_string(cx, &cc, 0, "expression")?;
        let timeout_ms = timeout_ms_or_default(cx, &cc, 1, 30_000)?;

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
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
                // The generated promise carries the whole budget; bound the
                // round-trip just past it so a stalled evaluate reports the
                // timeout instead of the global 30s.
                let bound = Duration::from_millis(timeout_ms.max(0) as u64 + 1000);
                let val = client
                    .send_timeout(
                        "Runtime.evaluate",
                        Some(serde_json::json!({ "expression": js, "awaitPromise": true })),
                        Some(&session_id),
                        bound,
                    )
                    .await?;
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
        let js_code = args::arg_string(cx, &cc, 0, "jsCode")?;
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
            async move {
                let val = client
                    .send(
                        "Runtime.evaluate",
                        Some(serde_json::json!({
                            "expression": js_code,
                            "awaitPromise": true,
                            "returnByValue": true,
                        })),
                        Some(&session_id),
                    )
                    .await?;
                if let Some(exception) = val.get("exceptionDetails") {
                    return Err(helpers::format_exception(exception));
                }
                let result = val.get("result");
                if let Some(value) = result.and_then(|r| r.get("value")) {
                    return Ok(Some(value.clone()));
                }
                // NaN, Infinity, BigInt and friends carry no `value`; reading
                // `description` alone would turn them into an opaque string.
                if let Some(unserializable) = result.and_then(|r| r.get("unserializableValue")) {
                    return Ok(Some(unserializable.clone()));
                }
                if let Some(desc) = result
                    .and_then(|r| r.get("description"))
                    .and_then(|v| v.as_str())
                {
                    return Ok(Some(serde_json::Value::String(desc.to_string())));
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
        cx.promise_fut_result(
            async move {
                let val = client
                    .send(
                        "Runtime.evaluate",
                        Some(serde_json::json!({
                            "expression": "document.documentElement.outerHTML",
                        })),
                        Some(&session_id),
                    )
                    .await?;
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

        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
        {
            if let Some(f) = get_enum_prop(cx, &opts, "format", SCREENSHOT_FORMATS)? {
                format = f;
            }
            if let Some(q) = get_i64_prop(cx, &opts, "quality")? {
                // `quality` is 1-100, and only the lossy formats accept it.
                // `as i32` used to truncate `1.9` to 1 and pass `300` through to
                // Chrome; `{}.valueOf()` produced 0.
                quality = Some(args::in_range(cx, q, "quality", 1, 100)? as i32);
            }
            if let Some(clip_obj) = get_object_prop(cx, &opts, "clip")? {
                let req = |key: &str| -> Result<f64, ZqError> {
                    get_number_prop(cx, &clip_obj, key)?
                        .ok_or_else(|| cx.throw_type(&format!("`clip.{key}` is required")))
                };
                let mut c = serde_json::json!({
                    "x": req("x")?,
                    "y": req("y")?,
                    "width": req("width")?,
                    "height": req("height")?,
                });
                if let Some(s) = get_number_prop(cx, &clip_obj, "scale")? {
                    c["scale"] = serde_json::json!(s);
                }
                clip = Some(c);
            }
            if let Some(fp) = get_bool_prop(cx, &opts, "fullPage")? {
                capture_beyond_viewport = Some(fp);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "captureBeyondViewport")? {
                capture_beyond_viewport = Some(b);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "fromSurface")? {
                from_surface = Some(b);
            }
            if let Some(b) = get_bool_prop(cx, &opts, "optimizeForSpeed")? {
                optimize_for_speed = Some(b);
            }
        }

        if quality.is_some() && format == "png" {
            return Err(cx.throw_range("`quality` applies only to the `jpeg` and `webp` formats"));
        }

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
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
                    .await?;
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
        // Positive integers only: the old `as i32` silently clamped `-1` to 1,
        // truncated `2.7` to 2 and saturated `1e12` to `i32::MAX`.
        let width = args::in_range(cx, args::arg_i64(cx, &cc, 0, "width")?, "width", 1, 100_000)?;
        let height = args::in_range(
            cx,
            args::arg_i64(cx, &cc, 1, "height")?,
            "height",
            1,
            100_000,
        )?;
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
    ///
    /// Waits for a cross-document load *or* a same-document (SPA) navigation:
    /// listening only for `Page.loadEventFired` meant `history.pushState`,
    /// a hash change or a link click could never satisfy it.
    #[zq_method(name = "waitForNavigation")]
    fn wait_for_navigation<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let timeout_ms = timeout_ms_or_default(cx, &cc, 0, 30_000)? as u64;

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
            async move {
                // Subscribe first: `Page.enable` and the navigation it is waiting
                // for can both complete before a subscriber created afterwards
                // looks at the stream, and the event is gone for good.
                let mut event_rx = client.subscribe_events();
                client.send("Page.enable", None, Some(&session_id)).await?;
                let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
                tokio::pin!(timeout);
                loop {
                    tokio::select! {
                        event = event_rx.recv() => {
                            match event {
                                Ok(evt) => {
                                    if evt.session_id.as_deref() == Some(&session_id)
                                        && matches!(
                                            evt.method.as_str(),
                                            "Page.loadEventFired" | "Page.navigatedWithinDocument"
                                        )
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    log::warn!("waitForNavigation lagged by {n} events");
                                    continue;
                                }
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
        let accept = args::arg_bool(cx, &cc, 0, "accept")?;
        let prompt_text = args::opt_arg_string(cx, &cc, 1, "promptText")?;
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
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
                    .await?;
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.printToPDF(options?)`
    #[zq_method(name = "printToPDF")]
    fn print_to_pdf<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let mut params = serde_json::json!({ "printBackground": true });
        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
        {
            if let Some(v) = get_bool_prop(cx, &opts, "landscape")? {
                params["landscape"] = serde_json::Value::Bool(v);
            }
            if let Some(v) = get_number_prop(cx, &opts, "paperWidth")? {
                params["paperWidth"] = serde_json::json!(v);
            }
            if let Some(v) = get_number_prop(cx, &opts, "paperHeight")? {
                params["paperHeight"] = serde_json::json!(v);
            }
            if let Some(v) = get_bool_prop(cx, &opts, "printBackground")? {
                params["printBackground"] = serde_json::Value::Bool(v);
            }
        }
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
            async move {
                let val = client
                    .send("Page.printToPDF", Some(params), Some(&session_id))
                    .await?;
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
        let selector = args::arg_string(cx, &cc, 0, "selector")?;
        let arr = cc
            .arg(1)
            .as_array(cx)?
            .ok_or_else(|| cx.throw_type("`filePaths` must be an array"))?;
        let len = arr.length(cx)?;
        let mut file_paths: Vec<String> = Vec::with_capacity(len);
        for i in 0..len {
            let item = arr.get_index(cx, i as u32)?;
            file_paths.push(args::to_string(cx, item, &format!("filePaths[{i}]"))?);
        }

        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
            async move {
                // `DOM.setFileInputFiles` hands the input the real files; building
                // a `DataTransfer` in page script produced empty `File` objects
                // named after the whole path.
                let node_id = crate::dom::resolve_selector(&client, &session_id, &selector).await?;
                client
                    .send(
                        "DOM.setFileInputFiles",
                        Some(serde_json::json!({ "files": file_paths, "nodeId": node_id })),
                        Some(&session_id),
                    )
                    .await?;
                Ok(())
            },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    /// `page.getLayoutMetrics()`
    #[zq_method(name = "getLayoutMetrics")]
    fn get_layout_metrics<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        cx.promise_fut_result(
            async move {
                client
                    .send("Page.getLayoutMetrics", None, Some(&session_id))
                    .await
            },
            |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
        )
    }

    /// `page.addInitScript(source)`
    #[zq_method(name = "addInitScript")]
    fn add_init_script<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let source = args::arg_string(cx, &cc, 0, "source")?;
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        cx.promise_fut_result(
            async move {
                let val = client
                    .send(
                        "Page.addScriptToEvaluateOnNewDocument",
                        Some(serde_json::json!({ "source": source })),
                        Some(&session_id),
                    )
                    .await?;
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
        cx.promise_fut_result(
            async move {
                client
                    .send("Page.getFrameTree", None, Some(&session_id))
                    .await
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
        cx.promise_fut_result(
            async move { client.close_target(&target_id).await },
            |_cx, ()| Ok(ZqValue::undefined()),
        )
    }

    // 鈹€鈹€ Page domain 鈥?extended 鈹€鈹€

    /// `page.setContent(html)`
    #[zq_method(name = "setContent")]
    fn set_content<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let html = args::arg_string(cx, &cc, 0, "html")?;
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;

        cx.promise_fut_result(
            async move {
                let tree = client
                    .send("Page.getFrameTree", None, Some(&session_id))
                    .await
                    .unwrap_or(serde_json::Value::Null);
                let frame_id = tree
                    .get("frameTree")
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
                            .await?;
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
        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
            && let Some(v) = get_bool_prop(cx, &opts, "ignoreCache")?
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
        cx.promise_fut_result(
            async move {
                client
                    .send("Page.getNavigationHistory", None, Some(&session_id))
                    .await
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
        if !cc.is_empty()
            && let Some(opts) = cc.arg(0).as_object()
            && let Some(f) = get_str_prop(cx, &opts, "format")?
        {
            params["format"] = serde_json::Value::String(f);
        }
        let client = get_page_client(cx, cc.this())?;
        let session_id = get_page_session(cx, cc.this())?;
        cx.promise_fut_result(
            async move {
                let val = client
                    .send("Page.captureSnapshot", Some(params), Some(&session_id))
                    .await?;
                Ok(val
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string())
            },
            |cx, data: String| ZqValue::new_string(cx, &data),
        )
    }

    /// `page.setBypassCSP(enabled)`
    #[zq_method(name = "setBypassCSP")]
    fn set_bypass_csp<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let enabled = args::arg_bool(cx, &cc, 0, "enabled")?;
        send_page_void_cmd(
            cx,
            &cc,
            "Page.setBypassCSP",
            Some(serde_json::json!({ "enabled": enabled })),
        )
    }

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

    // 鈹€鈹€ DOM domain 鈹€鈹€

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

    // 鈹€鈹€ Emulation domain 鈹€鈹€

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

    // 鈹€鈹€ Fetch domain 鈹€鈹€

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

    // 鈹€鈹€ Network domain 鈹€鈹€

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
    fn get_request_post_data<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_get_request_post_data(cx, cc)
    }

    #[zq_method(name = "startNetworkCapture")]
    fn start_network_capture<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_start_network_capture(cx, cc)
    }

    #[zq_method(name = "stopNetworkCapture")]
    fn stop_network_capture<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        network::js_page_stop_network_capture(cx, cc)
    }

    #[zq_method(name = "getCapturedRequests")]
    fn get_captured_requests<'a>(
        cx: ZqCtx<'a>,
        cc: ZqCallInfo<'a>,
    ) -> Result<ZqValue<'a>, ZqError> {
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

    // 鈹€鈹€ Log domain 鈹€鈹€

    #[zq_method(name = "onLog")]
    fn on_log<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::log::js_page_on_log(cx, cc)
    }

    #[zq_method(name = "getConsoleMessages")]
    fn get_console_messages<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::log::js_page_get_console_messages(cx, cc)
    }

    // 鈹€鈹€ Storage domain 鈹€鈹€

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

    // 鈹€鈹€ Overlay domain 鈹€鈹€

    #[zq_method]
    fn highlight<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::overlay::js_page_highlight(cx, cc)
    }

    #[zq_method(name = "hideHighlight")]
    fn hide_highlight<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::overlay::js_page_hide_highlight(cx, cc)
    }

    // 鈹€鈹€ CSS domain 鈹€鈹€

    #[zq_method(name = "getComputedStyle")]
    fn get_computed_style<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::css::js_page_get_computed_style(cx, cc)
    }

    #[zq_method(name = "getMatchedStyles")]
    fn get_matched_styles<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::css::js_page_get_matched_styles(cx, cc)
    }

    // 鈹€鈹€ IO domain 鈹€鈹€

    #[zq_method(name = "readStream")]
    fn read_stream<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        crate::io::js_page_read_stream(cx, cc)
    }

    // 鈹€鈹€ Tracing domain 鈹€鈹€

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
    cx.promise_fut_result(
        async move {
            client.send(method, params, Some(&session_id)).await?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Optional-property readers. `undefined` means "not provided"; a present value
/// of the wrong type throws (naming the key), per the strict argument policy in
/// [`crate::args`]. `null` is a value, not a default, and is rejected too.
///
/// The one-line aliases stay because they read better at the ~60 call sites than
/// `args::opt_string_prop(cx, &opts, "format")`.
pub(crate) fn get_str_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Result<Option<String>, ZqError> {
    args::opt_string_prop(cx, obj, key)
}

pub(crate) fn get_number_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Result<Option<f64>, ZqError> {
    args::opt_f64_prop(cx, obj, key)
}

pub(crate) fn get_i64_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Result<Option<i64>, ZqError> {
    args::opt_i64_prop(cx, obj, key)
}

pub(crate) fn get_bool_prop(cx: ZqCtx, obj: &ZqValue, key: &str) -> Result<Option<bool>, ZqError> {
    args::opt_bool_prop(cx, obj, key)
}

/// Optional nested object property; a non-object value throws.
pub(crate) fn get_object_prop<'a>(
    cx: ZqCtx<'a>,
    obj: &ZqValue<'a>,
    key: &str,
) -> Result<Option<ZqValue<'a>>, ZqError> {
    args::opt_object_prop(cx, obj, key)
}

/// Optional nested array property; a non-array value throws.
pub(crate) fn get_array_prop<'a>(
    cx: ZqCtx<'a>,
    obj: &ZqValue<'a>,
    key: &str,
) -> Result<Option<ZqValue<'a>>, ZqError> {
    args::opt_array_prop(cx, obj, key)
}

/// Optional enum-valued string property, validated against `allowed` (`None`
/// when absent). A wrong type throws a `TypeError`; an unknown value throws a
/// `RangeError` naming the property, the rejected value and the full set.
pub(crate) fn get_enum_prop(
    cx: ZqCtx,
    obj: &ZqValue,
    key: &str,
    allowed: &[&str],
) -> Result<Option<String>, ZqError> {
    let Some(v) = args::opt_string_prop(cx, obj, key)? else {
        return Ok(None);
    };
    if allowed.contains(&v.as_str()) {
        Ok(Some(v))
    } else {
        Err(cx.throw_range(&format!(
            "`{key}` must be one of {}; got {v:?}",
            allowed.join("|")
        )))
    }
}

/// True for the one `waitForSelector` failure that waiting cannot fix: the
/// selector itself does not parse. Chrome words it as a `SyntaxError` from
/// `querySelector`; everything else it can throw (lost execution context, a
/// document still parsing) is worth another round.
///
/// Message sniffing rather than a strict classification because the wording is
/// V8's, not the protocol's, and the caller's timeout remains the backstop.
fn is_bad_selector(message: &str) -> bool {
    message.contains("SyntaxError") && message.contains("selector")
}

/// Resolve an optional `timeoutMs` argument.
///
/// Only an omitted (`undefined`) argument means "use the default"; anything else
/// must be a non-negative integer. The old `ToNumber`-then-`as i64` path turned
/// `undefined` (NaN) and a typo alike into `0`, i.e. an immediate timeout, and
/// `waitForNavigation(undefined)` landed on `0.max(1000)` rather than the
/// documented 30000.
fn timeout_ms_or_default(
    cx: ZqCtx,
    cc: &ZqCallInfo,
    nth: usize,
    default: i64,
) -> Result<i64, ZqError> {
    match args::opt_arg_i64(cx, cc, nth, "timeoutMs")? {
        Some(n) => args::in_range(cx, n, "timeoutMs", 0, i64::MAX),
        None => Ok(default),
    }
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
    cx.promise_fut_result(
        async move {
            let hist = client
                .send("Page.getNavigationHistory", None, Some(&session_id))
                .await?;
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
                .await?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}
