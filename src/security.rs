// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Browser class — Security domain methods (flat on `Browser`).

use tokio::sync::broadcast;
use zuqe_zapi::*;
use zuqe_zapi_macros::zq_function;

use crate::{
    Listener, class_browser::get_browser_payload, helpers, next_cert_magic, require_callback,
    with_state,
};

/// `browser.setIgnoreHTTPSErrors(bool)`
pub(crate) fn js_browser_set_ignore_https_errors<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
    crate::class_browser::send_browser_void_cmd(
        cx,
        &cc,
        "Security.setIgnoreCertificateErrors",
        Some(serde_json::json!({ "ignore": enabled })),
    )
}

/// `accept(bool)` method installed on a certificate-error event object. Its
/// `trampoline()` is resolved into a JS function via [`ZqValue::new_function`].
///
/// The deprecated `Security.handleCertificateError` took a per-event
/// `continue`/`cancel` action; `Security.setIgnoreCertificateErrors` is its
/// replacement and decides the outcome for every subsequent request.
///
/// `magic` names the browser this `accept` belongs to: a native function cannot
/// capture the connection, and without the tag it would act on whichever
/// browser happened to register last.
#[zq_function(name = "accept")]
pub(crate) fn js_cert_accept<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let accept = cc.arg_bool(cx, 0).unwrap_or(false);
    let client = with_state(cx, |st| st.cert_client(cc.magic()));
    if let Some(client) = client {
        client.send_detached(
            "Security.setIgnoreCertificateErrors",
            Some(serde_json::json!({ "ignore": accept })),
            None,
        );
    }
    Ok(ZqValue::undefined())
}

/// `browser.handleCertificateError(callback)`
pub(crate) fn js_browser_handle_certificate_error<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    let callback = require_callback(cx, cc.arg(0))?;
    let magic = next_cert_magic();
    with_state(cx, |st| {
        st.set_cert_listener(magic, callback, client.clone())
    });

    let tx = cx.spawn_event_job("certificateError", move |cx, params: serde_json::Value| {
        let cb = with_state(cx, |st| st.cert_listeners.get(&magic).map(|l| l.callback));
        if let Some(cb) = cb
            && let Ok(obj) = ZqValue::new_object(cx)
        {
            let event_id = params.get("eventId").and_then(|v| v.as_i64()).unwrap_or(0);
            let _ = obj.set_str(cx, "eventId", ZqValue::int32(event_id as i32));
            if let Some(et) = params.get("errorType").and_then(|v| v.as_str())
                && let Ok(s) = ZqValue::new_string(cx, et)
            {
                let _ = obj.set_str(cx, "errorType", s);
            }
            if let Some(u) = params.get("requestURL").and_then(|v| v.as_str())
                && let Ok(s) = ZqValue::new_string(cx, u)
            {
                let _ = obj.set_str(cx, "requestURL", s);
            }
            // Attach `accept(bool)` built from the `#[zq_function]` body, tagged
            // with this browser's connection.
            if let Ok(f) = ZqValue::new_function(cx, js_cert_accept::trampoline(), "accept", magic)
            {
                let _ = obj.set_str(cx, "accept", f);
            }
            cx.call_event_handler(cb, ZqValue::undefined(), &[obj], "certificateError");
        }
    });

    let task = helpers::spawn(async move {
        let mut event_rx = client.subscribe_events();
        // Subscribe first: `Security.enable` starts the event flow, so an error
        // reported between the two would otherwise be lost.
        if let Err(e) = client.send("Security.enable", None, None).await {
            // Not swallowed: with the domain off, no event ever arrives and the
            // callback could not fire, so the caller has to learn that here.
            return log::error!(
                "handleCertificateError: Security.enable failed ({e}); certificate errors will not be reported"
            );
        }
        loop {
            match event_rx.recv().await {
                Ok(evt) if evt.shutdown => break,
                Ok(evt) if evt.method == "Security.certificateError" => {
                    if tx.send(evt.params.clone()).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("CDP certificate listener lagged by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Re-registering stops the previous forwarding task instead of leaving it
    // to deliver every event a second time.
    with_state(cx, |st| {
        st.set_listener_task(Listener::Cert, &magic.to_string(), task)
    });

    Ok(ZqValue::undefined())
}
