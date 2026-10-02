// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Browser class — Target domain methods (flat on `Browser`).

use zuqe_zapi::*;

use crate::{
    class_browser::get_browser_payload,
    helpers::{promise, promise_json, promise_void},
};

/// `browser.getTargets()`
pub(crate) fn js_browser_get_targets<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    promise_json(cx, async move {
        client
            .send("Target.getTargets", None, None)
            .await
            .unwrap_or_else(|_| Err("channel closed".into()))
    })
}

/// `browser.listPages()`
pub(crate) fn js_browser_list_pages<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    promise_json(cx, async move {
        let v = client
            .send("Target.getTargets", None, None)
            .await
            .unwrap_or_else(|_| Err("channel closed".into()))?;
        let pages = v
            .get("targetInfos")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|t| t.get("type").and_then(|x| x.as_str()) == Some("page"))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(serde_json::Value::Array(pages))
    })
}

/// `browser.newContext()`
pub(crate) fn js_browser_new_context<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    promise(cx, async move {
        let v = client
            .send("Target.createBrowserContext", None, None)
            .await
            .unwrap_or_else(|_| Err("channel closed".into()))?;
        Ok(v.get("browserContextId")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }, |cx, id| ZqValue::new_string(cx, &id))
}

/// `browser.disposeContext(contextId)`
pub(crate) fn js_browser_dispose_context<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let context_id = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let client = get_browser_payload(cx, cc.this())?.client.clone();
    promise_void(cx, async move {
        client
            .send(
                "Target.disposeBrowserContext",
                Some(serde_json::json!({ "browserContextId": context_id })),
                None,
            )
            .await
            .unwrap_or_else(|_| Err("channel closed".into()))?;
        Ok(())
    })
}
