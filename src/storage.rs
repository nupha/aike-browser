// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — Storage domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{
    class_page::{get_page_client, get_page_session, send_page_void_cmd},
    helpers::promise,
};

/// `page.clearData(origin, storageTypes?)`
pub(crate) fn js_page_clear_data<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let origin = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let types = if cc.len() > 1 {
        cc.arg(1)
            .coerce_string(cx)
            .unwrap_or_else(|_| "all".to_string())
    } else {
        "all".to_string()
    };
    send_page_void_cmd(
        cx,
        &cc,
        "Storage.clearDataForOrigin",
        Some(serde_json::json!({ "origin": origin, "storageTypes": types })),
    )
}

/// `page.getStorageKeyForFrame(frameId)`
pub(crate) fn js_page_get_storage_key_for_frame<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let frame_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    promise(
        cx,
        async move {
            let rx = client.send(
                "Storage.getStorageKeyForFrame",
                Some(serde_json::json!({ "frameId": frame_id })),
                Some(&session_id),
            );
            let val = rx.await.unwrap_or_else(|_| Err("channel closed".into()))?;
            Ok(val
                .get("storageKey")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()))
        },
        |cx, key: Option<String>| match key {
            Some(k) => ZqValue::new_string(cx, &k),
            None => Ok(ZqValue::null()),
        },
    )
}
