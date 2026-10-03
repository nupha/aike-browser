// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Runtime domain methods (flat on `Page`).
//!
//! `callFunctionOn` / `getProperties` / `evaluateHandle` / `onConsole`.

use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    class_page::{get_page_client, get_page_session},
    helpers::{self, format_exception},
    require_callback, with_state,
};

/// `page.callFunctionOn(objectId, fn, args?)`
pub(crate) fn js_page_call_function_on<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let object_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let fn_src = cc.arg(1).coerce_string(cx).unwrap_or_default();

    let mut call_args = Vec::new();
    if cc.len() > 2
        && let Some(arr) = cc.arg(2).as_array(cx)?
        && let Ok(len) = arr.length(cx)
    {
        for i in 0..len {
            if let Ok(item) = arr.get_index(cx, i as u32) {
                call_args.push(serde_json::json!({ "value": item.to_json(cx).unwrap_or(serde_json::Value::Null) }));
            }
        }
    }

    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let val = client
            .send(
                "Runtime.callFunctionOn",
                Some(serde_json::json!({
                    "objectId": object_id,
                    "functionDeclaration": fn_src,
                    "arguments": call_args,
                    "returnByValue": true,
                    // Without this a returned promise resolves to `null`.
                    "awaitPromise": true,
                })),
                Some(&session_id),
            )
            .await?;
        if let Some(details) = val.get("exceptionDetails") {
            return Err(format_exception(details));
        }
        Ok(val
            .get("result")
            .and_then(|x| x.get("value"))
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    })
}

/// `page.getProperties(objectId)`
pub(crate) fn js_page_get_properties<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let object_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let props = client
            .send(
                "Runtime.getProperties",
                Some(serde_json::json!({ "objectId": object_id, "ownProperties": true })),
                Some(&session_id),
            )
            .await?
            .get("result")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();

        // Build a name -> value map. Primitive values come inline via `value`;
        // object/array values only carry an `objectId`, so resolve them by value
        // with a callFunctionOn round-trip.
        let mut map = serde_json::Map::new();
        for p in &props {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else {
                continue;
            };
            let remote = p.get("value");
            let resolved = if let Some(v) = remote.and_then(|x| x.get("value")) {
                v.clone()
            } else if let Some(oid) = remote
                .and_then(|x| x.get("objectId"))
                .and_then(|x| x.as_str())
            {
                client
                    .send(
                        "Runtime.callFunctionOn",
                        Some(serde_json::json!({
                            "objectId": oid,
                            "functionDeclaration": "function () { return this; }",
                            "returnByValue": true,
                        })),
                        Some(&session_id),
                    )
                    .await
                    .ok()
                    .and_then(|j| j.get("result").and_then(|x| x.get("value")).cloned())
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            };
            map.insert(name.to_string(), resolved);
        }
        Ok(serde_json::Value::Object(map))
    })
}

/// `page.evaluateHandle(expr)`
pub(crate) fn js_page_evaluate_handle<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let expr = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let val = client
            .send(
                "Runtime.evaluate",
                Some(serde_json::json!({
                    "expression": expr,
                    "returnByValue": false,
                })),
                Some(&session_id),
            )
            .await?;
        if let Some(details) = val.get("exceptionDetails") {
            return Err(format_exception(details));
        }
        let result = val.get("result");
        let obj_id = result
            .and_then(|x| x.get("objectId"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let mut handle = serde_json::json!({ "objectId": obj_id });
        // Primitives have no handle; hand the value back so the caller can tell
        // an empty `objectId` from a real one.
        if let Some(v) = result.and_then(|x| x.get("value")) {
            handle["value"] = v.clone();
        }
        Ok(handle)
    })
}

/// `page.onConsole(callback)` — invoke `callback(msg)` for each
/// `Runtime.consoleAPICalled` event.
pub(crate) fn js_page_on_console<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;

    let callback = require_callback(cx, cc.arg(0))?;
    with_state(cx, |st| {
        st.runtime_console_callbacks
            .insert(session_id.clone(), callback);
    });

    let _ = crate::class_page::send_page_void_cmd(cx, &cc, "Runtime.enable", None);

    let tx = cx.spawn_event_job("console", move |cx, msg: serde_json::Value| {
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
            .and_then(|s| with_state(cx, |st| st.runtime_console_callbacks.get(s).copied()));
        if let (Some(cb), Some(_sess)) = (cb, sess)
            && let Ok(obj) = ZqValue::from_json(cx, &params)
        {
            cx.call_event_handler(cb, ZqValue::undefined(), &[obj], "console");
        }
    });

    let client_ev = client.clone();
    let session_ev = session_id.clone();
    helpers::spawn(async move {
        let mut event_rx = client_ev.subscribe_events();
        loop {
            match event_rx.recv().await {
                Ok(evt)
                    if evt.method == "Runtime.consoleAPICalled"
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
                // Lagging only means this listener missed events, not that the
                // stream is dead: dropping the receiver would silently kill it.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("CDP console listener lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    Ok(ZqValue::undefined())
}
