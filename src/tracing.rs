// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Tracing domain (flat on `Page`).

use std::time::Duration;

use tokio::sync::broadcast;
use zuqe_zapi::*;

use crate::{
    class_page::{get_page_client, get_page_session},
    io::read_io_stream,
};

/// `page.startTracing(categories?)`
pub(crate) fn js_page_start_tracing<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let categories = if cc.len() > 0 {
        cc.arg(0).coerce_string(cx).unwrap_or_default()
    } else {
        String::new()
    };
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let mut params = serde_json::json!({
            "bufferUsageReportingInterval": 0,
            "transferMode": "ReturnAsStream",
            "streamCompression": "none",
            "options": "simplifyNames",
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
        let _ = client.send("Tracing.end", None, Some(&session_id)).await;

        let mut event_rx = client.subscribe_events();
        let session_clone = session_id.clone();
        let mut stream_handle: Option<String> = None;
        let wait = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(wait);
        loop {
            tokio::select! {
                event = event_rx.recv() => {
                    match event {
                        Ok(evt)
                            if evt.method == "Tracing.tracingComplete"
                                && evt.session_id.as_deref() == Some(&session_clone) =>
                        {
                            stream_handle = evt
                                .params
                                .get("stream")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            break;
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = &mut wait => break,
            }
        }

        Ok(match stream_handle {
            Some(h) => read_io_stream(&client, &session_id, &h)
                .await
                .unwrap_or_default(),
            None => String::new(),
        })
    })
}
