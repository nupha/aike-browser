// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Log domain methods (flat on `Page`).

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    Listener, args, class_page::send_page_void_cmd, helpers, require_callback, with_state,
};

/// Buffered console/log entries of one page.
///
/// Owned by the page payload, so it dies with the page (a process-global map
/// keyed by session id outlived `page.close()` and grew without bound).
pub(crate) type LogBuffer = Arc<Mutex<VecDeque<serde_json::Value>>>;

/// Entries kept per page. A long-lived capture of `console.log` in a loop must
/// not turn into an unbounded allocation; the oldest entries are dropped.
pub(crate) const LOG_BUFFER_CAP: usize = 1000;

/// Append an entry, dropping the oldest once [`LOG_BUFFER_CAP`] is reached.
pub(crate) fn push_log(buffer: &LogBuffer, entry: serde_json::Value) {
    if let Ok(mut buf) = buffer.lock() {
        buf.push_back(entry);
        while buf.len() > LOG_BUFFER_CAP {
            buf.pop_front();
        }
    }
}

/// Render a `Log.entryAdded` / `Runtime.consoleAPICalled` payload as one
/// `ConsoleMessage`.
///
/// The two events have unrelated shapes (`{entry:{text,level,source}}` versus
/// `{type,args:[{value}]}`), so passing either through raw meant a caller
/// reading `msg.text` saw it for one and not the other.
fn to_console_message(params: &serde_json::Value) -> serde_json::Value {
    let text = params
        .get("entry")
        .and_then(|e| e.get("text"))
        .or_else(|| params.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // `Runtime.consoleAPICalled` carries the message as pre-rendered
            // `RemoteObject`s; `description` is what a browser devtools shows.
            params
                .get("args")
                .and_then(|v| v.as_array())
                .map(|args| args.iter().map(arg_text).collect::<Vec<_>>().join(" "))
                .unwrap_or_default()
        });
    serde_json::json!({
        "source": params
            .get("entry")
            .and_then(|e| e.get("source"))
            .and_then(|v| v.as_str())
            .or_else(|| params.get("type").and_then(|v| v.as_str()))
            .unwrap_or(""),
        "level": params
            .get("entry")
            .and_then(|e| e.get("level"))
            .and_then(|v| v.as_str())
            .or_else(|| params.get("type").and_then(|v| v.as_str()))
            .unwrap_or("log"),
        "text": text,
        "params": params,
    })
}

fn json_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// One `Runtime.consoleAPICalled` argument rendered as text. `description` is
/// what devtools shows (`"a"` with quotes, `"3"` for a number); `value` is the
/// fallback for arguments that have neither.
fn arg_text(a: &serde_json::Value) -> String {
    a.get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| a.get("value").map(json_to_text))
        .unwrap_or_default()
}

/// `page.onLog(callback)`
pub(crate) fn js_page_on_log<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let payload = crate::class_page::get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    let buffer = payload.log_buffer.clone();

    // Store the callback keyed by session in the runtime's traced state so the
    // GC keeps the JS function alive while it lives in this native-side map.
    let callback = require_callback(cx, cc.arg(0))?;
    with_state(cx, |st| {
        st.log_console_callbacks
            .insert(session_id.clone(), callback);
    });

    // Subscribe before enabling: an event emitted between `Log.enable` /
    // `Runtime.enable` and a receiver created later would be dropped.
    let event_rx = client.subscribe_events();
    // Both domains feed this listener, and `Runtime` has to be on for
    // `console.log` to show up at all — enabling only `Log` silently delivered
    // network and JavaScript errors only.
    let _ = send_page_void_cmd(cx, &cc, "Log.enable", None);
    let _ = send_page_void_cmd(cx, &cc, "Runtime.enable", None);

    let tx = cx.spawn_event_job("log", move |cx, msg: serde_json::Value| {
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
            .and_then(|s| with_state(cx, |st| st.log_console_callbacks.get(s).copied()));
        if let (Some(cb), Some(_sess)) = (cb, sess)
            && let Ok(obj) = ZqValue::from_json(cx, &params)
        {
            cx.call_event_handler(cb, ZqValue::undefined(), &[obj], "log");
        }
    });

    let session_ev = session_id.clone();
    let task = helpers::spawn(async move {
        // `event_rx` was subscribed synchronously before the enables above.
        let mut event_rx = event_rx;
        loop {
            match event_rx.recv().await {
                Ok(evt) if evt.shutdown => break,
                Ok(evt)
                    if (evt.method == "Log.entryAdded"
                        || evt.method == "Runtime.consoleAPICalled")
                        && evt.session_id.as_deref() == Some(&session_ev) =>
                {
                    push_log(&buffer, to_console_message(&evt.params));
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
                    log::warn!("CDP log listener lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    // Registering twice replaces the forwarding task instead of stacking a
    // second one, which would deliver every message to both callbacks.
    with_state(cx, |st| {
        st.set_listener_task(Listener::Log, &session_id, task)
    });

    Ok(ZqValue::undefined())
}

/// `page.getConsoleMessages(clear?)` — synchronous (the buffer is a plain
/// in-process ring; there is nothing to await).
pub(crate) fn js_page_get_console_messages<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let clear = args::opt_arg_bool(cx, &cc, 0, "clear")?.unwrap_or(false);
    let buffer = crate::class_page::get_page_payload(cx, cc.this())?
        .log_buffer
        .clone();

    // One lock, no gap: taking the buffer and putting it back left a window in
    // which a freshly arriving entry was appended to the emptied buffer and
    // then overwritten by the re-insertion.
    let messages: Vec<serde_json::Value> = match buffer.lock() {
        Ok(mut guard) if clear => guard.drain(..).collect(),
        Ok(guard) => guard.iter().cloned().collect(),
        Err(_) => Vec::new(),
    };

    ZqValue::from_json(cx, &serde_json::Value::Array(messages))
}
