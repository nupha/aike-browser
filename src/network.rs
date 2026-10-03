// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Network domain methods (cookies, capture, response body).

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use zuqe_zapi::*;

use crate::{
    cdp_network,
    class_page::{
        get_bool_prop, get_enum_prop, get_number_prop, get_page_payload, get_str_prop,
        send_page_void_cmd,
    },
    helpers,
};

/// `Network.CookieSameSite` values.
const COOKIE_SAME_SITE: &[&str] = &["Strict", "Lax", "None"];

/// Convert a CDP cookie JSON value to a JS object.
fn cookie_to_js<'a>(cx: ZqCtx<'a>, c: &serde_json::Value) -> Result<ZqValue<'a>, ZqError> {
    let obj = ZqValue::new_object(cx)?;
    for key in ["name", "value", "domain", "path"] {
        if let Some(s) = c.get(key).and_then(|v| v.as_str()) {
            obj.set_str(cx, key, ZqValue::new_string(cx, s)?)?;
        }
    }
    if let Some(b) = c.get("httpOnly").and_then(|v| v.as_bool()) {
        obj.set_str(cx, "httpOnly", ZqValue::bool(b))?;
    }
    if let Some(b) = c.get("secure").and_then(|v| v.as_bool()) {
        obj.set_str(cx, "secure", ZqValue::bool(b))?;
    }
    Ok(obj)
}

// ---------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------

/// `page.getCookies()`
pub(crate) fn js_page_get_cookies<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_result(
        async move {
            let val = cdp_network::get_cookies(&client, &session_id).await?;
            Ok(val
                .get("cookies")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default())
        },
        |cx, cookies: Vec<serde_json::Value>| {
            let mut js_cookies: Vec<ZqValue> = Vec::with_capacity(cookies.len());
            for c in &cookies {
                if let Ok(obj) = cookie_to_js(cx, c) {
                    js_cookies.push(obj);
                }
            }
            ZqValue::new_array(cx, &js_cookies)
        },
    )
}

/// `page.setCookie(cookie)`
pub(crate) fn js_page_set_cookie<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let cookie = cc
        .arg(0)
        .as_object()
        .ok_or_else(|| cx.throw_type("setCookie requires an object argument"))?;

    let mut params = serde_json::Map::new();
    for key in ["name", "value", "domain", "path"] {
        if let Some(v) = get_str_prop(cx, &cookie, key) {
            params.insert(key.into(), serde_json::Value::String(v));
        }
    }
    // The flag/attribute fields `Network.setCookie` accepts but a name/value
    // pair does not carry: dropping them silently stored a different cookie.
    for key in ["httpOnly", "secure"] {
        if let Some(v) = get_bool_prop(cx, &cookie, key) {
            params.insert(key.into(), serde_json::Value::Bool(v));
        }
    }
    if let Some(same_site) = get_enum_prop(cx, &cookie, "sameSite", COOKIE_SAME_SITE)? {
        params.insert("sameSite".into(), serde_json::Value::String(same_site));
    }
    if let Some(expires) = get_number_prop(cx, &cookie, "expires") {
        params.insert("expires".into(), serde_json::json!(expires));
    }

    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_result(
        async move {
            let _ = cdp_network::enable_network(&client, &session_id).await;
            cdp_network::set_cookie(&client, &session_id, serde_json::Value::Object(params))
                .await?;
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

// ---------------------------------------------------------------------------
// Network domain — P0/P1 overrides & helpers
// ---------------------------------------------------------------------------

/// `page.setUserAgent(userAgent)`
pub(crate) fn js_page_set_user_agent<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let ua = cc.arg(0).coerce_string(cx).unwrap_or_default();
    send_page_void_cmd(
        cx,
        &cc,
        "Network.setUserAgentOverride",
        Some(serde_json::json!({ "userAgent": ua })),
    )
}

/// `page.setExtraHTTPHeaders(headers)`
pub(crate) fn js_page_set_extra_http_headers<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let headers = if cc.len() > 0
        && let Some(obj) = cc.arg(0).as_object()
    {
        obj.to_json(cx).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::json!({})
    };
    send_page_void_cmd(
        cx,
        &cc,
        "Network.setExtraHTTPHeaders",
        Some(serde_json::json!({ "headers": headers })),
    )
}

/// `page.setOfflineMode(bool)`
///
/// `Network.overrideNetworkState` replaces the deprecated
/// `Network.emulateNetworkConditions`; `emulateNetworkConditionsByRule` is the
/// other half of that migration and only matters for per-URL rules.
pub(crate) fn js_page_set_offline_mode<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let offline = cc.arg_bool(cx, 0).unwrap_or(false);
    send_page_void_cmd(
        cx,
        &cc,
        "Network.overrideNetworkState",
        Some(serde_json::json!({
            "offline": offline,
            "latency": 0,
            "downloadThroughput": -1,
            "uploadThroughput": -1,
        })),
    )
}

