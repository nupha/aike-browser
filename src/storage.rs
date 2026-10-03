// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Storage domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{
    args,
    class_page::{get_page_client, get_page_session, send_page_void_cmd},
};

/// `page.clearData(origin, storageTypes?)`
pub(crate) fn js_page_clear_data<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let origin = args::arg_string(cx, &cc, 0, "origin")?;
    let types =
        args::opt_arg_string(cx, &cc, 1, "storageTypes")?.unwrap_or_else(|| "all".to_string());
    send_page_void_cmd(
        cx,
        &cc,
        "Storage.clearDataForOrigin",
        Some(serde_json::json!({ "origin": origin, "storageTypes": types })),
    )
}

/// `page.getStorageKeyForFrame(frameId)`
///
/// `Storage.getStorageKey` replaces the deprecated
/// `Storage.getStorageKeyForFrame`. It also makes `frameId` optional, but the
/// no-argument form answers "Frame tree node for given frame not found" on a
/// flat page session, so it is only sent when the caller supplied one.
pub(crate) fn js_page_get_storage_key_for_frame<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let frame_id = args::arg_string(cx, &cc, 0, "frameId")?;
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_result(
        async move {
            let val = client
                .send(
                    "Storage.getStorageKey",
                    Some(serde_json::json!({ "frameId": frame_id })),
                    Some(&session_id),
                )
                .await?;
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
