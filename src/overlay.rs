// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Page class — Overlay domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{
    class_page::{get_page_client, get_page_session, send_page_void_cmd},
    dom,
    helpers::promise_void,
};

/// Default highlight config: a translucent blue content box plus borders.
fn default_highlight_config() -> serde_json::Value {
    serde_json::json!({
        "showInfo": true,
        "contentColor": { "r": 111, "g": 168, "b": 220, "a": 0.5 },
        "paddingColor": { "r": 147, "g": 196, "b": 125, "a": 0.5 },
        "borderColor": { "r": 255, "g": 229, "b": 153, "a": 0.5 },
        "marginColor": { "r": 246, "g": 178, "b": 107, "a": 0.5 },
    })
}

/// `page.highlight(selector, opts?)`
pub(crate) fn js_page_highlight<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let config = default_highlight_config();
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    promise_void(cx, async move {
        // Best-effort highlight: mirrors the built-in module, which ignores the
        // CDP outcome of `Overlay.highlightNode` (a selector may resolve while
        // the node is not paintable).
        let _ = client.send("Overlay.enable", None, Some(&session_id)).await;
        let node_id = dom::resolve_selector(&client, &session_id, &selector).await?;
        let _ = client
            .send(
                "Overlay.highlightNode",
                Some(serde_json::json!({ "nodeId": node_id, "highlightConfig": config })),
                Some(&session_id),
            )
            .await;
        Ok(())
    })
}

/// `page.hideHighlight()`
pub(crate) fn js_page_hide_highlight<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    send_page_void_cmd(cx, &cc, "Overlay.hideHighlight", None)
}
