// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Fetch domain (request interception, flat on `Page`).

use base64::Engine as _;
use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    Listener, args,
    cdp::CdpClient,
    cdp_network,
    class_page::{
        get_i64_prop, get_object_prop, get_page_client, get_page_session, get_str_prop,
        send_page_void_cmd,
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
    let map = args::string_map(cx, obj, "headers")?;
    Ok(serde_json::Value::Array(
        map.into_iter()
            .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
            .collect(),
    ))
}

/// `page.setRequestInterception(enabled)`
pub(crate) fn js_page_set_request_interception<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = args::arg_bool(cx, &cc, 0, "enabled")?;
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;

    if enabled {
        // Subscribe *before* `Fetch.enable` is sent. A receiver created only
        // inside the forwarding task races the enable: a request already in
        // flight can be paused and its `Fetch.requestPaused` broadcast before
        // the task is scheduled, and a broadcast with no live receiver drops
        // the event — that request then stays paused and its page never loads.
        let event_rx = client.subscribe_events();
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
        let task = helpers::spawn(async move {
            // `event_rx` was subscribed synchronously above, before
            // `Fetch.enable`, so nothing paused in between can be missed.
            let mut event_rx = event_rx;
            // Enabling Network up-front ensures request bodies are tracked, so
            // `Network.getRequestPostData` can read them later.
            let _ = cdp_network::enable_network(&client_ev, &session_ev).await;
            loop {
                match event_rx.recv().await {
                    Ok(evt) if evt.shutdown => break,
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

        // Re-enabling replaces the forwarding task instead of stacking a second
        // one, which would resume each paused request from two event jobs.
        with_state(cx, |st| {
            st.set_listener_task(Listener::Fetch, &session_id, task)
        });

        // Resolve once Fetch.enable has been issued.
        send_page_void_cmd(cx, &cc, "Fetch.enable", Some(all_patterns()))
    } else {
        with_state(cx, |st| {
            st.fetch_callbacks.remove(&session_id);
            // Stop forwarding: the task outlived `setRequestInterception(false)`
            // before, so every on/off cycle leaked one.
            st.clear_listener_task(Listener::Fetch, &session_id);
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
    let req_id = args::arg_string(cx, &cc, 0, "interceptionId")?;
    let session_id = get_page_session(cx, cc.this())?;
    mark_handled(cx, &session_id, &req_id);

    let mut params = serde_json::json!({ "requestId": req_id });
    if cc.len() > 1
        && let Some(obj) = cc.arg(1).as_object()
    {
        if let Some(u) = get_str_prop(cx, &obj, "url")? {
            params["url"] = serde_json::Value::String(u);
        }
        if let Some(m) = get_str_prop(cx, &obj, "method")? {
            params["method"] = serde_json::Value::String(m);
        }
        if let Some(h) = get_object_prop(cx, &obj, "headers")? {
            params["headers"] = headers_to_json(cx, &h)?;
        }
        if let Some(p) = get_str_prop(cx, &obj, "postData")? {
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
    let req_id = args::arg_string(cx, &cc, 0, "interceptionId")?;
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
    let req_id = args::arg_string(cx, &cc, 0, "interceptionId")?;
    let session_id = get_page_session(cx, cc.this())?;
    mark_handled(cx, &session_id, &req_id);

    let mut params = serde_json::json!({ "requestId": req_id });
    if cc.len() > 1
        && let Some(obj) = cc.arg(1).as_object()
    {
        if let Some(code) = get_i64_prop(cx, &obj, "status")? {
            params["responseCode"] =
                serde_json::json!(args::in_range(cx, code, "status", 100, 599)?);
        }
        if let Some(h) = get_object_prop(cx, &obj, "headers")? {
            params["responseHeaders"] = headers_to_json(cx, &h)?;
        }
        if let Some(body) = get_str_prop(cx, &obj, "body")? {
            let b64 = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
            params["body"] = serde_json::Value::String(b64);
        }
        if let Some(ct) = get_str_prop(cx, &obj, "contentType")?
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
