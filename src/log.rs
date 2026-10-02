// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — Log domain methods (flat on `Page`).

use std::sync::{LazyLock, Mutex};

use zuqe_zapi::*;

use crate::{
    class_page::{get_page_client, get_page_session, send_page_void_cmd},
    helpers,
    with_state,
};

/// Per-session ring buffer of console/log entries.
static LOG_BUFFER: LazyLock<Mutex<std::collections::HashMap<String, Vec<serde_json::Value>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// `page.onLog(callback)`
pub(crate) fn js_page_on_log<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;

    // Store the callback keyed by session in the runtime's traced state so the
    // GC keeps the JS function alive while it lives in this native-side map.
    let callback = cc.arg(0).raw();
    with_state(cx, |st| {
        st.log_console_callbacks.insert(session_id.clone(), callback);
    });

    let _ = send_page_void_cmd(cx, &cc, "Log.enable", None);

    let tx = cx.spawn_channel_job("log", move |cx, msg: serde_json::Value| {
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
            && ZqValue::from(cb).call(cx, ZqValue::undefined(), &[obj]).is_err()
        {
            let exc = cx.take_exception();
            #[cfg(debug_assertions)]
            helpers::print_exc(cx, exc);
        }
        ZqChannelPoll::Continue
    });

    let client_ev = client.clone();
    let session_ev = session_id.clone();
    helpers::spawn(async move {
        let mut event_rx = client_ev.subscribe_events();
        loop {
            match event_rx.recv().await {
                Ok(evt)
                    if (evt.method == "Log.entryAdded"
                        || evt.method == "Runtime.consoleAPICalled")
                        && evt.session_id.as_deref() == Some(&session_ev) =>
                {
                    if let Ok(mut guard) = LOG_BUFFER.lock() {
                        guard.entry(session_ev.clone()).or_default().push(evt.params.clone());
                    }
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

    Ok(ZqValue::undefined())
}

/// `page.getConsoleMessages(clear?)`
pub(crate) fn js_page_get_console_messages<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let clear = cc.len() > 0 && cc.arg_bool(cx, 0).unwrap_or(false);
    let session_id = get_page_session(cx, cc.this())?;

    let mut messages: Vec<serde_json::Value> = Vec::new();
    if let Ok(mut guard) = LOG_BUFFER.lock()
        && let Some(buf) = guard.get_mut(&session_id)
    {
        messages = std::mem::take(buf);
    }

    if !clear && let Ok(mut guard) = LOG_BUFFER.lock() {
        guard.insert(session_id, messages.clone());
    }

    ZqValue::from_json(cx, &serde_json::Value::Array(messages))
}
