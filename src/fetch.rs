// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — Fetch domain (request interception, flat on `Page`).

use base64::Engine as _;
use zuqe_zapi::*;

use crate::{
    cdp_network,
    class_page::{get_number_prop, get_page_client, get_page_session, get_str_prop, send_page_void_cmd},
    helpers,
    with_state,
};

/// `page.setRequestInterception(enabled)`
pub(crate) fn js_page_set_request_interception<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;

    if enabled {
        let tx = cx.spawn_channel_job("fetch", move |cx, msg: serde_json::Value| {
            let sess = msg
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let params = msg
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let cb = sess
                .as_ref()
                .and_then(|s| with_state(cx, |st| st.fetch_callbacks.get(s).copied()));

            if let (Some(cb), Some(_sess)) = (cb, sess)
                && let Ok(obj) = ZqValue::from_json(cx, &params)
            {
                // A throwing event callback cannot propagate: clear + log.
                if ZqValue::from(cb).call(cx, ZqValue::undefined(), &[obj]).is_err() {
                    let exc = cx.take_exception();
                    #[cfg(debug_assertions)]
                    helpers::print_exc(cx, exc);
                }
            }
            ZqChannelPoll::Continue
        });

        let client_ev = client.clone();
        let session_ev = session_id.clone();
        helpers::spawn(async move {
            let _ = client_ev
                .send(
                    "Fetch.enable",
                    Some(serde_json::json!({ "patterns": [{ "urlPattern": "*" }] })),
                    Some(&session_ev),
                )
                .await;
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
                    Err(_) => break,
                }
            }
        });

        // Resolve once Fetch.enable has been issued.
        send_page_void_cmd(
            cx,
            &cc,
            "Fetch.enable",
            Some(serde_json::json!({ "patterns": [{ "urlPattern": "*" }] })),
        )
    } else {
        with_state(cx, |st| {
            st.fetch_callbacks.remove(&session_id);
        });
        send_page_void_cmd(cx, &cc, "Fetch.disable", None)
    }
}

/// `page.continueRequest(reqId, opts?)`
pub(crate) fn js_page_continue_request<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let req_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let _ = get_page_client(cx, cc.this())?;

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
        if let Ok(Some(h)) = obj.get_object_str(cx, "headers") {
            params["headers"] = net_obj_to_json(cx, &h);
        }
        if let Some(p) = get_str_prop(cx, &obj, "postData") {
            params["postData"] = serde_json::Value::String(p);
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
    let reason = if cc.len() > 1 {
        if let Some(obj) = cc.arg(1).as_object() {
            get_str_prop(cx, &obj, "reason").unwrap_or_else(|| "Failed".to_string())
        } else {
            cc.arg(1)
                .coerce_string(cx)
                .unwrap_or_else(|_| "Failed".to_string())
        }
    } else {
        "Failed".to_string()
    };
    let _ = get_page_client(cx, cc.this())?;
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
    let _ = get_page_client(cx, cc.this())?;

    let mut params = serde_json::json!({ "requestId": req_id });
    if cc.len() > 1
        && let Some(obj) = cc.arg(1).as_object()
    {
        if let Some(code) = get_number_prop(cx, &obj, "status") {
            params["responseCode"] = serde_json::json!(code as i32);
        }
        if let Ok(Some(h)) = obj.get_object_str(cx, "headers") {
            params["responseHeaders"] = net_obj_to_json(cx, &h);
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
    send_page_void_cmd(cx, &cc, "Fetch.fulfillRequest", Some(params))
}

/// `page.onRequestPaused(callback)`
pub(crate) fn js_page_on_request_paused<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let session_id = get_page_session(cx, cc.this())?;
    let callback = cc.arg(0).raw();
    with_state(cx, |st| {
        st.fetch_callbacks.insert(session_id, callback);
    });
    Ok(ZqValue::undefined())
}

/// Shallow object → JSON map (via the engine's `JSON.stringify`).
fn net_obj_to_json(cx: ZqCtx, obj: &ZqValue) -> serde_json::Value {
    obj.to_json(cx)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
}
