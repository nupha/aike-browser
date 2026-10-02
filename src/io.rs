// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — IO domain (flat on `Page`).

use base64::Engine as _;
use zuqe_zapi::*;

use crate::{cdp::CdpClient, class_page::get_page_payload, helpers::promise_string};

/// Read a CDP IO stream (`handle`) to completion and return its UTF-8 text.
pub(crate) async fn read_io_stream(
    client: &CdpClient,
    session_id: &str,
    handle: &str,
) -> Result<String, String> {
    let mut offset: u64 = 0;
    let chunk: u64 = 1 << 16; // 64 KiB per read
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let rx = client.send(
            "IO.read",
            Some(serde_json::json!({ "handle": handle, "offset": offset, "size": chunk })),
            Some(session_id),
        );
        let resp = rx.await.unwrap_or_else(|_| Err("channel closed".into()))?;

        let data = resp.get("data").and_then(|v| v.as_str()).unwrap_or("");
        let base64 = resp
            .get("base64Encoded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let written = if base64 {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap_or_default();
            buf.extend_from_slice(&bytes);
            bytes.len()
        } else {
            let bytes = data.as_bytes();
            buf.extend_from_slice(bytes);
            bytes.len()
        };

        let eof = resp.get("eof").and_then(|v| v.as_bool()).unwrap_or(false);
        if eof {
            break;
        }
        offset += written as u64;
    }

    Ok(String::from_utf8_lossy(&buf).into_owned())
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
    promise_string(cx, async move {
        read_io_stream(&client, &session_id, &handle).await
    })
}
