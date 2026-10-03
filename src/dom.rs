// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — DOM domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{cdp::CdpClient, cdp_input, class_page::get_page_payload};

/// Enable the DOM domain and resolve a CSS selector to a `nodeId`.
pub(crate) async fn resolve_selector(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<i64, String> {
    let _ = client.send("DOM.enable", None, Some(session_id)).await;
    let doc = client
        .send(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": -1 })),
            Some(session_id),
        )
        .await?
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "DOM.getDocument: no root nodeId".to_string())?;
    client
        .send(
            "DOM.querySelector",
            Some(serde_json::json!({ "nodeId": doc, "selector": selector })),
            Some(session_id),
        )
        .await?
        .get("nodeId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("selector not found: {selector}"))
}

/// Shared plumbing: resolve `this` to (client, session_id).
fn page<'a>(cx: ZqCtx<'a>, this: ZqValue<'a>) -> Result<(CdpClient, String), ZqError> {
    let payload = get_page_payload(cx, this)?;
    Ok((payload.client.clone(), payload.session_id.clone()))
}

/// `page.querySelector(selector)` → nodeId (number).
pub(crate) fn js_page_query_selector<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_result(
        async move { resolve_selector(&client, &session_id, &selector).await },
        |_cx, id| Ok(ZqValue::int32(id as i32)),
    )
}

/// `page.querySelectorAll(selector)` → array of nodeIds.
pub(crate) fn js_page_query_selector_all<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_result(
        async move {
            let _ = client.send("DOM.enable", None, Some(&session_id)).await;
            let root = client
                .send(
                    "DOM.getDocument",
                    Some(serde_json::json!({ "depth": -1 })),
                    Some(&session_id),
                )
                .await?
                .get("root")
                .and_then(|r| r.get("nodeId"))
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "DOM.getDocument: no root nodeId".to_string())?;
            let q = client
                .send(
                    "DOM.querySelectorAll",
                    Some(serde_json::json!({ "nodeId": root, "selector": selector })),
                    Some(&session_id),
                )
                .await?;
            Ok(q.get("nodeIds")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect::<Vec<_>>())
                .unwrap_or_default())
        },
        |cx, ids: Vec<i64>| {
            let vals: Vec<ZqValue> = ids.iter().map(|i| ZqValue::int32(*i as i32)).collect();
            ZqValue::new_array(cx, &vals)
        },
    )
}

/// `page.getAttribute(selector, name)` → attribute value (string|null).
pub(crate) fn js_page_get_attribute<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let name = cc.arg(1).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        let a = client
            .send(
                "DOM.getAttributes",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        let attrs = a
            .get("attributes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut i = 0;
        while i + 1 < attrs.len() {
            if attrs[i].as_str() == Some(&name) {
                return Ok(attrs[i + 1].clone());
            }
            i += 2;
        }
        Ok(serde_json::Value::Null)
    })
}

/// `page.focus(selector)`
pub(crate) fn js_page_focus<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        client
            .send(
                "DOM.focus",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        Ok(())
    })
}

/// `page.getBoxModel(selector)`
pub(crate) fn js_page_get_box_model<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_json(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        let m = client
            .send(
                "DOM.getBoxModel",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        Ok(m.get("model").cloned().unwrap_or(serde_json::Value::Null))
    })
}

/// `page.remove(selector)`
pub(crate) fn js_page_remove<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        client
            .send(
                "DOM.removeNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        Ok(())
    })
}

