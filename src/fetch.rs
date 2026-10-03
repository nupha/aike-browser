// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Fetch domain (request interception, flat on `Page`).

use base64::Engine as _;
use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    cdp::CdpClient,
    cdp_network,
    class_page::{
        get_number_prop, get_page_client, get_page_session, get_str_prop, send_page_void_cmd,
    },
    helpers, require_callback, with_state,
};

/// `Network.ErrorReason` values `Fetch.failRequest` accepts.
const ERROR_REASONS: &[&str] = &[
    "Failed",
    "Aborted",
    "TimedOut",
    "AccessDenied",
    "ConnectionClosed",
    "ConnectionReset",
    "ConnectionRefused",
    "ConnectionAborted",
    "ConnectionFailed",
    "NameNotResolved",
    "InternetDisconnected",
    "AddressUnreachable",
    "BlockedByClient",
    "BlockedByResponse",
];

/// Interception patterns: every request, on every frame.
fn all_patterns() -> serde_json::Value {
    serde_json::json!({ "patterns": [{ "urlPattern": "*" }] })
}

/// Convert a JS `Record<string,string>` into the `Fetch.HeaderEntry[]` the
/// protocol requires (an array of `{name, value}`, not an object).
fn headers_to_json(cx: ZqCtx, obj: &ZqValue) -> Result<serde_json::Value, ZqError> {
    let Some(serde_json::Value::Object(map)) = obj.to_json(cx) else {
        return Ok(serde_json::Value::Array(Vec::new()));
    };
    Ok(serde_json::Value::Array(
        map.into_iter()
            .map(|(name, value)| {
                serde_json::json!({
                    "name": name,
                    "value": match value {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    },
                })
            })
            .collect(),
    ))
}

