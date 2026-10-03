// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — IO domain (flat on `Page`).

use base64::Engine as _;
use zuqe_zapi::*;

use crate::{cdp::CdpClient, class_page::get_page_payload};

/// Read a CDP IO stream (`handle`) to completion and return its UTF-8 text.
///
/// Always closes the handle, including on the error paths: an `IO.read` handle
/// that is never closed stays pinned in the browser for the life of the page.
pub(crate) async fn read_io_stream(
    client: &CdpClient,
    session_id: &str,
    handle: &str,
) -> Result<String, String> {
    let mut offset: u64 = 0;
    let chunk: u64 = 1 << 16; // 64 KiB per read
    let mut buf: Vec<u8> = Vec::new();

    let result = loop {
        let resp = match client
            .send(
                "IO.read",
                Some(serde_json::json!({ "handle": handle, "offset": offset, "size": chunk })),
                Some(session_id),
            )
            .await
        {
            Ok(resp) => resp,
            Err(e) => break Err(e),
        };

        let data = resp.get("data").and_then(|v| v.as_str()).unwrap_or("");
        let base64 = resp
            .get("base64Encoded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let written = if base64 {
            // A decode failure used to yield zero bytes, so `offset` never
            // advanced and the loop re-read the same offset forever — a request
            // storm against the browser.
            match base64::engine::general_purpose::STANDARD.decode(data) {
                Ok(bytes) => {
                    buf.extend_from_slice(&bytes);
                    bytes.len()
                }
                Err(e) => break Err(format!("IO.read: bad base64 chunk: {e}")),
            }
        } else {
            let bytes = data.as_bytes();
            buf.extend_from_slice(bytes);
            bytes.len()
        };

        if resp.get("eof").and_then(|v| v.as_bool()).unwrap_or(false) {
            break Ok(());
        }
        if written == 0 {
            // No progress and no eof: the stream cannot advance any further.
            break Err("IO.read: stream produced no data and signalled no eof".to_string());
        }
        offset += written as u64;
    };

    close(client, session_id, handle).await;
    result.map(|()| String::from_utf8_lossy(&buf).into_owned())
}

/// Release a CDP IO stream handle, ignoring the outcome (the read already
/// succeeded or already failed).
async fn close(client: &CdpClient, session_id: &str, handle: &str) {
    let _ = client
        .send(
            "IO.close",
            Some(serde_json::json!({ "handle": handle })),
            Some(session_id),
        )
        .await;
}

/// `page.readStream(handle)`
pub(crate) fn js_page_read_stream<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let handle = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_string(async move { read_io_stream(&client, &session_id, &handle).await })
}
