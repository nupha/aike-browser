// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Network domain methods (cookies, capture, response body).

use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    args, cdp_network,
    class_page::{
        get_bool_prop, get_enum_prop, get_number_prop, get_page_payload, get_str_prop,
        send_page_void_cmd,
    },
    helpers,
};

/// `Network.CookieSameSite` values.
const COOKIE_SAME_SITE: &[&str] = &["Strict", "Lax", "None"];

/// Copy the cookie fields `Network.setCookie` accepts from `cookie` into
/// `params`, so `getCookies()` → `setCookies()` round-trips without silently
/// dropping attributes.
fn cookie_fields_to_params(
    cx: ZqCtx,
    cookie: &ZqValue,
    params: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), ZqError> {
    for key in ["name", "value", "domain", "path"] {
        if let Some(v) = get_str_prop(cx, cookie, key)? {
            params.insert(key.into(), serde_json::Value::String(v));
        }
    }
    for key in ["httpOnly", "secure"] {
        if let Some(v) = get_bool_prop(cx, cookie, key)? {
            params.insert(key.into(), serde_json::Value::Bool(v));
        }
    }
    if let Some(same_site) = get_enum_prop(cx, cookie, "sameSite", COOKIE_SAME_SITE)? {
        params.insert("sameSite".into(), serde_json::Value::String(same_site));
    }
    if let Some(expires) = get_number_prop(cx, cookie, "expires")? {
        params.insert("expires".into(), serde_json::json!(expires));
    }
    Ok(())
}

/// Convert a CDP cookie JSON value to a JS object, carrying every field
/// `setCookie` can store back — `expires` / `sameSite` / `priority` used to be
/// dropped here, so a round-trip quietly rewrote the cookie.
fn cookie_to_js<'a>(cx: ZqCtx<'a>, c: &serde_json::Value) -> Result<ZqValue<'a>, ZqError> {
    let obj = ZqValue::new_object(cx)?;
    for key in ["name", "value", "domain", "path"] {
        if let Some(s) = c.get(key).and_then(|v| v.as_str()) {
            obj.set_property_str(cx, key, ZqValue::new_string(cx, s)?)?;
        }
    }
    for key in ["httpOnly", "secure", "session"] {
        if let Some(b) = c.get(key).and_then(|v| v.as_bool()) {
            obj.set_property_str(cx, key, ZqValue::bool(b))?;
        }
    }
    if let Some(s) = c.get("sameSite").and_then(|v| v.as_str()) {
        obj.set_property_str(cx, "sameSite", ZqValue::new_string(cx, s)?)?;
    }
    for key in ["expires", "priority", "size", "sourcePort", "sourceScheme"] {
        // `Network.Cookie.expires` is `-1` for a session cookie; surfacing that
        // would make an omitted expiry look like 1970.
        if key == "expires" && c.get(key).and_then(|v| v.as_f64()).is_some_and(|e| e < 0.0) {
            continue;
        }
        if let Some(n) = c.get(key).and_then(|v| v.as_f64()) {
            obj.set_property_str(cx, key, ZqValue::number(n))?;
        }
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
            // A cookie that cannot be built is skipped rather than failing the
            // whole listing.
            ZqValue::array_from_iter(cx, cookies.iter().filter_map(|c| cookie_to_js(cx, c).ok()))
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

    // The flag/attribute fields `Network.setCookie` accepts but a name/value
    // pair does not carry: dropping them silently stored a different cookie.
    let mut params = serde_json::Map::new();
    cookie_fields_to_params(cx, &cookie, &mut params)?;

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
    let ua = args::arg_string(cx, &cc, 0, "userAgent")?;
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
    // Require an object whose values are all strings. A non-object used to
    // become `headers: {}` (silently clearing every header), and a failed
    // `to_json` became `headers: null`, which Chrome rejects with "not an
    // object".
    let obj = cc
        .arg(0)
        .as_object()
        .ok_or_else(|| cx.throw_type("`headers` must be an object"))?;
    let headers = args::string_map(cx, &obj, "headers")?;
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
    let offline = args::arg_bool(cx, &cc, 0, "enabled")?;
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
    let disabled = args::arg_bool(cx, &cc, 0, "disabled")?;
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
    let arr = cc
        .arg(0)
        .as_array(cx)?
        .ok_or_else(|| cx.throw_type("`cookies` must be an array"))?;
    let len = arr.length(cx)?;
    let mut cookies = Vec::with_capacity(len);
    for i in 0..len {
        let item = arr.get_index(cx, i as u32)?;
        let obj = item
            .as_object()
            .ok_or_else(|| cx.throw_type(&format!("`cookies[{i}]` must be an object")))?;
        // Same field set as `setCookie`, so a batch read back from
        // `getCookies()` keeps `sameSite` / `expires` / `httpOnly`.
        let mut c = serde_json::Map::new();
        cookie_fields_to_params(cx, &obj, &mut c)?;
        cookies.push(serde_json::Value::Object(c));
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
    let name = args::arg_string(cx, &cc, 0, "name")?;
    let url = args::opt_arg_string(cx, &cc, 1, "url")?;
    let mut params = serde_json::json!({ "name": name });
    if let Some(url) = url {
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
    let request_id = args::arg_string(cx, &cc, 0, "requestId")?;
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
            // Subscribe before `Network.enable`, so an event emitted while the
            // domain is switching on cannot be dropped.
            let mut event_rx = client.subscribe_events();
            cdp_network::enable_network(&client, &session_id).await?;

            let session_cap = session_id.clone();
            let recorder = network.clone();
            // No poll interval and no stop flag: the loop is woken by the event
            // stream itself, and it ends when the stream does. The old shape
            // woke every 200ms for the whole capture, and after `Closed` it
            // spun that timer at full speed until someone called
            // `stopNetworkCapture`.
            let handle = helpers::spawn(async move {
                loop {
                    match event_rx.recv().await {
                        Ok(evt) if evt.shutdown => break,
                        Ok(evt)
                            if evt.session_id.as_deref() == Some(&session_cap)
                                && evt.method.starts_with("Network.") =>
                        {
                            recorder.record(serde_json::json!({
                                "method": evt.method,
                                "params": evt.params,
                            }));
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("CDP network-capture listener lagged by {n} events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Aborts the task a previous `startNetworkCapture` left running.
            network.start_capture(handle);
            Ok(())
        },
        |_cx, ()| Ok(ZqValue::undefined()),
    )
}

/// `page.stopNetworkCapture()` — synchronous.
pub(crate) fn js_page_stop_network_capture<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    get_page_payload(cx, cc.this())?.network.stop_capture();
    Ok(ZqValue::undefined())
}

/// `page.getCapturedRequests()` — synchronous.
pub(crate) fn js_page_get_captured_requests<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let requests = get_page_payload(cx, cc.this())?.network.snapshot();
    ZqValue::from_json(cx, &serde_json::Value::Array(requests))
}

/// `page.clearCapturedRequests()` — synchronous.
pub(crate) fn js_page_clear_captured_requests<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    get_page_payload(cx, cc.this())?.network.clear();
    Ok(ZqValue::undefined())
}

/// `page.getRequestPostData(requestId)`
pub(crate) fn js_page_get_request_post_data<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let request_id = args::arg_string(cx, &cc, 0, "requestId")?;
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
