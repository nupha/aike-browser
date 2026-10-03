// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Tracing domain (flat on `Page`).

use std::time::Duration;

use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    args,
    class_page::{get_page_client, get_page_session},
    io::read_io_stream,
};

/// How long `stopTracing` waits for Chrome to finish writing the stream.
const TRACE_DRAIN: Duration = Duration::from_secs(5);

/// `page.startTracing(categories?)`
pub(crate) fn js_page_start_tracing<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let categories = args::opt_arg_string(cx, &cc, 0, "categories")?.unwrap_or_default();
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_void(async move {
        // No `options`: it is a free-form trace-config string and the value
        // that used to be sent ("simplifyNames") is not one Chrome recognises,
        // so it only looked like a setting.
        let mut params = serde_json::json!({
            "bufferUsageReportingInterval": 0,
            "transferMode": "ReturnAsStream",
            "streamCompression": "none",
        });
        if !categories.is_empty() {
            params["categories"] = serde_json::Value::String(categories);
        }
        client
            .send("Tracing.start", Some(params), Some(&session_id))
            .await?;
        Ok(())
    })
}

/// `page.stopTracing()`
pub(crate) fn js_page_stop_tracing<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_string(async move {
        // Subscribe *before* ending the trace: `Tracing.tracingComplete` can
        // arrive immediately, and a subscriber created afterwards waited out
        // the full timeout and then returned an empty string.
        let mut event_rx = client.subscribe_events();
        let session_cap = session_id.clone();
        client.send("Tracing.end", None, Some(&session_id)).await?;

        let mut stream_handle: Option<String> = None;
        let mut data_loss = false;
        let wait = tokio::time::sleep(TRACE_DRAIN);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    match event {
                        Ok(evt) if evt.shutdown => {
                            return Err("CDP connection closed while waiting for the trace".to_string());
                        }
                        Ok(evt)
                            if evt.method == "Tracing.tracingComplete"
                                && evt.session_id.as_deref() == Some(&session_cap) =>
                        {
                            stream_handle = evt
                                .params
                                .get("stream")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            // A dropped trace is not a silent empty result.
                            data_loss = evt
                                .params
                                .get("dataLossOccurred")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            break;
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            log::warn!("CDP tracing listener lagged by {n} events");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = &mut wait => break,
            }
        }

        let Some(handle) = stream_handle else {
            return Err(
                "Tracing.end produced no trace: Chrome sent no tracingComplete event".to_string(),
            );
        };
        if data_loss {
            log::warn!("stopTracing: Chrome reported dataLossOccurred; the trace is incomplete");
        }
        read_io_stream(&client, &session_id, &handle).await
    })
}
