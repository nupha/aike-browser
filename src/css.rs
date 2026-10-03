// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — CSS domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{
    args,
    class_page::{get_page_client, get_page_session},
    dom,
};

/// `page.getComputedStyle(selector)`
pub(crate) fn js_page_get_computed_style<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = args::arg_string(cx, &cc, 0, "selector")?;
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let node_id = dom::resolve_selector(&client, &session_id, &selector).await?;
        let _ = client.send("CSS.enable", None, Some(&session_id)).await;
        let m = client
            .send(
                "CSS.getComputedStyleForNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        let entries = m
            .get("computedStyle")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut map = serde_json::Map::new();
        for e in &entries {
            if let (Some(name), Some(value)) =
                (e.get("name").and_then(|v| v.as_str()), e.get("value"))
            {
                map.insert(name.to_string(), value.clone());
            }
        }
        Ok(serde_json::Value::Object(map))
    })
}

/// `page.getMatchedStyles(selector)`
pub(crate) fn js_page_get_matched_styles<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = args::arg_string(cx, &cc, 0, "selector")?;
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let node_id = dom::resolve_selector(&client, &session_id, &selector).await?;
        let _ = client.send("CSS.enable", None, Some(&session_id)).await;
        client
            .send(
                "CSS.getMatchedStylesForNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await
    })
}
