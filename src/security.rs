// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Browser class — Security domain methods (flat on `Browser`).

use zuqe_zapi::*;
use zuqe_zapi_macros::zq_function;

use crate::{
    class_browser::{get_browser_payload, send_browser_void_cmd},
    helpers,
    with_state,
};

/// `browser.setIgnoreHTTPSErrors(bool)`
pub(crate) fn js_browser_set_ignore_https_errors<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
    send_browser_void_cmd(
        cx,
        &cc,
        "Security.setIgnoreCertificateErrors",
        Some(serde_json::json!({ "ignore": enabled })),
    )
}

/// `accept(bool)` method installed on a certificate-error event object. Its
/// `trampoline()` is resolved into a JS function via [`ZqValue::new_function`].
#[zq_function(name = "accept")]
pub(crate) fn js_cert_accept<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let accept = cc.arg_bool(cx, 0).unwrap_or(false);
    let (event_id, client) = with_state(cx, |st| (st.cert_event_id, st.cert_client.clone()));
    if let (Some(id), Some(client)) = (event_id, client) {
        let action = if accept { "accept" } else { "cancel" };
        client.send(
            "Security.handleCertificateError",
            Some(serde_json::json!({ "eventId": id, "action": action })),
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
    let callback = cc.arg(0).raw();
    with_state(cx, |st| {
        st.cert_callback = Some(callback);
        st.cert_client = Some(client.clone());
    });

    let _ = send_browser_void_cmd(cx, &cc, "Security.enable", None);

    let tx = cx.spawn_channel_job("certificateError", |cx, params: serde_json::Value| {
        let cb = with_state(cx, |st| st.cert_callback);
        if let Some(cb) = cb {
            let event_id = params.get("eventId").and_then(|v| v.as_i64()).unwrap_or(0);
            with_state(cx, |st| st.cert_event_id = Some(event_id));

            if let Ok(obj) = ZqValue::new_object(cx) {
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
                // Attach `accept(bool)` built from the `#[zq_function]` body.
                if let Ok(f) = ZqValue::new_function(
                    cx,
                    js_cert_accept::trampoline(),
                    "accept",
                    0,
                ) {
                    let _ = obj.set_str(cx, "accept", f);
                }
                if ZqValue::from(cb).call(cx, ZqValue::undefined(), &[obj]).is_err() {
                    let exc = cx.take_exception();
                    #[cfg(debug_assertions)]
                    helpers::print_exc(cx, exc);
                }
            }
            with_state(cx, |st| st.cert_event_id = None);
        }
        ZqChannelPoll::Continue
    });

    helpers::spawn(async move {
        let mut event_rx = client.subscribe_events();
        loop {
            match event_rx.recv().await {
                Ok(evt) if evt.method == "Security.certificateError" => {
                    if tx.send(evt.params.clone()).is_err() {
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