/// `page.setCacheDisabled(bool)`
pub(crate) fn js_page_set_cache_disabled<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let disabled = cc.arg_bool(cx, 0).unwrap_or(false);
    send_page_void_cmd(
        cx,
        &cc,
        "Network.setCacheDisabled",
        Some(serde_json::json!({ "cacheDisabled": disabled })),
    )
}

/// `page.setCookies(cookies)` — batch.
pub(crate) fn js_page_set_cookies<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let mut cookies = Vec::new();
    if cc.len() > 0
        && let Some(arr) = cc.arg(0).as_array(cx)?
        && let Ok(len) = arr.length(cx)
    {
        for i in 0..len {
            let Ok(item) = arr.get_index(cx, i as u32) else {
                continue;
            };
            if let Some(obj) = item.as_object() {
                let mut c = serde_json::Map::new();
                for key in ["name", "value", "domain", "path"] {
                    if let Some(v) = get_str_prop(cx, &obj, key) {
                        c.insert(key.into(), serde_json::Value::String(v));
                    }
                }
                if let Some(b) = get_bool_prop(cx, &obj, "secure") {
                    c.insert("secure".into(), serde_json::Value::Bool(b));
                }
                if let Some(b) = get_bool_prop(cx, &obj, "httpOnly") {
                    c.insert("httpOnly".into(), serde_json::Value::Bool(b));
                }
                cookies.push(serde_json::Value::Object(c));
            }
        }
    }
    send_page_void_cmd(
        cx,
        &cc,
        "Network.setCookies",
        Some(serde_json::json!({ "cookies": cookies })),
    )
}

/// `page.deleteCookie(name, url)`
pub(crate) fn js_page_delete_cookie<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let name = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let url = cc.arg(1).coerce_string(cx).unwrap_or_default();
    let mut params = serde_json::json!({ "name": name });
    if !url.is_empty() {
        params["url"] = serde_json::Value::String(url);
    }
    send_page_void_cmd(cx, &cc, "Network.deleteCookies", Some(params))
}

/// `page.clearBrowserCookies()`
pub(crate) fn js_page_clear_browser_cookies<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    send_page_void_cmd(cx, &cc, "Network.clearBrowserCookies", None)
}

// ---------------------------------------------------------------------------
// Network capture
// ---------------------------------------------------------------------------

/// `page.getResponseBody(requestId)`
pub(crate) fn js_page_get_response_body<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let request_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_result(
        async move { cdp_network::get_response_body(&client, &session_id, &request_id).await },
        |cx, v: serde_json::Value| ZqValue::from_json(cx, &v),
    )
}

/// `page.startNetworkCapture()`
pub(crate) fn js_page_start_network_capture<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    let network = payload.network.clone();

    cx.promise_fut_result(
        async move {
            cdp_network::enable_network(&client, &session_id).await?;

            let running = Arc::new(AtomicBool::new(true));
            let captured = network.captured_requests.clone();
            let mut event_rx = client.subscribe_events();
            let session_id_cap = session_id.clone();
            let running_clone = running.clone();

            let handle = helpers::spawn(async move {
                while running_clone.load(Ordering::SeqCst) {
                    tokio::select! {
                        event = event_rx.recv() => {
                            if let Ok(evt) = event
                                && evt.session_id.as_deref() == Some(&session_id_cap)
                                && evt.method.starts_with("Network.")
                                && let Ok(mut guard) = captured.try_lock()
                            {
                                guard.push(serde_json::json!({
                                    "method": evt.method,
                                    "params": evt.params,
                                }));
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
                    }
                }
            });

            if let Ok(mut guard) = network.capture_handle.try_lock() {
                *guard = Some(handle);
            }
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

/// `page.stopNetworkCapture()`
pub(crate) fn js_page_stop_network_capture<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let network = payload.network.clone();
    if let Ok(mut guard) = network.capture_handle.try_lock()
        && let Some(handle) = guard.take()
    {
        handle.abort();
    }
    Ok(ZqValue::undefined())
}

/// `page.getCapturedRequests()`
pub(crate) fn js_page_get_captured_requests<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    let requests: Vec<serde_json::Value> =
        if let Ok(guard) = payload.network.captured_requests.try_lock() {
            guard.clone()
        } else {
            Vec::new()
        };
    ZqValue::from_json(cx, &serde_json::Value::Array(requests))
}

/// `page.clearCapturedRequests()`
pub(crate) fn js_page_clear_captured_requests<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = get_page_payload(cx, cc.this())?;
    if let Ok(mut guard) = payload.network.captured_requests.try_lock() {
        guard.clear();
    }
    Ok(ZqValue::undefined())
}

/// `page.getRequestPostData(requestId)`
pub(crate) fn js_page_get_request_post_data<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let request_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_result(
        async move {
            let _ = cdp_network::enable_network(&client, &session_id).await;
            let val = client
                .send(
                    "Network.getRequestPostData",
                    Some(serde_json::json!({ "requestId": request_id })),
                    Some(&session_id),
                )
                .await?;
            Ok(val
                .get("postData")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string())
        },
        |cx, body: String| ZqValue::new_string(cx, &body),
    )
}