/// Center of a node's content box via `DOM.getBoxModel` (by `nodeId`).
async fn content_center(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<(f64, f64), String> {
    let node_id = resolve_selector(client, session_id, selector).await?;
    let m = client
        .send(
            "DOM.getBoxModel",
            Some(serde_json::json!({ "nodeId": node_id })),
            Some(session_id),
        )
        .await?;
    quad_center(m.get("model"))
}

/// Center of a backend node id via `DOM.getBoxModel`.
async fn backend_center(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: u64,
) -> Result<(f64, f64), String> {
    let _ = client.send("DOM.enable", None, Some(session_id)).await;
    let m = client
        .send(
            "DOM.getBoxModel",
            Some(serde_json::json!({ "backendNodeId": backend_node_id })),
            Some(session_id),
        )
        .await?;
    quad_center(m.get("model"))
}

fn quad_center(model: Option<&serde_json::Value>) -> Result<(f64, f64), String> {
    let quad = model
        .and_then(|mo| mo.get("content"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "no box model content".to_string())?;
    let xs: Vec<f64> = quad.iter().step_by(2).filter_map(|v| v.as_f64()).collect();
    let ys: Vec<f64> = quad
        .iter()
        .skip(1)
        .step_by(2)
        .filter_map(|v| v.as_f64())
        .collect();
    if xs.len() < 2 || ys.len() < 2 {
        return Err("malformed box model".to_string());
    }
    Ok((
        (xs[0] + xs[2.min(xs.len() - 1)]) / 2.0,
        (ys[0] + ys[2.min(ys.len() - 1)]) / 2.0,
    ))
}

/// `page.click(selector)`
pub(crate) fn js_page_click<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let (cxp, cyp) = content_center(&client, &session_id, &selector).await?;
        cdp_input::mouse_click(&client, &session_id, cxp, cyp, "left", 1).await
    })
}

/// `page.type(selector, text)`
pub(crate) fn js_page_type<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let text = cc.arg(1).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        client
            .send(
                "DOM.focus",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        client
            .send(
                "Input.insertText",
                Some(serde_json::json!({ "text": text })),
                Some(&session_id),
            )
            .await?;
        Ok(())
    })
}

/// Build the aria-node JSON string from a raw `Accessibility.getFullAXTree` result.
fn build_aria_nodes(val: &serde_json::Value) -> String {
    let Some(ax_nodes) = val.get("nodes").and_then(|n| n.as_array()) else {
        return "[]".to_string();
    };
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut idx_of: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (i, n) in ax_nodes.iter().enumerate() {
        let node_id = n
            .get("nodeId")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        idx_of.insert(node_id, i);
        let role = n
            .get("role")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = n
            .get("name")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let desc = n
            .get("description")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let text = n
            .get("value")
            .and_then(|v| v.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        out.push(serde_json::json!({
            "nodeId": node_id,
            "backendNodeId": n.get("backendDOMNodeId").and_then(|v| v.as_u64()).unwrap_or(0),
            "parentIndex": -1,
            "contentType": "element",
            "textValue": text,
            "attributes": { "role": role, "name": name, "description": desc },
        }));
    }
    for (i, n) in ax_nodes.iter().enumerate() {
        let parent_idx = n
            .get("parentId")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|p| idx_of.get(&p).copied())
            .map(|x| x as i64)
            .unwrap_or(-1i64);
        out[i]["parentIndex"] = serde_json::json!(parent_idx);
    }
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

/// `page.ariaSnapshot()`
pub(crate) fn js_page_aria_snapshot<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_string(async move {
        let _ = client
            .send("Accessibility.enable", None, Some(&session_id))
            .await;
        let val = client
            .send("Accessibility.getFullAXTree", None, Some(&session_id))
            .await?;
        Ok(build_aria_nodes(&val))
    })
}

/// `page.clickBackendNode(backendNodeId)`
pub(crate) fn js_page_click_backend_node<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let backend_node_id = cc.arg_f64(cx, 0).map(|n| n as u64).unwrap_or(0);
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let (cxp, cyp) = backend_center(&client, &session_id, backend_node_id).await?;
        cdp_input::mouse_click(&client, &session_id, cxp, cyp, "left", 1).await
    })
}

/// `page.typeBackendNode(backendNodeId, text)`
pub(crate) fn js_page_type_backend_node<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let backend_node_id = cc.arg_f64(cx, 0).map(|n| n as u64).unwrap_or(0);
    let text = cc.arg(1).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_void(async move {
        let _ = client.send("DOM.enable", None, Some(&session_id)).await;
        client
            .send(
                "DOM.focus",
                Some(serde_json::json!({ "backendNodeId": backend_node_id })),
                Some(&session_id),
            )
            .await?;
        cdp_input::insert_text(&client, &session_id, &text).await
    })
}

/// `page.getNodeValue(selector)` → outerHTML (string).
pub(crate) fn js_page_get_node_value<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let (client, session_id) = page(cx, cc.this())?;
    cx.promise_fut_string(async move {
        let node_id = resolve_selector(&client, &session_id, &selector).await?;
        let m = client
            .send(
                "DOM.getOuterHTML",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(&session_id),
            )
            .await?;
        m.get("outerHTML")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "no outerHTML".to_string())
    })
}