/// `page.setRequestInterception(enabled)`
pub(crate) fn js_page_set_request_interception<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;

    if enabled {
        let resume_client = client.clone();
        let tx = cx.spawn_event_job("fetch", move |cx, msg: serde_json::Value| {
            let sess = msg
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let params = msg
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let cb = sess
                .as_ref()
                .and_then(|s| with_state(cx, |st| st.fetch_callbacks.get(s).copied()));

            if let (Some(cb), Some(_)) = (cb, request_id.clone())
                && let Ok(obj) = ZqValue::from_json(cx, &params)
            {
                cx.call_event_handler(cb, ZqValue::undefined(), &[obj], "fetch");
            }
            // A paused request stays paused until somebody continues, aborts or
            // fulfills it — a missing callback (or one that threw, or one that
            // ignored the event) would hang the page forever.
            if let (Some(sess), Some(request_id)) = (sess, request_id)
                && with_state(cx, |st| st.claim_fetch(&sess, &request_id))
            {
                resume(&resume_client, &sess, &request_id);
            }
        });

        let client_ev = client.clone();
        let session_ev = session_id.clone();
        helpers::spawn(async move {
            // Enabling Network up-front ensures request bodies are tracked, so
            // `Network.getRequestPostData` can read them later.
            let _ = cdp_network::enable_network(&client_ev, &session_ev).await;
            let mut event_rx = client_ev.subscribe_events();
            loop {
                match event_rx.recv().await {
                    Ok(evt)
                        if evt.method == "Fetch.requestPaused"
                            && evt.session_id.as_deref() == Some(&session_ev) =>
                    {
                        let msg = serde_json::json!({
                            "sessionId": evt.session_id,
                            "params": evt.params,
                        });
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    // Lagging only means this listener missed events, not that
                    // the stream is dead: dropping it would leave every skipped
                    // request paused forever.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("CDP request-interception listener lagged by {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Resolve once Fetch.enable has been issued.
        send_page_void_cmd(cx, &cc, "Fetch.enable", Some(all_patterns()))
    } else {
        with_state(cx, |st| {
            st.fetch_callbacks.remove(&session_id);
        });
        send_page_void_cmd(cx, &cc, "Fetch.disable", None)
    }
}

/// Tell the interception safety net that JS has taken responsibility for
/// `request_id`, so it does not resume an already-resolved request.
fn mark_handled(cx: ZqCtx, session_id: &str, request_id: &str) {
    with_state(cx, |st| st.mark_fetch_handled(session_id, request_id));
}

/// Resume a paused request the JS side did not handle.
fn resume(client: &CdpClient, session_id: &str, request_id: &str) {
    client.send_detached(
        "Fetch.continueRequest",
        Some(serde_json::json!({ "requestId": request_id })),
        Some(session_id),
    );
}

/// `page.continueRequest(reqId, opts?)`
pub(crate) fn js_page_continue_request<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let req_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let session_id = get_page_session(cx, cc.this())?;
    mark_handled(cx, &session_id, &req_id);

    let mut params = serde_json::json!({ "requestId": req_id });
    if cc.len() > 1
        && let Some(obj) = cc.arg(1).as_object()
    {
        if let Some(u) = get_str_prop(cx, &obj, "url") {
            params["url"] = serde_json::Value::String(u);
        }
        if let Some(m) = get_str_prop(cx, &obj, "method") {
            params["method"] = serde_json::Value::String(m);
        }
        if let Some(h) = crate::class_page::get_object_prop(cx, &obj, "headers") {
            params["headers"] = headers_to_json(cx, &h)?;
        }
        if let Some(p) = get_str_prop(cx, &obj, "postData") {
            // `Fetch.Request.postData` is a binary::String.
            params["postData"] = serde_json::Value::String(
                base64::engine::general_purpose::STANDARD.encode(p.as_bytes()),
            );
        }
    }
    send_page_void_cmd(cx, &cc, "Fetch.continueRequest", Some(params))
}

/// `page.abortRequest(reqId, reason?)`
pub(crate) fn js_page_abort_request<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let req_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    // An unknown reason is a protocol error, and the request would be left
    // paused forever, so reject before sending. Both the bare-string and the
    // `{ reason }` spellings are accepted.
    let reason = match cc.arg(1).as_object() {
        Some(opts) => opts
            .get_enum(cx, "reason", ERROR_REASONS)?
            .unwrap_or_else(|| "Failed".to_string()),
        None if cc.len() > 1 => cc.arg_enum(cx, 1, ERROR_REASONS, "reason")?,
        None => "Failed".to_string(),
    };
    let session_id = get_page_session(cx, cc.this())?;
    mark_handled(cx, &session_id, &req_id);
    send_page_void_cmd(
        cx,
        &cc,
        "Fetch.failRequest",
        Some(serde_json::json!({ "requestId": req_id, "errorReason": reason })),
    )
}

/// `page.respondRequest(reqId, opts?)`
pub(crate) fn js_page_respond_request<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let req_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let session_id = get_page_session(cx, cc.this())?;
    mark_handled(cx, &session_id, &req_id);

    let mut params = serde_json::json!({ "requestId": req_id });
    if cc.len() > 1
        && let Some(obj) = cc.arg(1).as_object()
    {
        if let Some(code) = get_number_prop(cx, &obj, "status") {
            params["responseCode"] = serde_json::json!(code as i32);
        }
        if let Some(h) = crate::class_page::get_object_prop(cx, &obj, "headers") {
            params["responseHeaders"] = headers_to_json(cx, &h)?;
        }
        if let Some(body) = get_str_prop(cx, &obj, "body") {
            let b64 = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
            params["body"] = serde_json::Value::String(b64);
        }
        if let Some(ct) = get_str_prop(cx, &obj, "contentType")
            && params.get("responseHeaders").is_none()
        {
            params["responseHeaders"] =
                serde_json::json!([{ "name": "Content-Type", "value": ct }]);
        }
    }
    // `Fetch.fulfillRequest.responseCode` is required; a paused request would
    // otherwise stay paused forever.
    if params.get("responseCode").is_none() {
        params["responseCode"] = serde_json::json!(200);
    }
    send_page_void_cmd(cx, &cc, "Fetch.fulfillRequest", Some(params))
}

/// `page.onRequestPaused(callback)`
pub(crate) fn js_page_on_request_paused<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let session_id = get_page_session(cx, cc.this())?;
    let callback = require_callback(cx, cc.arg(0))?;
    with_state(cx, |st| {
        st.fetch_callbacks.insert(session_id, callback);
    });
    Ok(ZqValue::undefined())
}
